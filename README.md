# leftover

[<img alt="github" src="https://img.shields.io/badge/github-WangLabCSU/leftover-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/WangLabCSU/leftover)
[<img alt="crates.io" src="https://img.shields.io/crates/v/leftover.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/leftover)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-leftover-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/leftover)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/WangLabCSU/leftover/ci.yml?branch=main&style=for-the-badge" height="20">](https://github.com/WangLabCSU/leftover/actions?query=branch%3Amain)

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
assert_eq!(view.slice(1..5), (b"bc".as_slice(), b"de".as_slice()));
assert_eq!(view.take_view(2), b"abcde");
```

`slice()` selects a range across the leftover and current chunk, returning two
borrowed slices without copying. It accepts ranges such as `..`, `2..`, `..4`,
and `1..=4`. Use `get(index)` to retrieve one byte.

`accumulate()` copies the current chunk. `take_view()` borrows directly when
there is no leftover; otherwise it copies the selected bytes into the leftover.
Use `take_owned()` to copy the result into a new vector that can outlive the
next read. Its `end` argument, like `take_view()`'s, counts bytes in the current
chunk only.

Use `consume(end)` to discard the leftover and the first `end` bytes of the
current chunk without copying them. Use `consume(0)` to discard just the leftover.

The adapter does not implement `BufRead`. To append through a delimiter into a
reusable vector, use `read_until()` with any `BufRead` reader instead.
