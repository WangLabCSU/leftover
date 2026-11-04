use std::fs::{self, File};
use std::hint::black_box;
use std::io::{self, BufRead, BufReader, Cursor, IsTerminal, Read, Seek, Write};
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::{Buf, BytesMut};
use bytesreader::BytesReader;
use leftover::{read_until, LeftoverReader};
use memchr::memchr;

// Workloads and presentation follow bytesreader's reading benchmark:
// https://github.com/WangLabCSU/bytesreader/blob/main/benches/reading.rs
const CAPACITY: usize = 8 * 1024;
const BYTES_METHODS: std::ops::RangeInclusive<usize> = 9..=15;
const METHODS: [(&str, &str); 16] = [
    ("LeftoverReader<BufReader<R>>", "view.take_view -> &[u8]"),
    (
        "LeftoverReader<BufReader<R>>",
        "view.take_owned -> new Vec<u8>",
    ),
    ("BufReader<R>", "leftover::read_until / reuse"),
    ("BufReader<R>", "leftover::read_until / new Vec"),
    ("BufReader<R>", "std::read_until / reuse"),
    ("BufReader<R>", "std::read_until / new Vec"),
    ("BufReader<R>", "fill_buf + slice + consume"),
    ("BufReader<R>", "fill_buf + to_vec + consume"),
    ("BufReader<R>", "fill_buf + mem::take + consume"),
    ("BytesReader<R>", "take_view -> &[u8]"),
    ("BytesReader<R>", "take_view + to_vec -> Vec<u8>"),
    ("BytesReader<R>", "take_bytes -> BytesMut"),
    ("BufReader<R>", "fill_buf + BytesMut::from + consume"),
    ("BytesMut", "slice + advance -> &[u8]"),
    ("BytesMut", "to_vec + advance -> Vec<u8>"),
    ("BytesMut", "split_to -> BytesMut"),
];

const GROUPS: [(&str, &[usize]); 4] = [
    ("Borrowed &[u8]", &[0, 6, 9, 13]),
    ("New Vec<u8> per record", &[1, 7, 8, 3, 5, 10, 14]),
    ("Owned BytesMut per record", &[11, 12, 15]),
    ("Reused Vec<u8>", &[2, 4]),
];

fn incomplete() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "record has no final LF")
}

// The direct bytes control follows bytesreader's stable-Rust benchmark: keep
// partial records in BytesMut, search each byte once, and initialize the spare
// tail before Read::read. split_to transfers the completed BytesMut instead of
// borrowing or copying it.
fn bytes_mut<R: Read, const MODE: usize>(
    mut input: R,
    capacity: usize,
    mut observe: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut buffer = BytesMut::with_capacity(capacity);
    let mut scanned = 0;
    loop {
        if let Some(end) = memchr(b'\n', &buffer[scanned..]).map(|i| scanned + i + 1) {
            match MODE {
                0 => {
                    observe(&buffer[..end]);
                    buffer.advance(end);
                }
                1 => {
                    observe(&buffer[..end].to_vec());
                    buffer.advance(end);
                }
                2 => observe(&buffer.split_to(end)),
                _ => unreachable!(),
            }
            scanned = 0;
        } else {
            scanned = buffer.len();
            buffer.reserve(capacity / 2);
            buffer.resize(buffer.capacity(), 0);
            let count = input.read(&mut buffer[scanned..])?;
            if count > buffer.len() - scanned {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reader returned more bytes than requested",
                ));
            }
            buffer.truncate(scanned + count);
            if count == 0 {
                return if buffer.is_empty() {
                    Ok(())
                } else {
                    Err(incomplete())
                };
            }
        }
    }
}

// Keep a scan offset because BytesReader retains the entire partial record.
fn bytes_reader<R: Read, const MODE: usize>(
    input: R,
    capacity: usize,
    mut observe: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut reader = BytesReader::with_capacity(capacity, input);
    let mut scanned = 0;
    loop {
        let buf = reader.buffer();
        if let Some(end) = memchr(b'\n', &buf[scanned..]).map(|i| scanned + i + 1) {
            match MODE {
                0 => observe(reader.take_view(..end, None)),
                1 => observe(&reader.take_view(..end, None).to_vec()),
                2 => observe(&reader.take_bytes(..end, None)),
                _ => unreachable!(),
            }
            scanned = 0;
        } else {
            scanned = buf.len();
            if reader.read_more()? == 0 {
                return if reader.buffer().is_empty() {
                    Ok(())
                } else {
                    Err(incomplete())
                };
            }
        }
    }
}

fn generic<R: Read, const OWNED: bool>(
    input: R,
    capacity: usize,
    mut observe: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut reader = LeftoverReader::new(BufReader::with_capacity(capacity, input));
    loop {
        let view = reader.fill_view()?;
        let buf = view.buffer();
        if buf.is_empty() {
            return if view.leftover().is_empty() {
                Ok(())
            } else {
                Err(incomplete())
            };
        }
        if let Some(end) = memchr(b'\n', buf).map(|i| i + 1) {
            if OWNED {
                observe(&view.take_owned(end));
            } else {
                observe(view.take_view(end));
            }
        } else {
            view.accumulate();
        }
    }
}

fn until<R: Read, const STANDARD: bool, const REUSE: bool>(
    input: R,
    capacity: usize,
    mut observe: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut reader = BufReader::with_capacity(capacity, input);
    let mut output = Vec::new();
    loop {
        if REUSE {
            output.clear();
        } else {
            output = Vec::new();
        }
        let count = if STANDARD {
            reader.read_until(b'\n', &mut output)?
        } else {
            read_until(&mut reader, b'\n', &mut output)?
        };
        if count == 0 {
            return Ok(());
        }
        if output.last() != Some(&b'\n') {
            return Err(incomplete());
        }
        observe(&output);
    }
}

fn run<R: Read>(
    method: usize,
    input: R,
    capacity: usize,
    observe: impl FnMut(&[u8]),
) -> io::Result<()> {
    match method {
        0 => generic::<_, false>(input, capacity, observe),
        1 => generic::<_, true>(input, capacity, observe),
        2 => until::<_, false, true>(input, capacity, observe),
        3 => until::<_, false, false>(input, capacity, observe),
        4 => until::<_, true, true>(input, capacity, observe),
        5 => until::<_, true, false>(input, capacity, observe),
        6 => standard::<_, 0>(input, capacity, observe),
        7 => standard::<_, 1>(input, capacity, observe),
        8 => standard::<_, 2>(input, capacity, observe),
        9 => bytes_reader::<_, 0>(input, capacity, observe),
        10 => bytes_reader::<_, 1>(input, capacity, observe),
        11 => bytes_reader::<_, 2>(input, capacity, observe),
        12 => standard::<_, 3>(input, capacity, observe),
        13 => bytes_mut::<_, 0>(input, capacity, observe),
        14 => bytes_mut::<_, 1>(input, capacity, observe),
        15 => bytes_mut::<_, 2>(input, capacity, observe),
        _ => unreachable!(),
    }
}

// Direct BufReader controls use the same LF search as the wrappers. Only
// incomplete records need scratch storage. MODE 1 copies the assembled record
// and reuses scratch; MODE 2 transfers scratch with mem::take instead. MODE 3
// copies to BytesMut, reusing scratch for spanning records.
fn standard<R: Read, const MODE: usize>(
    input: R,
    capacity: usize,
    mut observe: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut reader = BufReader::with_capacity(capacity, input);
    let mut partial = Vec::new();
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return if partial.is_empty() {
                Ok(())
            } else {
                Err(incomplete())
            };
        }
        if let Some(end) = memchr(b'\n', buf).map(|i| i + 1) {
            if partial.is_empty() {
                if MODE == 0 {
                    observe(&buf[..end]);
                } else if MODE == 3 {
                    observe(&BytesMut::from(&buf[..end]));
                } else {
                    observe(&buf[..end].to_vec());
                }
            } else {
                partial.extend_from_slice(&buf[..end]);
                match MODE {
                    0 => observe(&partial),
                    1 => observe(&partial.to_vec()),
                    2 => observe(&std::mem::take(&mut partial)),
                    3 => observe(&BytesMut::from(partial.as_slice())),
                    _ => unreachable!(),
                }
                partial.clear();
            }
            reader.consume(end);
        } else {
            partial.extend_from_slice(buf);
            let used = buf.len();
            reader.consume(used);
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Summary {
    records: usize,
    bytes: usize,
    checksum: u64,
}

impl Summary {
    fn observe(&mut self, record: &[u8]) {
        let record = black_box(record);
        self.records += 1;
        self.bytes += record.len();
        self.checksum = self.checksum.wrapping_add(
            u64::from(record[0])
                + (u64::from(record[record.len() / 2]) << 8)
                + (u64::from(record[record.len() - 1]) << 16),
        );
    }
}

fn elapsed<R: Read>(
    method: usize,
    input: R,
    capacity: usize,
    expected: &Summary,
) -> io::Result<Duration> {
    let mut got = Summary::default();
    let start = Instant::now();
    run(method, input, capacity, |record| got.observe(record))?;
    let time = start.elapsed();
    assert_eq!(&got, expected);
    black_box(got);
    Ok(time)
}

struct InputFile {
    file: File,
    path: PathBuf,
}

impl Drop for InputFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct Options {
    mib: usize,
    rounds: usize,
    passes: usize,
    summary: bool,
    all_sizes: bool,
    sizes: Vec<usize>,
    test: bool,
    bytesreader: bool,
}

fn options() -> io::Result<Option<Options>> {
    let mut options = Options {
        mib: 4,
        rounds: 0,
        passes: 4,
        summary: true,
        all_sizes: false,
        sizes: Vec::new(),
        test: false,
        bytesreader: false,
    };
    let mut position = 0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => (),
            "--summary=yes" => options.summary = true,
            "--summary=no" => options.summary = false,
            "--all-sizes" => options.all_sizes = true,
            "--bytesreader" => options.bytesreader = true,
            "--size" => {
                let value = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--size requires a payload size in bytes",
                    )
                })?;
                options.sizes.push(value.parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid payload size {value:?}"),
                    )
                })?);
            }
            "--range" => {
                let value = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--range requires START:END:STEP in bytes",
                    )
                })?;
                let parts: Vec<_> = value.split(':').collect();
                let invalid = || {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("invalid range {value:?}; expected START:END:STEP with START <= END and STEP > 0"))
                };
                if parts.len() != 3 {
                    return Err(invalid());
                }
                let start: usize = parts[0].parse().map_err(|_| invalid())?;
                let end: usize = parts[1].parse().map_err(|_| invalid())?;
                let step: usize = parts[2].parse().map_err(|_| invalid())?;
                if start > end || step == 0 {
                    return Err(invalid());
                }
                options.sizes.extend((start..=end).step_by(step));
            }
            "--test" => options.test = true,
            "--help" | "-h" => {
                println!(
                    "reader [MiB=4] [rounds=9 (16 with --bytesreader)] [passes=4] [--summary=yes|no (default: yes)] [--all-sizes | --size BYTES | --range START:END:STEP ...] [--bytesreader] [--test]"
                );
                return Ok(None);
            }
            _ => {
                let value = arg.parse::<usize>().ok().filter(|&n| n > 0);
                let Some(value) = value.filter(|_| position < 3) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid argument {arg:?}; use --help for usage"),
                    ));
                };
                match position {
                    0 => options.mib = value,
                    1 => options.rounds = value,
                    2 => options.passes = value,
                    _ => unreachable!(),
                }
                position += 1;
            }
        }
    }
    if options.all_sizes && !options.sizes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--all-sizes cannot be combined with --size or --range",
        ));
    }
    if !options.summary && options.sizes.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "--summary=no requires --size BYTES or --range START:END:STEP; use --summary=yes for mixed workloads"));
    }
    if options.rounds == 0 {
        options.rounds = if options.bytesreader {
            METHODS.len()
        } else {
            METHODS.len() - BYTES_METHODS.count()
        };
    }
    Ok(Some(options))
}

fn main() -> io::Result<()> {
    let Some(options) = options()? else {
        return Ok(());
    };
    let methods: Vec<usize> = (0..METHODS.len())
        .filter(|method| options.bytesreader || !BYTES_METHODS.contains(method))
        .collect();
    let target = options
        .mib
        .checked_mul(1 << 20)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "input size exceeds usize"))?;
    let oversized = vec![4096, 8192, 16384, 32768, 65536, 102400];
    let workloads: Vec<(String, Vec<usize>)> = if !options.sizes.is_empty() {
        if options.summary {
            vec![(
                format!("Mixed payloads {:?} B", options.sizes),
                options.sizes,
            )]
        } else {
            options
                .sizes
                .into_iter()
                .map(|size| (format!("Payload {size} B"), vec![size]))
                .collect()
        }
    } else {
        vec![
            (
                "Mixed payloads 10 B and 20..=500 B / step 20".into(),
                std::iter::once(10).chain((20..=500).step_by(20)).collect(),
            ),
            (
                "Mixed payloads 520..=1000 B / step 20".into(),
                (520..=1000).step_by(20).collect(),
            ),
            (
                "Mixed payloads 1020..=1500 B / step 20".into(),
                (1020..=1500).step_by(20).collect(),
            ),
            ("Mixed payloads 4, 8, 16, 32, 64, 100 KiB".into(), oversized),
        ]
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("leftover-bench-{}-{stamp}", std::process::id()));
    let mut input = InputFile {
        file: File::create_new(&path)?,
        path,
    };
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let width = terminal_width();
    if options.bytesreader {
        println!("BufReader buffers have 8 KiB (8192 B); BytesReader and BytesMut start at 8 KiB and can grow.");
    } else {
        println!("BufReader buffers have 8 KiB (8192 B).");
    }
    println!("Payload sizes exclude the final LF byte. File input uses the page cache.");
    println!(
        "Methods: {} | measured rounds: {} | passes/sample: {} | warmup rounds: 2",
        methods.len(),
        options.rounds,
        options.passes
    );
    if options.test {
        println!("Validation only: compare every record byte, without timing.");
    } else {
        println!("Times are ns/record; lower is faster. Throughput is MiB/s; higher is faster.");
        println!("Green marks the lowest median in each output group (terminal only).\n");
    }
    for (label, sizes) in workloads {
        let mut data = Vec::with_capacity(target);
        let mut records = 0;
        while data.len() < target || records < 64 {
            let len = sizes[records % sizes.len()];
            // Payload bytes never contain LF. Vary bytes by record and position
            // so validation catches skipped, duplicated and reordered data.
            data.extend((0..len).map(|i| ((records * 13 + i * 7) % 245 + 11) as u8));
            data.push(b'\n');
            records += 1;
        }
        input.file.rewind()?;
        input.file.set_len(0)?;
        input.file.write_all(&data)?;
        let mut expected = Summary::default();
        for record in data.split_inclusive(|&b| b == b'\n') {
            expected.observe(record);
        }
        for source in ["memory Cursor<&[u8]>", "cached file"] {
            println!("\n{label} + LF | {source} | initial buffer 8 KiB");
            io::stdout().flush()?;
            // Check the complete output separately for each source before timing.
            for &method in &methods {
                let mut reference = data.split_inclusive(|&b| b == b'\n');
                let observe = |item: &[u8]| assert_eq!(Some(item), reference.next());
                if source.starts_with("memory") {
                    run(method, Cursor::new(data.as_slice()), CAPACITY, observe)?;
                } else {
                    input.file.rewind()?;
                    run(method, &mut input.file, CAPACITY, observe)?;
                }
                assert!(reference.next().is_none());
            }
            if options.test {
                println!(
                    "PASS | {} methods | {} records | {} bytes",
                    methods.len(),
                    expected.records,
                    expected.bytes
                );
                continue;
            }
            let mut samples: [Vec<f64>; METHODS.len()] = std::array::from_fn(|_| Vec::new());
            for round in 0..options.rounds + 2 {
                for slot in 0..methods.len() {
                    // Rotate measured rounds from zero, separately from warmup,
                    // so the default number of rounds balances every starting position.
                    let method = methods[(slot + round.saturating_sub(2)) % methods.len()];
                    let mut time = Duration::ZERO;
                    for _ in 0..options.passes {
                        time += if source.starts_with("memory") {
                            elapsed(
                                method,
                                Cursor::new(black_box(data.as_slice())),
                                CAPACITY,
                                &expected,
                            )?
                        } else {
                            input.file.rewind()?;
                            elapsed(method, &mut input.file, CAPACITY, &expected)?
                        };
                    }
                    if round >= 2 {
                        samples[method].push(
                            time.as_secs_f64() * 1e9
                                / options.passes as f64
                                / expected.records as f64,
                        );
                    }
                }
            }
            println!("{} records | {} bytes\n", expected.records, expected.bytes);
            print_table(&mut samples, &methods, &expected, width, color);
        }
    }
    Ok(())
}

fn terminal_width() -> usize {
    if let Some(width) = std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()) {
        return width;
    }
    #[cfg(unix)]
    if io::stdout().is_terminal() {
        if let Ok(tty) = File::open("/dev/tty") {
            if let Ok(output) = Command::new("stty")
                .arg("size")
                .stdin(tty)
                .stderr(Stdio::null())
                .output()
            {
                if let Some(width) = String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                {
                    return width;
                }
            }
        }
    }
    80
}

// All labels and numbers are ASCII. Split long type names without truncating
// them; numeric columns stay on the first line of each method's row.
fn wrap(text: &str, width: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while rest.len() > width {
        let end = rest[..width].rfind(' ').filter(|&i| i > 0).unwrap_or(width);
        lines.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    lines.push(rest);
    lines
}

fn border(widths: &[usize; 5], left: char, middle: char, right: char) {
    let cells: Vec<_> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
    println!("{left}{}{right}", cells.join(&middle.to_string()));
}

fn print_table(
    samples: &mut [Vec<f64>; METHODS.len()],
    methods: &[usize],
    expected: &Summary,
    width: usize,
    color: bool,
) {
    for values in samples.iter_mut() {
        values.sort_unstable_by(f64::total_cmp);
    }
    let medians: [f64; METHODS.len()] = std::array::from_fn(|method| {
        let values = &samples[method];
        if values.is_empty() {
            f64::INFINITY
        } else {
            (values[(values.len() - 1) / 2] + values[values.len() / 2]) / 2.0
        }
    });
    let rows: [[String; 5]; METHODS.len()] = std::array::from_fn(|method| {
        if samples[method].is_empty() {
            return std::array::from_fn(|_| String::new());
        }
        let median = medians[method];
        let throughput =
            expected.bytes as f64 / expected.records as f64 * 1e9 / median / (1 << 20) as f64;
        [
            format!("{} / {}", METHODS[method].0, METHODS[method].1),
            format!("{median:.2}"),
            format!("{:.2}", samples[method][0]),
            format!("{:.2}", samples[method].last().unwrap()),
            format!("{throughput:.2}"),
        ]
    });
    let headers = [
        "Reader / operation",
        "Median ns",
        "Min ns",
        "Max ns",
        "MiB/s",
    ];
    let mut widths: [usize; 5] = std::array::from_fn(|i| {
        methods
            .iter()
            .map(|&method| rows[method][i].len())
            .max()
            .unwrap()
            .max(headers[i].len())
    });
    let fixed = widths[1..].iter().sum::<usize>() + 16;
    widths[0] = widths[0].min(width.saturating_sub(fixed).max(headers[0].len()));
    border(&widths, '┌', '┬', '┐');
    print!("│ {:<width$} │", headers[0], width = widths[0]);
    for i in 1..5 {
        print!(" {:>width$} │", headers[i], width = widths[i]);
    }
    println!();
    let inner_width = widths.iter().sum::<usize>() + 14;
    for (group, group_methods) in GROUPS {
        if !group_methods.iter().any(|method| methods.contains(method)) {
            continue;
        }
        border(&widths, '├', '┴', '┤');
        println!(
            "│ {:<width$} │",
            format!("Output: {group}"),
            width = inner_width - 2
        );
        border(&widths, '├', '┬', '┤');
        let best = group_methods
            .iter()
            .filter(|&&i| methods.contains(&i))
            .map(|&i| medians[i])
            .min_by(f64::total_cmp)
            .unwrap();
        for &method in group_methods.iter().filter(|&&i| methods.contains(&i)) {
            for (line, label) in wrap(&rows[method][0], widths[0]).iter().enumerate() {
                print!("│ {label:<width$} │", width = widths[0]);
                for i in 1..5 {
                    let value = if line == 0 {
                        rows[method][i].as_str()
                    } else {
                        ""
                    };
                    let (green, reset) = if color && line == 0 && i == 1 && medians[method] == best
                    {
                        ("\x1b[32m", "\x1b[0m")
                    } else {
                        ("", "")
                    };
                    print!(" {green}{value:>width$}{reset} │", width = widths[i]);
                }
                println!();
            }
        }
    }
    border(&widths, '└', '┴', '┘');
}
