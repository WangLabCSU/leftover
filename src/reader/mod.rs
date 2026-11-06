use std::io::{self, BufRead};

mod action;
mod staging;
mod view;

pub use self::view::LeftoverView;

use self::action::Action;
use self::staging::Staging;

/// A [`BufRead`] adapter for processing data across buffer boundaries.
///
/// A `BufRead` reader exposes one buffered chunk at a time. When a record or
/// other data spans chunks, this adapter lets you save its earlier bytes and
/// access them alongside the next chunk.
///
/// Call [`Self::fill_view`] to inspect the current chunk alongside any bytes
/// saved from earlier chunks. If you need more data, call
/// [`LeftoverView::accumulate`] before filling the next view. Once you have
/// enough, [`LeftoverView::take_view`] borrows the data and
/// [`LeftoverView::take_owned`] copies it into a vector. Use
/// [`LeftoverView::consume`] to discard the data without returning it.
///
/// This adapter does not implement `Read` or `BufRead`.
///
/// # Examples
///
/// Accumulate a chunk, then take it together with part of the next chunk.
/// The argument to `take_view` counts bytes from the current chunk only:
///
/// ```
/// use std::io::BufReader;
///
/// use leftover::LeftoverReader;
///
/// let input = BufReader::with_capacity(3, b"abcdef".as_slice());
/// let mut reader = LeftoverReader::new(input);
/// let view = reader.fill_view()?;
/// assert_eq!(view.buffer(), b"abc");
/// view.accumulate();
///
/// let view = reader.fill_view()?;
/// assert_eq!(view.leftover(), b"abc");
/// assert_eq!(view.buffer(), b"def");
/// assert_eq!(view.take_view(2), b"abcde");
/// assert_eq!(reader.fill_view()?.buffer(), b"f");
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Read newline-delimited records into separate vectors, including a final
/// record without a newline. Search each current chunk once; accumulate it
/// when no delimiter is found. `take_owned` copies each record so it can be
/// kept after reading the next one:
///
/// ```
/// use std::io::BufReader;
///
/// use leftover::LeftoverReader;
///
/// let input = BufReader::with_capacity(3, b"one\n\nlast".as_slice());
/// let mut reader = LeftoverReader::new(input);
/// let mut records = Vec::new();
///
/// loop {
///     let view = reader.fill_view()?;
///     if view.buffer().is_empty() {
///         if !view.leftover().is_empty() {
///             records.push(view.take_owned(0));
///         }
///         break;
///     }
///     if let Some(end) = view.buffer().iter().position(|&byte| byte == b'\n') {
///         records.push(view.take_owned(end + 1));
///     } else {
///         view.accumulate();
///     }
/// }
///
/// assert_eq!(records.len(), 3);
/// assert_eq!(records[0], b"one\n");
/// assert_eq!(records[1], b"\n");
/// assert_eq!(records[2], b"last");
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct LeftoverReader<R: ?Sized> {
    leftover: Vec<u8>,
    deferred: Option<Action>,
    inner: R,
}

impl<R> LeftoverReader<R> {
    /// Wrap a buffered reader without reading from it.
    pub fn new(reader: R) -> Self {
        Self {
            leftover: Vec::new(),
            deferred: None,
            inner: reader,
        }
    }
}

impl<R: BufRead + ?Sized> LeftoverReader<R> {
    // Every entry point that needs mutable access shares this dispatcher.
    // Take the action before calling user-provided consume, so a panic cannot
    // cause it to be replayed. Leave an empty slot untouched.
    #[inline]
    fn commit_deferred(deferred: &mut Option<Action>, leftover: &mut Vec<u8>, inner: &mut R) {
        match deferred {
            None => {}
            Some(_) => {
                let Some(action) = deferred.take() else {
                    unreachable!();
                };
                action.commit(leftover, inner);
            }
        }
    }

    /// Return a view of the leftover and current chunk, reading more input if
    /// needed.
    ///
    /// The leftover and current chunk are borrowed separately, without joining
    /// or copying them. Bytes already taken or consumed are excluded. At EOF,
    /// the current chunk is empty, but the leftover can still contain data.
    /// A reader with a zero-capacity buffer can also return an empty chunk
    /// before EOF.
    ///
    /// Filling another view does not advance past the current chunk by itself.
    /// Call [`LeftoverView::accumulate`] to save the chunk before reading more.
    ///
    /// # Errors
    /// Returns errors from the underlying reader, including interruptions.
    /// Data saved with [`LeftoverView::accumulate`] is kept if reading fails;
    /// retrying does not duplicate it.
    ///
    /// ```
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(b"abc".as_slice());
    /// let view = reader.fill_view()?;
    /// assert!(view.leftover().is_empty());
    /// assert_eq!(view.buffer(), b"abc");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    ///
    /// Finish using the borrowed chunks before reading again:
    ///
    /// ```compile_fail,E0499
    /// use leftover::LeftoverReader;
    /// let mut reader = LeftoverReader::new(b"abc".as_slice());
    /// let view = reader.fill_view().unwrap();
    /// reader.fill_view().unwrap();
    /// assert_eq!(view.buffer(), b"abc");
    /// ```
    #[inline]
    pub fn fill_view(&mut self) -> io::Result<LeftoverView<'_>> {
        let Self {
            leftover,
            deferred,
            inner,
        } = self;
        Self::commit_deferred(deferred, leftover, inner);
        // On error the previous action is already committed, but no new
        // view or action exists. Retrying cannot replay the old action.
        let buf = inner.fill_buf()?;
        Ok(LeftoverView::new(leftover, buf, Staging::new(deferred)))
    }
}

impl<R: ?Sized> LeftoverReader<R> {
    /// Return a reference to the wrapped reader.
    ///
    /// Bytes taken, consumed, or accumulated by the latest view may still appear in the
    /// wrapped reader until the next [`Self::fill_view`], [`Self::get_mut`],
    /// or [`Self::into_inner`] call.
    #[inline]
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Return a mutable reference to the wrapped reader.
    ///
    /// Bytes taken, consumed, or accumulated by the latest view are consumed
    /// from the wrapped reader before the reference is returned. Accumulated
    /// data is kept. Reading or seeking through this reference can change where
    /// the next view obtains its current chunk.
    #[inline]
    pub fn get_mut(&mut self) -> &mut R
    where
        R: BufRead,
    {
        // Reuse the field borrows so an unsized inner's address is computed once.
        let Self {
            leftover,
            deferred,
            inner,
        } = self;
        Self::commit_deferred(deferred, leftover, inner);
        inner
    }

    /// Unwrap the reader, discarding any leftover.
    ///
    /// The returned reader continues after bytes already taken, consumed, or accumulated.
    /// Bytes saved in the leftover are not put back into the returned reader.
    #[inline]
    pub fn into_inner(mut self) -> R
    where
        R: BufRead + Sized,
    {
        Self::commit_deferred(&mut self.deferred, &mut self.leftover, &mut self.inner);
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{self, BufRead, BufReader, Cursor, Read};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use super::LeftoverReader;

    #[test]
    fn supports_slices_and_trait_objects() {
        let mut input = b"abcdef".as_slice();
        let source: &mut dyn BufRead = &mut input;
        let mut reader = LeftoverReader::new(source);
        assert_eq!(reader.fill_view().unwrap().take_view(2), b"ab");
        assert_eq!(reader.fill_view().unwrap().buffer(), b"cdef");
        assert_eq!(reader.fill_view().unwrap().take_owned(2), b"cd");
        reader.fill_view().unwrap().accumulate();
        assert_eq!(reader.fill_view().unwrap().take_owned(0), b"ef");

        let mut reader = LeftoverReader::new(b"abcdef".as_slice());
        let erased: &mut LeftoverReader<dyn BufRead> = &mut reader;
        assert_eq!(erased.fill_view().unwrap().take_view(2), b"ab");
        assert_eq!(erased.get_mut().fill_buf().unwrap(), b"cdef");
    }

    #[test]
    fn fill_view_exposes_a_prefilled_reader_without_copying() {
        let mut inner = BufReader::with_capacity(3, b"abcdef".as_slice());
        let pointer = inner.fill_buf().unwrap().as_ptr();
        let mut reader = LeftoverReader::new(inner);
        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert_eq!(view.take_view(3).as_ptr(), pointer);
        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert_eq!(view.take_owned(3), b"def");
    }

    #[test]
    fn joins_spanning_records_at_every_buffer_boundary() {
        let data = b"a\nlonger record\n\nlast";
        for capacity in 1..=data.len() + 1 {
            let mut reader =
                LeftoverReader::new(BufReader::with_capacity(capacity, data.as_slice()));
            let mut got = Vec::new();
            loop {
                let view = reader.fill_view().unwrap();
                if view.buffer().is_empty() {
                    if !view.leftover().is_empty() {
                        got.push(view.take_view(0).to_vec());
                    }
                    break;
                }
                if let Some(end) = view
                    .buffer()
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|i| i + 1)
                {
                    got.push(view.take_owned(end));
                } else {
                    view.accumulate();
                }
            }
            assert_eq!(got.concat(), data);
            assert_eq!(
                got,
                data.split_inclusive(|&b| b == b'\n')
                    .map(<[u8]>::to_vec)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn consuming_and_refilling_through_get_mut_keeps_state_valid() {
        for owned in [false, true] {
            let mut reader =
                LeftoverReader::new(BufReader::with_capacity(3, b"abcdefghi".as_slice()));
            reader.fill_view().unwrap().accumulate();
            let view = reader.fill_view().unwrap();
            if owned {
                assert_eq!(view.take_owned(1), b"abcd");
            } else {
                assert_eq!(view.take_view(1), b"abcd");
            }
            reader.get_mut().consume(2);
            let view = reader.fill_view().unwrap();
            assert!(view.leftover().is_empty());
            if owned {
                assert_eq!(view.take_owned(1), b"g");
            } else {
                assert_eq!(view.take_view(1), b"g");
            }
            let mut inner = reader.into_inner();
            let mut rest = Vec::new();
            inner.read_to_end(&mut rest).unwrap();
            assert_eq!(rest, b"hi");
        }
    }

    #[test]
    fn failed_refills_do_not_duplicate_the_prefix() {
        struct FailsTwice(usize);
        impl Read for FailsTwice {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                if self.0 < 2 {
                    self.0 += 1;
                    Err(io::ErrorKind::Interrupted.into())
                } else {
                    Ok(0)
                }
            }
        }
        let source = b"abc"
            .as_slice()
            .chain(FailsTwice(0))
            .chain(b"def".as_slice());
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, source));
        assert_eq!(reader.fill_view().unwrap().take_view(1), b"a");
        reader.fill_view().unwrap().accumulate();
        for _ in 0..2 {
            assert_eq!(
                reader.fill_view().err().unwrap().kind(),
                io::ErrorKind::Interrupted
            );
        }
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"bc");
        assert_eq!(view.buffer(), b"def");
        assert_eq!(view.take_view(2), b"bcde");
    }

    #[test]
    fn zero_capacity_and_empty_cursor_reach_eof() {
        let mut empty = LeftoverReader::new(Cursor::new(b""));
        assert!(empty.fill_view().unwrap().buffer().is_empty());
        assert!(empty.fill_view().unwrap().take_owned(0).is_empty());
        let mut zero = LeftoverReader::new(BufReader::with_capacity(0, b"x".as_slice()));
        assert!(zero.fill_view().unwrap().buffer().is_empty());
        assert_eq!(*zero.into_inner().get_ref(), b"x");
    }

    #[test]
    fn view_operations_commit_each_action_once() {
        let mut reader = LeftoverReader::new(b"abcd".as_slice());
        assert_eq!(reader.fill_view().unwrap().take_view(2), b"ab");
        assert_eq!(*reader.get_mut(), b"cd");
        assert_eq!(*reader.get_mut(), b"cd");
        assert_eq!(reader.fill_view().unwrap().take_view(1), b"c");
        assert_eq!(reader.into_inner(), b"d");
    }

    #[test]
    fn commit_clears_the_slot_before_consume_panics() {
        struct PanicOnConsume {
            consumes: usize,
        }
        impl Read for PanicOnConsume {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }
        impl BufRead for PanicOnConsume {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Ok(b"x")
            }
            fn consume(&mut self, _: usize) {
                self.consumes += 1;
                panic!("consume failed");
            }
        }
        let mut reader = LeftoverReader::new(PanicOnConsume { consumes: 0 });
        reader.leftover.extend_from_slice(b"prefix");
        assert_eq!(reader.fill_view().unwrap().take_view(1), b"prefixx");
        assert!(catch_unwind(AssertUnwindSafe(|| {
            reader.get_mut();
        }))
        .is_err());
        assert!(reader.leftover.is_empty());
        assert!(reader.deferred.is_none());
        assert_eq!(reader.get_mut().consumes, 1);
        assert_eq!(reader.into_inner().consumes, 1);
    }

    #[test]
    fn mutable_access_commits_without_discarding_accumulated_bytes() {
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
        reader.fill_view().unwrap().accumulate();
        let pointer = reader.leftover.as_ptr();
        assert_eq!(reader.get_mut().fill_buf().unwrap(), b"def");
        assert_eq!(reader.leftover, b"abc");
        assert_eq!(reader.leftover.as_ptr(), pointer);
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"abc");
        assert_eq!(view.leftover().as_ptr(), pointer);
        assert_eq!(view.take_view(1), b"abcd");
        let mut inner = reader.into_inner();
        assert_eq!(inner.fill_buf().unwrap(), b"ef");
    }

    #[test]
    fn dropping_a_view_keeps_both_parts_available() {
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
        reader.fill_view().unwrap().accumulate();
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"abc");
        assert_eq!(view.buffer(), b"def");
        // Reading a view without completing an operation stages nothing.
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"abc");
        assert_eq!(view.buffer(), b"def");
        assert_eq!(view.take_view(1), b"abcd");
    }

    #[test]
    fn extraction_commits_accumulation() {
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
        reader.fill_view().unwrap().accumulate();
        let mut inner = reader.into_inner();
        assert_eq!(inner.fill_buf().unwrap(), b"def");
    }

    #[test]
    fn extraction_commits_only_once() {
        struct Counted<'a> {
            inner: BufReader<&'static [u8]>,
            consumes: &'a Cell<usize>,
        }
        impl Read for Counted<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.inner.read(buf)
            }
        }
        impl BufRead for Counted<'_> {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                self.inner.fill_buf()
            }
            fn consume(&mut self, amount: usize) {
                self.consumes.set(self.consumes.get() + 1);
                self.inner.consume(amount);
            }
        }
        let consumes = Cell::new(0);
        let mut reader = LeftoverReader::new(Counted {
            inner: BufReader::with_capacity(3, b"abcdef".as_slice()),
            consumes: &consumes,
        });
        reader.fill_view().unwrap().accumulate();
        assert_eq!(reader.fill_view().unwrap().take_view(1), b"abcd");
        assert_eq!(consumes.get(), 1);
        let mut inner = reader.into_inner();
        assert_eq!(consumes.get(), 2);
        assert_eq!(inner.fill_buf().unwrap(), b"ef");
        assert_eq!(consumes.get(), 2);
    }
}
