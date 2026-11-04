# Reader benchmark

Run from the project root. Arguments after `--` are passed to the benchmark:

```text
cargo bench --locked --bench reader -- [MiB] [rounds] [passes] [options]
```

| Argument                 | Default    | Meaning                                                                |
| ------------------------ | ---------- | ---------------------------------------------------------------------- |
| `MiB`                    | `4`        | Minimum input size in MiB per workload.                                |
| `rounds`                 | `9` / `16` | Measured rounds; two warmup rounds run first.                          |
| `passes`                 | `4`        | Complete reads of the input per round.                                 |
| `--summary=yes`          | Default    | Mix sizes into workloads.                                              |
| `--summary=no`           | —          | Measure each selected size separately; requires `--size` or `--range`. |
| `--size BYTES`           | —          | Select a payload size; may be repeated.                                |
| `--range START:END:STEP` | —          | Select payload sizes from start to end with the given step, in bytes.  |
| `--all-sizes`            | —          | Select 10 B, 20–1500 B in steps of 20 B, and 4/8/16/32/64/100 KiB.     |
| `--bytesreader`          | Off        | Include `BytesReader` and `BytesMut` methods.                          |
| `--test`                 | —          | Verify output bytes without timing.                                    |
| `--help`, `-h`           | —          | Print usage.                                                           |

Positional arguments must be positive integers and are supplied in order. A
range includes its end when the step reaches it; `--size` and `--range` may
be repeated or combined. `--all-sizes` requires `--summary=yes` and cannot be
combined with `--size` or `--range`.

The default is 9 measured rounds, or 16 with `--bytesreader`, so each method
starts a measured round once.

```sh
# Four mixed-size workloads, using 4 MiB, 9 measured rounds, and 4 passes per round.
cargo bench --locked --bench reader

# Include BytesReader and BytesMut in the comparison.
cargo bench --locked --bench reader -- --bytesreader

# Measure selected payload sizes separately.
cargo bench --locked --bench reader -- --summary=no --size 100 --size 500

# Measure 10, 20, 40, ..., 500 B separately.
cargo bench --locked --bench reader -- --summary=no --size 10 --range 20:500:20

# Faster run: 1 MiB, 3 rounds, and 1 pass per round.
cargo bench --locked --bench reader -- 1 3 1 --summary=no --size 100

# Verify all output bytes without timing.
cargo bench --locked --bench reader -- 1 --test --summary=no --size 100
```

Sizes are payload bytes; each record also contains a trailing LF.

The default `--summary=yes` mixes sizes in four separate workloads: 10 and
20–500 B, 520–1000 B, 1020–1500 B (20 B steps), and 4, 8, 16, 32, 64,
100 KiB. Specifying sizes with `--summary=yes` mixes them into one workload.

Each workload contains at least the requested MiB and 64 records.

## Reading the results

Each table identifies its payload and source: an in-memory `Cursor` or a cached
file. File results measure cached I/O, not cold disk access. The benchmark
compares `LeftoverReader` and direct `BufReader`. Add `--bytesreader` to include
`BytesReader` and `BytesMut`. All start with an 8 KiB buffer. The `BufReader`
buffer stays fixed, while `BytesReader` and `BytesMut` can grow to hold a
spanning record. The direct `BytesMut` loop initializes its spare tail before
each `Read::read` and uses `advance` or `split_to` to consume records.

- **Median, min, max ns:** nanoseconds per record, calculated from full-pass
  averages. Lower is faster.
- **MiB/s:** throughput calculated from the median. Higher is faster.
- **Output groups:** compare methods within the same group: borrowed `&[u8]`,
  new `Vec<u8>`, owned `BytesMut`, or reused `Vec<u8>`. Borrowed results may
  still require copying when a record spans reads.
- **Green median:** fastest in its output group; ties are also highlighted.
  Color is disabled when output is redirected or `NO_COLOR` is set.

Mixed workloads do not reveal the crossover point between individual sizes;
use `--summary=no` for that comparison. Reader construction, reading, LF search,
record handling, a sampled checksum, and destruction are timed. Full output is
checked before timing; input generation and file rewinding are excluded.
