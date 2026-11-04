# leftover

`leftover` provides a `BufRead` adapter for processing data, such as records,
that spans buffer boundaries. It lets you save a partial record before reading
the next chunk and then work with both parts together.

`LeftoverReader` gives you a view of the current chunk and any bytes saved from
earlier chunks. When a record spans chunks, `accumulate()` saves the current
chunk so the next view can include it:

```rust
use std::io::BufReader;

use leftover::LeftoverReader;

let input = BufReader::with_capacity(3, b"abcdef".as_slice());
let mut reader = LeftoverReader::new(input);
reader.fill_view().unwrap().accumulate();

let view = reader.fill_view().unwrap();
assert_eq!(view.leftover(), b"abc");
assert_eq!(view.buffer(), b"def");
assert_eq!(view.take_view(2), b"abcde");
```

`accumulate()` copies the current chunk. `take_view()` borrows directly when
there is no leftover; otherwise it copies the selected bytes into the leftover.
Use `take_owned()` to copy the result into a new vector that can outlive the
next read. Its `end` argument, like `take_view()`'s, counts bytes in the current
chunk only.

The adapter does not implement `BufRead`. To append through a delimiter into a
reusable vector, use `read_until()` with any `BufRead` reader instead.
