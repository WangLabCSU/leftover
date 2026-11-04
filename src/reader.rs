use std::io::{self, BufRead};

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
/// [`LeftoverView::take_owned`] copies it into a vector.
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
pub struct LeftoverReader<R: ?Sized> {
    leftover: Vec<u8>,
    // A view records how many bytes to consume from the last exposed chunk
    // and whether to clear the prefix. Commit this before accessing inner mutably.
    staged: Option<Action>,
    inner: R,
}

enum LeftoverAction {
    None,
    Clear,
}

struct Action {
    // Bounded by the chunk exposed to the view that recorded this action.
    consume: usize,
    leftover: LeftoverAction,
}

/// A view of the saved bytes and the current input chunk.
///
/// Created by [`LeftoverReader::fill_view`]. The [`leftover`](Self::leftover)
/// precedes the [`buffer`](Self::buffer) in the input; either can be empty.
/// Its methods work with these bytes without reading from the source.
pub struct LeftoverView<'a> {
    leftover: &'a mut Vec<u8>,
    buf: &'a [u8],
    // None when the view is created; consuming the view can stage one action.
    staged: &'a mut Option<Action>,
}

impl<'a> LeftoverView<'a> {
    /// Return the current input chunk, excluding the leftover.
    #[inline]
    pub fn buffer(&self) -> &[u8] {
        self.buf
    }

    /// Return bytes saved from earlier chunks.
    #[inline]
    pub fn leftover(&self) -> &[u8] {
        self.leftover
    }

    /// Add the current chunk to the leftover.
    ///
    /// Copies all bytes from [`Self::buffer`] to the end of the leftover,
    /// allocating more space if needed. The next view starts with the following
    /// chunk.
    ///
    /// # Panics
    /// Panics if the accumulated data would exceed the maximum vector capacity.
    ///
    /// ```
    /// use std::io::BufReader;
    ///
    /// use leftover::LeftoverReader;
    ///
    /// let input = BufReader::with_capacity(3, b"abcdef".as_slice());
    /// let mut reader = LeftoverReader::new(input);
    /// reader.fill_view()?.accumulate();
    /// let view = reader.fill_view()?;
    /// assert_eq!(view.leftover(), b"abc");
    /// assert_eq!(view.buffer(), b"def");
    /// assert_eq!(view.take_owned(2), b"abcde");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    pub fn accumulate(self) {
        self.leftover.extend_from_slice(self.buf);
        *self.staged = Some(Action {
            consume: self.buf.len(),
            leftover: LeftoverAction::None,
        });
    }

    /// Take the leftover and the first `end` bytes of the current chunk as one
    /// borrowed slice.
    ///
    /// Taken bytes are excluded from subsequent views. `end` counts only bytes
    /// from [`Self::buffer`]; use zero to take just the leftover.
    ///
    /// Borrows the current chunk directly when there is no leftover. Otherwise,
    /// copies the selected bytes into the leftover, allocating more space if needed.
    ///
    /// # Panics
    /// Panics if `end` exceeds the current chunk's length or the combined data
    /// would exceed the maximum vector capacity.
    ///
    /// ```
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(b"abcdef".as_slice());
    /// let view = reader.fill_view()?;
    /// assert_eq!(view.take_view(2), b"ab");
    /// assert_eq!(reader.fill_view()?.buffer(), b"cdef");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    pub fn take_view(self, end: usize) -> &'a [u8] {
        let buf = &self.buf[..end];
        let view = if self.leftover.is_empty() {
            *self.staged = Some(Action {
                consume: end,
                leftover: LeftoverAction::None,
            });
            buf
        } else {
            self.leftover.extend_from_slice(buf);
            *self.staged = Some(Action {
                consume: end,
                leftover: LeftoverAction::Clear,
            });
            self.leftover.as_slice()
        };
        view
    }

    /// Take the leftover and the first `end` bytes of the current chunk in a
    /// new vector.
    ///
    /// Copies the bytes into a new vector. Taken bytes are excluded from
    /// subsequent views. `end` counts only bytes from [`Self::buffer`]; use zero
    /// to take just the leftover. A nonempty result gets its own allocation.
    ///
    /// # Panics
    /// Panics if `end` exceeds the current chunk's length or the resulting
    /// vector would exceed its maximum capacity.
    ///
    /// ```
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(b"abcdef".as_slice());
    /// let owned = reader.fill_view()?.take_owned(2);
    /// assert_eq!(owned, b"ab");
    /// assert_eq!(reader.fill_view()?.buffer(), b"cdef");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    pub fn take_owned(self, end: usize) -> Vec<u8> {
        let buf = &self.buf[..end];
        let owned = if self.leftover.is_empty() {
            buf.to_vec()
        } else {
            Self::take_owned_with_leftover(self.leftover, buf)
        };
        // The owned result does not borrow leftover; any prefix was cleared
        // above, so committing this action only needs to consume the chunk.
        *self.staged = Some(Action {
            consume: end,
            leftover: LeftoverAction::None,
        });
        owned
    }

    // Keep the common case, where a record fits in one chunk, compact.
    #[cold]
    #[inline(never)]
    fn take_owned_with_leftover(leftover: &mut Vec<u8>, buf: &[u8]) -> Vec<u8> {
        let mut owned = Vec::with_capacity(leftover.len() + buf.len());
        owned.extend_from_slice(leftover);
        leftover.clear();
        owned.extend_from_slice(buf);
        owned
    }
}

impl<R> LeftoverReader<R> {
    /// Wrap a buffered reader without reading from it.
    pub fn new(reader: R) -> Self {
        Self {
            leftover: Vec::new(),
            staged: None,
            inner: reader,
        }
    }
}

impl<R: BufRead + ?Sized> LeftoverReader<R> {
    #[inline]
    fn commit(&mut self) {
        // Commit each action once, before reading. Retrying a failed read must
        // not consume the same bytes again or discard accumulated data.
        if let Some(action) = self.staged.take() {
            if let LeftoverAction::Clear = action.leftover {
                self.leftover.clear();
            }
            if action.consume > 0 {
                self.inner.consume(action.consume);
            }
        }
    }

    /// Return a view of the leftover and current chunk, reading more input if
    /// needed.
    ///
    /// The leftover and current chunk are borrowed separately, without joining
    /// or copying them. Bytes already taken are excluded. At EOF, the current
    /// chunk is empty, but the leftover can still contain data. A reader with
    /// a zero-capacity buffer can also return an empty chunk before EOF.
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
        // A new view must start with no staged action: its operations can
        // replace the slot without discarding a previous consumption.
        self.commit();
        let buf = self.inner.fill_buf()?;
        Ok(LeftoverView {
            leftover: &mut self.leftover,
            buf,
            staged: &mut self.staged,
        })
    }
}

impl<R: ?Sized> LeftoverReader<R> {
    /// Return a reference to the wrapped reader.
    ///
    /// Bytes taken or accumulated by the latest view may still appear in the
    /// wrapped reader until the next [`Self::fill_view`], [`Self::get_mut`],
    /// or [`Self::into_inner`] call.
    #[inline]
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Return a mutable reference to the wrapped reader.
    ///
    /// Reading or seeking through this reference keeps the accumulated data,
    /// which still comes before the next chunk returned by this adapter.
    #[inline]
    pub fn get_mut(&mut self) -> &mut R
    where
        R: BufRead,
    {
        self.commit();
        &mut self.inner
    }

    /// Unwrap the reader, discarding any leftover.
    ///
    /// The returned reader continues after bytes already taken or accumulated.
    /// Bytes saved in the leftover are not put back into the returned reader.
    #[inline]
    pub fn into_inner(mut self) -> R
    where
        R: BufRead + Sized,
    {
        self.commit();
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{self, BufRead, BufReader, Cursor, Read};

    use super::LeftoverReader;

    #[test]
    fn taking_or_accumulating_from_a_view_never_calls_fill_buf() {
        struct Counted<'a> {
            inner: BufReader<&'static [u8]>,
            fills: &'a Cell<usize>,
        }

        impl Read for Counted<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.inner.read(buf)
            }
        }

        impl BufRead for Counted<'_> {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                self.fills.set(self.fills.get() + 1);
                self.inner.fill_buf()
            }

            fn consume(&mut self, amount: usize) {
                self.inner.consume(amount);
            }
        }

        for owned in [false, true] {
            for prefix in [false, true] {
                let fills = Cell::new(0);
                let source = Counted {
                    inner: BufReader::with_capacity(3, b"abcdef".as_slice()),
                    fills: &fills,
                };
                let mut reader = LeftoverReader::new(source);
                if prefix {
                    let view = reader.fill_view().unwrap();
                    let calls = fills.get();
                    view.accumulate();
                    assert_eq!(fills.get(), calls);
                }
                let view = reader.fill_view().unwrap();
                assert_eq!(view.leftover(), if prefix { &b"abc"[..] } else { &[] });
                let calls = fills.get();
                let expected = if prefix { &b"abcde"[..] } else { &b"ab"[..] };
                if owned {
                    assert_eq!(view.take_owned(2), expected);
                } else {
                    assert_eq!(view.take_view(2), expected);
                }
                assert_eq!(fills.get(), calls);
                let view = reader.fill_view().unwrap();
                assert_eq!(fills.get(), calls + 1);
                assert!(view.leftover().is_empty());
                assert_eq!(view.buffer(), if prefix { b"f" } else { b"c" });
            }
        }
    }

    #[test]
    fn temporary_views_borrow_chunks_and_exclude_taken_bytes() {
        let mut input = BufReader::with_capacity(3, b"abcdef".as_slice());
        let pointer = input.fill_buf().unwrap().as_ptr();
        let mut reader = LeftoverReader::new(input);
        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert_eq!(view.buffer(), b"abc");
        assert_eq!(view.buffer().as_ptr(), pointer);

        assert_eq!(view.take_view(2), b"ab");
        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert_eq!(view.buffer(), b"c");
        assert_eq!(view.buffer().as_ptr(), pointer.wrapping_add(2));

        view.accumulate();
        let prefix_pointer = reader.leftover.as_ptr();
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"c");
        assert_eq!(view.buffer(), b"def");
        assert_eq!(view.leftover().as_ptr(), prefix_pointer);
        assert_eq!(view.take_view(1), b"cd");

        reader.fill_view().unwrap().accumulate();
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"ef");
        assert!(view.buffer().is_empty());
        assert_eq!(view.take_owned(0), b"ef");
        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert!(view.buffer().is_empty());
    }

    #[test]
    fn taking_an_owned_prefix_keeps_the_current_chunk() {
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
        reader.fill_view().unwrap().accumulate();
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"abc");
        assert_eq!(view.buffer(), b"def");
        assert_eq!(view.take_owned(0), b"abc");

        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert_eq!(view.buffer(), b"def");
        assert_eq!(view.take_owned(1), b"d");
        assert_eq!(reader.fill_view().unwrap().buffer(), b"ef");
    }

    #[test]
    fn empty_chunks_can_preserve_or_take_the_prefix() {
        for owned in [false, true] {
            let mut reader = LeftoverReader::new(b"abc".as_slice());
            reader.fill_view().unwrap().accumulate();
            let view = reader.fill_view().unwrap();
            assert!(view.buffer().is_empty());
            view.accumulate();

            let view = reader.fill_view().unwrap();
            assert_eq!(view.leftover(), b"abc");
            assert!(view.buffer().is_empty());
            if owned {
                assert_eq!(view.take_owned(0), b"abc");
            } else {
                assert_eq!(view.take_view(0), b"abc");
            }
            let view = reader.fill_view().unwrap();
            assert!(view.leftover().is_empty());
            assert!(view.buffer().is_empty());
        }
    }

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
        struct Fails;
        impl Read for Fails {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::Interrupted.into())
            }
        }
        let source = b"abc".as_slice().chain(Fails);
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, source));
        assert_eq!(reader.fill_view().unwrap().take_view(1), b"a");
        reader.fill_view().unwrap().accumulate();
        for _ in 0..2 {
            assert_eq!(
                reader.fill_view().err().unwrap().kind(),
                io::ErrorKind::Interrupted
            );
            assert_eq!(reader.leftover, b"bc");
        }
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
}
