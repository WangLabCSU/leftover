use std::ops::{Bound, RangeBounds};

use super::deferred::{Action, Deferred, LeftoverAction};

/// A view of the saved bytes and the current input chunk.
///
/// Created by [`LeftoverReader::fill_view`](crate::LeftoverReader::fill_view).
/// The [`leftover`](Self::leftover) precedes the [`buffer`](Self::buffer) in
/// the input; either can be empty.
/// Its methods work with these bytes without reading from the source.
#[derive(Debug)]
pub struct LeftoverView<'a> {
    leftover: &'a mut Vec<u8>,
    buf: &'a [u8],
    // Deferred::None when created; consuming the view can stage an action.
    deferred: &'a mut Deferred,
}

impl<'a> LeftoverView<'a> {
    #[inline]
    pub(super) fn new(
        leftover: &'a mut Vec<u8>,
        buf: &'a [u8],
        deferred: &'a mut Deferred,
    ) -> Self {
        Self {
            leftover,
            buf,
            deferred,
        }
    }

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

    /// Return a range of the leftover followed by the current chunk.
    ///
    /// The range is measured from the start of the leftover. The returned
    /// slices borrow from the leftover and current chunk, respectively; either
    /// may be empty. The data is not copied.
    ///
    /// Accepts a range with inclusive, exclusive, or open-ended bounds.
    ///
    /// # Panics
    /// Panics if the range starts after it ends or extends beyond the combined
    /// length of the two parts.
    ///
    /// ```
    /// use std::io::BufReader;
    ///
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
    /// reader.fill_view()?.accumulate();
    /// let view = reader.fill_view()?;
    /// assert_eq!(view.slice(1..5), (b"bc".as_slice(), b"de".as_slice()));
    /// assert_eq!(view.slice(2..=3), (b"c".as_slice(), b"d".as_slice()));
    /// assert_eq!(view.slice(..2), (b"ab".as_slice(), b"".as_slice()));
    /// assert_eq!(view.get(3), b'd');
    /// assert_eq!(view.take_view(2), b"abcde");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    #[track_caller]
    pub fn slice(&self, range: impl RangeBounds<usize>) -> (&[u8], &[u8]) {
        let split = self.leftover.len();
        let len = split + self.buf.len();
        let bounds = (range.start_bound().cloned(), range.end_bound().cloned());
        let end = match bounds.1 {
            Bound::Included(end) if end >= len => Self::slice_index_fail(0, end, len),
            // end < len, so adding one cannot overflow.
            Bound::Included(end) => end + 1,
            Bound::Excluded(end) if end > len => Self::slice_index_fail(0, end, len),
            Bound::Excluded(end) => end,
            Bound::Unbounded => len,
        };
        let start = match bounds.0 {
            Bound::Excluded(start) if start >= end => Self::slice_index_fail(start, end, len),
            // start < end, so adding one cannot overflow.
            Bound::Excluded(start) => start + 1,
            Bound::Included(start) if start > end => Self::slice_index_fail(start, end, len),
            Bound::Included(start) => start,
            Bound::Unbounded => 0,
        };
        if start >= split {
            (&[], &self.buf[start - split..end - split])
        } else if end > split {
            (&self.leftover[start..], &self.buf[..end - split])
        } else {
            (&self.leftover[start..end], &[])
        }
    }

    /// Return a range of the leftover followed by the current chunk without
    /// checking whether it is in bounds.
    ///
    /// The returned slices borrow from the leftover and current chunk,
    /// respectively. The data is not copied.
    ///
    /// # Safety
    /// `range` must be valid for a slice whose length is the combined length
    /// of the leftover and current chunk. Adjusting exclusive start or inclusive
    /// end bounds must not overflow.
    ///
    /// ```
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(b"abcdef".as_slice());
    /// let view = reader.fill_view()?;
    /// // SAFETY: 1..4 lies within the six bytes exposed by the view.
    /// assert_eq!(unsafe { view.slice_unchecked(1..4) }, (b"".as_slice(), b"bcd".as_slice()));
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    pub unsafe fn slice_unchecked(&self, range: impl RangeBounds<usize>) -> (&[u8], &[u8]) {
        let split = self.leftover.len();
        let start_bound = range.start_bound();
        let end_bound = range.end_bound();

        // Check the combined range with the standard library's optional UB
        // diagnostics. The zero-sized slice allocates no memory, and its
        // unused result can be optimized away when UB checks are disabled.
        // SAFETY: The pointer is non-null and aligned, and a slice of `()`
        // occupies no memory regardless of its length.
        let logical = unsafe {
            std::slice::from_raw_parts(
                std::ptr::NonNull::<()>::dangling().as_ptr(),
                split + self.buf.len(),
            )
        };
        // SAFETY: The caller guarantees the range is valid for this length.
        let _ = unsafe { logical.get_unchecked((start_bound.cloned(), end_bound.cloned())) };

        let start = match start_bound {
            Bound::Included(&start) => start,
            Bound::Excluded(&start) => start + 1,
            Bound::Unbounded => 0,
        };
        let end = match end_bound {
            Bound::Included(&end) => end + 1,
            Bound::Excluded(&end) => end,
            Bound::Unbounded => split + self.buf.len(),
        };
        if start >= split {
            // SAFETY: The caller guarantees the range is valid, and this
            // branch places both bounds in the current chunk.
            (&[], unsafe {
                self.buf.get_unchecked(start - split..end - split)
            })
        } else if end > split {
            // SAFETY: The caller guarantees the range is valid. This branch
            // places its start in the leftover and its end in the chunk.
            unsafe {
                (
                    self.leftover.get_unchecked(start..),
                    self.buf.get_unchecked(..end - split),
                )
            }
        } else {
            // SAFETY: The caller guarantees the range is valid, and this
            // branch places both bounds in the leftover.
            (unsafe { self.leftover.get_unchecked(start..end) }, &[])
        }
    }

    /// Return the byte at `index`, counting from the start of the leftover.
    ///
    /// # Panics
    /// Panics if `index` is outside the combined length of the leftover and
    /// current chunk.
    #[inline]
    #[track_caller]
    pub fn get(&self, index: usize) -> u8 {
        let split = self.leftover.len();
        if index < split {
            // SAFETY: This branch checks that the index is in the leftover.
            *unsafe { self.leftover.get_unchecked(index) }
        } else if index - split < self.buf.len() {
            // SAFETY: The preceding branch rules out underflow, and this
            // branch checks that the adjusted index is in the current chunk.
            *unsafe { self.buf.get_unchecked(index - split) }
        } else {
            Self::index_fail(index, split + self.buf.len())
        }
    }

    #[cold]
    #[inline(never)]
    #[track_caller]
    fn index_fail(index: usize, len: usize) -> ! {
        panic!("index out of bounds: the len is {len} but the index is {index}")
    }

    #[cold]
    #[inline(never)]
    #[track_caller]
    fn slice_index_fail(start: usize, end: usize, len: usize) -> ! {
        if start > len {
            panic!("range start index {start} out of range for slice of length {len}");
        }
        if end > len {
            panic!("range end index {end} out of range for slice of length {len}");
        }
        if start > end {
            panic!("slice index starts at {start} but ends at {end}");
        }
        // An inclusive end at len or an excluded start at end is invalid.
        panic!("range end index {end} out of range for slice of length {len}")
    }

    /// Return the byte at `index` without checking whether it is in bounds.
    ///
    /// `index` counts from the start of the leftover.
    ///
    /// # Safety
    /// `index` must be less than the combined length of the leftover and
    /// current chunk.
    ///
    /// ```
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(b"abc".as_slice());
    /// let view = reader.fill_view()?;
    /// // SAFETY: The view contains three bytes.
    /// assert_eq!(unsafe { view.get_unchecked(1) }, b'b');
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    pub unsafe fn get_unchecked(&self, index: usize) -> u8 {
        let split = self.leftover.len();
        // Check the combined index with the standard library's optional UB
        // diagnostics. The zero-sized slice allocates no memory, and its
        // unused result can be optimized away when UB checks are disabled.
        // SAFETY: The pointer is non-null and aligned, and a slice of `()`
        // occupies no memory regardless of its length.
        let logical = unsafe {
            std::slice::from_raw_parts(
                std::ptr::NonNull::<()>::dangling().as_ptr(),
                split + self.buf.len(),
            )
        };
        // SAFETY: The caller guarantees the index is valid for this length.
        let _ = unsafe { logical.get_unchecked(index) };

        if index < split {
            // SAFETY: This branch checks that the index is in the leftover.
            unsafe { *self.leftover.get_unchecked(index) }
        } else {
            // SAFETY: The caller ensures the index is within both parts
            // together, and this branch places it in the current chunk.
            unsafe { *self.buf.get_unchecked(index - split) }
        }
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
        self.deferred
            .stage(Action::new(self.buf.len(), LeftoverAction::None));
    }

    /// Discard the leftover and the first `end` bytes of the current chunk.
    ///
    /// `end` counts only bytes from [`Self::buffer`]; use zero to discard just
    /// the leftover. The discarded bytes are excluded from subsequent views.
    /// This operation does not copy the data.
    ///
    /// # Panics
    /// Panics if `end` exceeds the current chunk's length. In that case, the
    /// leftover and current chunk remain available.
    ///
    /// ```
    /// use std::io::BufReader;
    ///
    /// use leftover::LeftoverReader;
    ///
    /// let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
    /// reader.fill_view()?.accumulate();
    /// reader.fill_view()?.consume(2); // Discards "abcde".
    /// let view = reader.fill_view()?;
    /// assert!(view.leftover().is_empty());
    /// assert_eq!(view.buffer(), b"f");
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[inline]
    pub fn consume(self, end: usize) {
        assert!(
            end <= self.buf.len(),
            "end exceeds the current chunk's length"
        );
        self.leftover.clear();
        self.deferred.stage(Action::new(end, LeftoverAction::None));
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
            self.deferred.stage(Action::new(end, LeftoverAction::None));
            buf
        } else {
            self.leftover.extend_from_slice(buf);
            self.deferred.stage(Action::new(end, LeftoverAction::Clear));
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
        self.deferred.stage(Action::new(end, LeftoverAction::None));
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{self, BufRead, BufReader, Read};

    use super::super::LeftoverReader;

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
    fn slice_borrows_ranges_across_buffer_boundaries() {
        let data = b"abcdef";
        for split in 0..=data.len() {
            let capacity = if split == 0 { data.len() } else { split };
            let mut reader =
                LeftoverReader::new(BufReader::with_capacity(capacity, data.as_slice()));
            if split != 0 {
                reader.fill_view().unwrap().accumulate();
            }
            let view = reader.fill_view().unwrap();
            assert_eq!(view.leftover().len(), split);
            let total = view.leftover().len() + view.buffer().len();
            assert_eq!(
                [view.slice(..).0, view.slice(..).1].concat(),
                &data[..total]
            );
            // SAFETY: An unbounded range selects the entire view.
            assert_eq!(unsafe { view.slice_unchecked(..) }, view.slice(..));
            for start in 0..=total {
                let (leftover, buffer) = view.slice(start..);
                assert_eq!([leftover, buffer].concat(), &data[start..total]);
                // SAFETY: start is within the combined view.
                assert_eq!(unsafe { view.slice_unchecked(start..) }, (leftover, buffer));
                for end in start..=total {
                    let (leftover, buffer) = view.slice(start..end);
                    assert_eq!([leftover, buffer].concat(), &data[start..end]);
                    // SAFETY: start <= end <= the combined length of the view.
                    assert_eq!(
                        unsafe { view.slice_unchecked(start..end) },
                        (leftover, buffer)
                    );
                    if start < end {
                        let (leftover, buffer) = view.slice(start..=end - 1);
                        assert_eq!([leftover, buffer].concat(), &data[start..end]);
                        // SAFETY: start < end <= the combined length of the view.
                        assert_eq!(
                            unsafe { view.slice_unchecked(start..=end - 1) },
                            (leftover, buffer)
                        );
                    }
                    if !leftover.is_empty() {
                        assert_eq!(leftover.as_ptr(), view.leftover()[start..].as_ptr());
                    }
                    if !buffer.is_empty() {
                        assert_eq!(
                            buffer.as_ptr(),
                            view.buffer()[start.saturating_sub(split)..].as_ptr()
                        );
                    }
                }
            }
            for end in 0..=total {
                let (leftover, buffer) = view.slice(..end);
                assert_eq!([leftover, buffer].concat(), &data[..end]);
                // SAFETY: end is at most the combined length of the view.
                assert_eq!(unsafe { view.slice_unchecked(..end) }, (leftover, buffer));
                if end < total {
                    let (leftover, buffer) = view.slice(..=end);
                    assert_eq!([leftover, buffer].concat(), &data[..=end]);
                    // SAFETY: end is less than the combined length of the view.
                    assert_eq!(unsafe { view.slice_unchecked(..=end) }, (leftover, buffer));
                    assert_eq!(view.get(end), data[end]);
                    // SAFETY: end is less than the combined length of the view.
                    assert_eq!(unsafe { view.get_unchecked(end) }, data[end]);
                }
            }
            if total >= 2 {
                use std::ops::Bound::{Excluded, Included};

                let (leftover, buffer) = view.slice((Excluded(0), Included(1)));
                assert_eq!([leftover, buffer].concat(), &data[1..2]);
                // SAFETY: This range selects index 1, which is within the view.
                assert_eq!(
                    unsafe { view.slice_unchecked((Excluded(0), Included(1))) },
                    (leftover, buffer)
                );
            }
            let buffer_len = view.buffer().len();
            assert_eq!(view.take_owned(buffer_len), &data[..total]);
        }
    }

    #[test]
    fn slice_rejects_invalid_ranges_without_consuming_data() {
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
        reader.fill_view().unwrap().accumulate();
        let view = reader.fill_view().unwrap();
        for range in [
            std::ops::Range { start: 4, end: 3 },
            0..7,
            7..7,
            3..usize::MAX,
        ] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = view.slice(range);
            }));
            assert!(result.is_err());
        }
        for index in [6, usize::MAX] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = view.get(index);
            }));
            assert!(result.is_err());
        }
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = view.slice(..=usize::MAX);
        }))
        .is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = view.slice((
                std::ops::Bound::Excluded(usize::MAX),
                std::ops::Bound::Unbounded,
            ));
        }))
        .is_err());
        assert_eq!(view.take_view(3), b"abcdef");
    }

    #[test]
    fn invalid_ranges_report_the_same_errors_as_standard_bounds() {
        use std::fmt::Debug;
        use std::ops::Bound::{Excluded, Included, Unbounded};
        use std::ops::{Bound, Range, RangeBounds};
        use std::panic::{catch_unwind, AssertUnwindSafe};

        #[derive(Clone, Debug)]
        struct CustomRange(Range<usize>);

        impl RangeBounds<usize> for CustomRange {
            fn start_bound(&self) -> Bound<&usize> {
                self.0.start_bound()
            }

            fn end_bound(&self) -> Bound<&usize> {
                self.0.end_bound()
            }
        }

        fn message(f: impl FnOnce()) -> String {
            let error = catch_unwind(AssertUnwindSafe(f)).expect_err("index must be invalid");
            if let Some(message) = error.downcast_ref::<String>() {
                message.clone()
            } else {
                error.downcast_ref::<&str>().unwrap().to_string()
            }
        }

        fn compare<I>(view: &super::LeftoverView<'_>, input: &[u8], range: I)
        where
            I: Clone + Debug + RangeBounds<usize>,
        {
            let standard = message(|| {
                let bounds = (range.start_bound().cloned(), range.end_bound().cloned());
                let _ = &input[bounds];
            });
            let actual = message(|| {
                let _ = view.slice(range.clone());
            });
            assert_eq!(actual, standard, "range: {range:?}");
        }

        let input = b"abcdef".as_slice();
        for split in [0, 3, 6] {
            let capacity = if split == 0 { input.len() } else { split };
            let mut reader = LeftoverReader::new(BufReader::with_capacity(capacity, input));
            if split != 0 {
                reader.fill_view().unwrap().accumulate();
            }
            let view = reader.fill_view().unwrap();
            let (leftover, buffer) = view.slice(CustomRange(1..5));
            assert_eq!([leftover, buffer].concat(), b"bcde");
            compare(&view, input, CustomRange(1..7));

            for range in [
                Range { start: 4, end: 3 },
                Range { start: 7, end: 3 },
                0..7,
                7..7,
                7..9,
                3..usize::MAX,
            ] {
                compare(&view, input, range);
            }
            for range in [
                std::ops::RangeInclusive::new(4, 2),
                1..=6,
                7..=9,
                2..=usize::MAX,
            ] {
                compare(&view, input, range);
            }
            compare(&view, input, 7..);
            compare(&view, input, ..7);
            compare(&view, input, ..=6);
            compare(&view, input, ..=usize::MAX);
            for range in [
                (Included(7), Excluded(9)),
                (Included(7), Included(9)),
                (Excluded(usize::MAX), Unbounded),
                (Excluded(usize::MAX), Included(usize::MAX)),
                (Unbounded, Included(usize::MAX)),
                (Excluded(2), Excluded(2)),
            ] {
                compare(&view, input, range);
            }
            let mut bounds = vec![Unbounded];
            for value in [0, 1, 2, 5, 6, 7, usize::MAX] {
                bounds.extend([Included(value), Excluded(value)]);
            }
            for &start in &bounds {
                for &end in &bounds {
                    let range = (start, end);
                    match catch_unwind(|| &input[range]) {
                        Ok(expected) => {
                            let (leftover, buffer) = view.slice(range);
                            assert_eq!([leftover, buffer].concat(), expected, "range: {range:?}");
                        }
                        Err(_) => compare(&view, input, range),
                    }
                }
            }
            for index in [6, 9, usize::MAX] {
                assert_eq!(
                    message(|| {
                        let _ = input[index];
                    }),
                    message(|| {
                        let _ = view.get(index);
                    })
                );
            }
        }
    }

    #[test]
    fn consume_discards_leftover_and_selected_chunk_bytes() {
        for end in 0..=3 {
            let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
            reader.fill_view().unwrap().accumulate();
            let view = reader.fill_view().unwrap();
            assert_eq!(view.leftover(), b"abc");
            assert_eq!(view.buffer(), b"def");
            view.consume(end);

            let view = reader.fill_view().unwrap();
            assert!(view.leftover().is_empty());
            assert_eq!(view.buffer(), &b"def"[end..]);
        }

        let mut reader = LeftoverReader::new(b"abcdef".as_slice());
        reader.fill_view().unwrap().consume(2);
        assert_eq!(reader.fill_view().unwrap().buffer(), b"cdef");
        reader.fill_view().unwrap().consume(1);
        assert_eq!(reader.into_inner(), b"def".as_slice());
    }

    #[test]
    fn consume_at_eof_and_out_of_bounds() {
        let mut reader = LeftoverReader::new(BufReader::with_capacity(3, b"abcdef".as_slice()));
        reader.fill_view().unwrap().accumulate();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.fill_view().unwrap().consume(4);
        }));
        assert!(result.is_err());
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"abc");
        assert_eq!(view.buffer(), b"def");
        view.accumulate();
        let view = reader.fill_view().unwrap();
        assert_eq!(view.leftover(), b"abcdef");
        assert!(view.buffer().is_empty());
        view.consume(0);
        let view = reader.fill_view().unwrap();
        assert!(view.leftover().is_empty());
        assert!(view.buffer().is_empty());
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
}
