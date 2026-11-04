//! Process records and other data that span buffered input chunks.
//!
//! [`LeftoverReader`] adapts a [`std::io::BufRead`] reader for data that crosses
//! buffer boundaries. It lets you save a partial record before reading the
//! next chunk, then inspect the saved bytes alongside the new chunk.
//!
//! Use [`LeftoverReader::fill_view`] to inspect the available bytes. If the
//! current chunk does not contain enough data, call [`LeftoverView::accumulate`]
//! to save it before filling the next view. Once you have enough, call
//! [`LeftoverView::take_view`] to borrow the data or [`LeftoverView::take_owned`]
//! to copy it into a vector. Use [`LeftoverView::consume`] to discard the same
//! range without copying it. These view operations do not read from the source.
//!
//! Both `take` methods return the entire leftover followed by the first `end`
//! bytes of the current chunk. For example, with leftover `ab` and current
//! chunk `cdef`, an `end` of 2 takes `abcd`. At EOF, use `end = 0` to take any
//! remaining leftover.
//!
//! ```
//! use std::io::BufReader;
//!
//! use leftover::LeftoverReader;
//!
//! let input = BufReader::with_capacity(3, b"abcdef".as_slice());
//! let mut reader = LeftoverReader::new(input);
//! let view = reader.fill_view()?;
//! assert_eq!(view.buffer(), b"abc");
//! view.accumulate();
//! let view = reader.fill_view()?;
//! assert_eq!(view.leftover(), b"abc");
//! assert_eq!(view.buffer(), b"def");
//! assert_eq!(view.take_owned(2), b"abcde");
//! assert_eq!(reader.fill_view()?.buffer(), b"f");
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! `LeftoverReader` does not implement `Read` or `BufRead`. For an operation
//! that appends through a delimiter into a reusable vector, use [`read_until`]
//! with any `BufRead` reader.

mod read_until;
mod reader;

pub use self::read_until::read_until;
pub use self::reader::{LeftoverReader, LeftoverView};
