//! Deterministic low-repetition Unicode workloads shared by old/new zterm builds.
//! This measures parser/screen processing, not PTY I/O or frame presentation.
//! No dependency beyond std in this development-only example.
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeSet;
use std::fmt::Write;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;
use zterm::{Cell, Terminal};

struct Counted;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            ALLOCATIONS.fetch_add(1, Relaxed);
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, old, size) };
        if !result.is_null() {
            ALLOCATIONS.fetch_add(1, Relaxed);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: Counted = Counted;

const SEED: u64 = 0x4d59_5df4_d0f3_3173;
const COLS: usize = 120;
const ROWS: usize = 32;
const HISTORY: usize = 2048;
const WARMUP: usize = 4;
const FRAGMENTS: [usize; 11] = [1, 2, 5, 3, 7, 11, 4, 13, 17, 23, 31];

fn next(state: &mut u64) -> u64 {
    // Fully specified integer arithmetic: no OS entropy or language RNG version.
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state ^ (*state >> 29)
}

fn scalar(state: &mut u64) -> char {
    let number = next(state);
    let code = match number % 8 {
        0..=2 => 0x4e00 + ((number >> 8) % (0x9fff - 0x4e00 + 1)) as u32,
        3..=5 => 0xac00 + ((number >> 8) % (0xd7a3 - 0xac00 + 1)) as u32,
        6 => 0x0410 + ((number >> 8) % 64) as u32,
        _ => 0x03b1 + ((number >> 8) % 25) as u32,
    };
    char::from_u32(code).unwrap()
}

fn unicode_log(units: usize) -> Vec<u8> {
    let mut state = SEED;
    let mut text = String::with_capacity(units * 4);
    text.push_str("\x1b[0m");
    for index in 0..units {
        if index % 40 == 0 {
            write!(text, "{:06} INFO ", index / 40).unwrap();
        }
        text.push(scalar(&mut state));
        if index % 40 == 39 {
            text.push_str("\r\n");
        }
    }
    if !units.is_multiple_of(40) {
        text.push_str("\r\n");
    }
    text.into_bytes()
}

fn cluster_log(units: usize) -> Vec<u8> {
    // Intact, deterministic clusters; byte fragmentation is applied only during
    // feed(), so ZWJ/RI/variation selectors and combining marks remain semantic
    // test cases instead of being dropped or flattened for throughput.
    const EMOJI: [&str; 16] = [
        "👩🏽‍💻",
        "👨🏿‍🔬",
        "👩‍🚀",
        "👨‍👩‍👧‍👦",
        "🏳️‍🌈",
        "❤️",
        "✈️",
        "☕️",
        "🇨🇳",
        "🇯🇵",
        "🇬🇷",
        "🇺🇳",
        "1️⃣",
        "9️⃣",
        "👩🏻‍🏫",
        "🧑🏾‍🚒",
    ];
    const MARKS: [char; 6] = [
        '\u{0301}', '\u{0300}', '\u{0308}', '\u{0302}', '\u{0303}', '\u{0327}',
    ];
    let mut state = SEED ^ 0x9e37_79b9_7f4a_7c15;
    let mut text = String::with_capacity(units * 16);
    text.push_str("\x1b[0m");
    for index in 0..units {
        if index % 16 == 0 {
            write!(text, "{:06} TEXT ", index / 16).unwrap();
        }
        // A changing CJK/Hangul neighbor creates many different boundary pairs
        // around a small, realistic family of emoji/combining sequences.
        text.push(scalar(&mut state));
        let number = next(&mut state);
        if number & 1 == 0 {
            text.push_str(EMOJI[((number >> 8) as usize) % EMOJI.len()]);
        } else {
            text.push(char::from_u32(0x0100 + ((number >> 8) % 128) as u32).unwrap());
            text.push(MARKS[((number >> 24) as usize) % MARKS.len()]);
            text.push(MARKS[((number >> 32) as usize) % MARKS.len()]);
        }
        text.push(' ');
        if index % 16 == 15 {
            text.push_str("\r\n");
        }
    }
    if !units.is_multiple_of(16) {
        text.push_str("\r\n");
    }
    text.into_bytes()
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
}

fn data_hash(data: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    hash_bytes(&mut hash, data);
    hash
}

fn semantic_hash(terminal: &Terminal) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    let history = terminal.history_len();
    hash_bytes(&mut hash, &(history as u64).to_le_bytes());
    let (x, y) = terminal.cursor();
    hash_bytes(&mut hash, &(x as u64).to_le_bytes());
    hash_bytes(&mut hash, &(y as u64).to_le_bytes());
    for retained in 0..history + terminal.rows() {
        let (row, offset) = if retained < history {
            (0, history - retained)
        } else {
            (retained - history, 0)
        };
        let wrapped = terminal
            .view_row_wrap(row, offset)
            .map_or(u64::MAX, |value| value as u64);
        hash_bytes(&mut hash, &wrapped.to_le_bytes());
        for column in 0..terminal.cols() {
            let cell = terminal.view_cell(column, row, offset);
            hash_bytes(&mut hash, &[cell.width]);
            hash_bytes(&mut hash, &cell.flags.to_le_bytes());
            for ch in terminal.cell_chars(cell) {
                hash_bytes(&mut hash, &(ch as u32).to_le_bytes());
            }
            hash_bytes(&mut hash, &u32::MAX.to_le_bytes());
        }
    }
    hash
}

fn fragmented(terminal: &mut Terminal, data: &[u8]) {
    let mut offset = 0;
    let mut fragment = 0;
    while offset < data.len() {
        let end = (offset + FRAGMENTS[fragment % FRAGMENTS.len()]).min(data.len());
        terminal.feed(&data[offset..end]);
        offset = end;
        fragment += 1;
    }
}

fn verify_fragments(data: &[u8]) -> u64 {
    let mut whole = Terminal::new(COLS, ROWS, HISTORY);
    whole.feed(data);
    let mut split = Terminal::new(COLS, ROWS, HISTORY);
    fragmented(&mut split, data);
    let expected = semantic_hash(&whole);
    assert_eq!(
        semantic_hash(&split),
        expected,
        "UTF-8/control/grapheme chunking changed retained text, width, flags, wraps, or cursor"
    );
    expected
}

fn measure(name: &str, data: &[u8], split: bool, iterations: usize, samples: usize) {
    let reference_hash = verify_fragments(data);
    let text = std::str::from_utf8(data).unwrap();
    let unique: BTreeSet<char> = text.chars().filter(|ch| !ch.is_ascii()).collect();
    let mut pairs = BTreeSet::new();
    let mut previous = None;
    let mut pair_count = 0;
    for ch in text.chars() {
        if let Some(left) = previous
            && !ch.is_ascii()
        {
            pair_count += 1;
            pairs.insert((left, ch));
        }
        previous = (!ch.is_ascii()).then_some(ch);
    }
    let mut terminal = Terminal::new(COLS, ROWS, HISTORY);
    let feed = |terminal: &mut Terminal| {
        if split {
            fragmented(terminal, black_box(data));
        } else {
            terminal.feed(black_box(data));
        }
        terminal.clear_dirty();
    };
    for _ in 0..WARMUP {
        feed(&mut terminal);
    }
    let mut rates = vec![0.0f64; samples];
    let mut elapsed_ns = vec![0u128; samples];
    let mut allocations = vec![0usize; samples];
    for index in 0..samples {
        let before = ALLOCATIONS.load(Relaxed);
        let started = Instant::now();
        for _ in 0..iterations {
            feed(&mut terminal);
        }
        let elapsed = started.elapsed();
        allocations[index] = ALLOCATIONS.load(Relaxed) - before;
        elapsed_ns[index] = elapsed.as_nanos();
        rates[index] = data.len() as f64 * iterations as f64 / elapsed.as_secs_f64() / 1_048_576.0;
        black_box(terminal.cell(0, 0));
    }
    let output_hash = semantic_hash(&terminal);
    let mut sorted = rates.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "{{\"case\":\"{name}\",\"bytes_per_feed\":{},\"payload_fnv1a64\":\"{:016x}\",\"non_ascii_scalars\":{},\"unique_non_ascii_scalars\":{},\"adjacent_non_ascii_pairs\":{},\"unique_adjacent_non_ascii_pairs\":{},\"fragmented\":{split},\"iterations_per_sample\":{iterations},\"warmup_feeds\":{WARMUP},\"samples_mib_s\":{:?},\"elapsed_ns\":{:?},\"allocations\":{:?},\"median_mib_s\":{},\"single_feed_semantic_fnv1a64\":\"{reference_hash:016x}\",\"final_semantic_fnv1a64\":\"{output_hash:016x}\",\"fragment_equivalence\":true}}",
        data.len(),
        data_hash(data),
        text.chars().filter(|ch| !ch.is_ascii()).count(),
        unique.len(),
        pair_count,
        pairs.len(),
        rates,
        elapsed_ns,
        allocations,
        sorted[samples / 2]
    );
}

fn positive(value: Option<String>, name: &str) -> usize {
    let value = value
        .expect("missing numeric argument")
        .parse::<usize>()
        .expect(name);
    assert!(value > 0, "{name} must be positive");
    value
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mut units = 65_536;
    let mut iterations = 8;
    let mut samples = 7;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--units" => units = positive(arguments.next(), "units"),
            "--iterations" => iterations = positive(arguments.next(), "iterations"),
            "--samples" => samples = positive(arguments.next(), "samples"),
            "--help" => {
                println!("benchmark_diverse [--units 65536] [--iterations 8] [--samples 7]");
                return;
            }
            _ => panic!("unknown argument: {argument}"),
        }
    }
    assert!(
        (16_384..=262_144).contains(&units),
        "units must remain a diverse 16K..256K workload"
    );
    assert!((1..=128).contains(&iterations) && (3..=21).contains(&samples) && samples % 2 == 1);
    println!(
        "{{\"schema\":1,\"scope\":\"parser and retained screen model; no PTY or renderer\",\"seed\":\"{SEED:016x}\",\"units\":{units},\"geometry\":[{COLS},{ROWS}],\"history_rows\":{HISTORY},\"cell_bytes\":{}}}",
        std::mem::size_of::<Cell>()
    );
    let log = unicode_log(units);
    let clusters = cluster_log(units);
    measure("diverse-unicode-log", &log, false, iterations, samples);
    measure(
        "diverse-unicode-log-fragmented",
        &log,
        true,
        iterations,
        samples,
    );
    measure(
        "diverse-emoji-combining-log",
        &clusters,
        false,
        iterations,
        samples,
    );
    measure(
        "diverse-emoji-combining-log-fragmented",
        &clusters,
        true,
        iterations,
        samples,
    );
}
