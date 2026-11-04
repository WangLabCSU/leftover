use std::io::{self, BufRead};

use memchr::memchr;

/// Read from `reader` through `delim` or EOF, appending the bytes to `buf`.
///
/// The delimiter is included when found. Returns the number of bytes appended,
/// or zero if the reader supplies no bytes.
///
/// This scans for the delimiter and copies the bytes read into `buf`, growing
/// the vector if needed. Existing contents are kept; call [`Vec::clear`]
/// between reads if each item should start in an empty buffer.
///
/// # Errors
/// Retries [`io::ErrorKind::Interrupted`]. On other input errors, bytes already
/// appended remain in `buf` and have been consumed from `reader`.
///
/// ```
/// use leftover::read_until;
///
/// let mut reader = b"first\nsecond".as_slice();
/// let mut output = Vec::new();
/// assert_eq!(read_until(&mut reader, b'\n', &mut output)?, 6);
/// assert_eq!(output, b"first\n");
/// output.clear();
/// assert_eq!(read_until(&mut reader, b'\n', &mut output)?, 6);
/// assert_eq!(output, b"second");
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn read_until<R: BufRead + ?Sized>(
    reader: &mut R,
    delim: u8,
    buf: &mut Vec<u8>,
) -> io::Result<usize> {
    let mut read = 0;
    loop {
        let (done, used) = {
            let available = match reader.fill_buf() {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            match memchr(delim, available) {
                Some(i) => {
                    buf.extend_from_slice(&available[..=i]);
                    (true, i + 1)
                }
                None => {
                    buf.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };
        // Commit each appended chunk immediately. An input error on a later
        // iteration leaves this prefix in buf without exposing it again.
        reader.consume(used);
        read += used;
        if done || used == 0 {
            return Ok(read);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufRead, BufReader, Cursor, Read};

    use super::read_until;

    #[test]
    fn read_until_accepts_slices_and_trait_objects_and_reuses_output() {
        let mut input = b"first\nsecond\0tail".as_slice();
        let mut output = Vec::with_capacity(32);
        output.extend_from_slice(b"prefix:");
        let pointer = output.as_ptr();
        let capacity = output.capacity();
        assert_eq!(read_until(&mut input, b'\n', &mut output).unwrap(), 6);
        assert_eq!(output, b"prefix:first\n");
        output.clear();
        let reader: &mut dyn BufRead = &mut input;
        assert_eq!(read_until(reader, 0, &mut output).unwrap(), 7);
        assert_eq!(output, b"second\0");
        output.clear();
        assert_eq!(read_until(reader, 0, &mut output).unwrap(), 4);
        assert_eq!(output, b"tail");
        assert_eq!(read_until(reader, 0, &mut output).unwrap(), 0);
        assert_eq!(output, b"tail");
        assert_eq!(output.as_ptr(), pointer);
        assert_eq!(output.capacity(), capacity);
    }

    #[test]
    fn read_until_matches_standard_reader_across_buffer_boundaries() {
        let input = b"\nabc\r\n\0def\nlast";
        for delimiter in [b'\n', 0, b'x'] {
            for capacity in 1..=input.len() + 1 {
                let mut reader = BufReader::with_capacity(capacity, input.as_slice());
                let mut reference = Cursor::new(input);
                let mut output = Vec::new();
                let mut expected = Vec::new();
                loop {
                    output.clear();
                    expected.clear();
                    let got = read_until(&mut reader, delimiter, &mut output).unwrap();
                    let count = reference.read_until(delimiter, &mut expected).unwrap();
                    assert_eq!(got, count);
                    assert_eq!(output, expected);
                    if got == 0 {
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn read_until_handles_interruptions_and_preserves_partial_output_on_error() {
        struct ErrorOnce {
            input: &'static [u8],
            calls: usize,
            error: io::ErrorKind,
        }

        impl Read for ErrorOnce {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls == 2 {
                    return Err(self.error.into());
                }
                self.input.read(output)
            }
        }

        for error in [io::ErrorKind::Interrupted, io::ErrorKind::WouldBlock] {
            let source = ErrorOnce {
                input: b"abc\n",
                calls: 0,
                error,
            };
            let mut reader = BufReader::with_capacity(2, source);
            let mut output = Vec::new();
            let result = read_until(&mut reader, b'\n', &mut output);
            if error == io::ErrorKind::Interrupted {
                assert_eq!(result.unwrap(), 4);
            } else {
                assert_eq!(result.unwrap_err().kind(), error);
                assert_eq!(output, b"ab");
                assert_eq!(read_until(&mut reader, b'\n', &mut output).unwrap(), 2);
            }
            assert_eq!(output, b"abc\n");
        }
    }
}
