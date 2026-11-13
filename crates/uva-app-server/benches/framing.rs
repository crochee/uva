//! Content-Length framing benchmarks—the transport cost of every frame.
//!
//! ```bash
//! make bench # print the table
//! make bench-check # enforce the ratios (CI)
//! cargo bench -p uva-app-server --bench framing -- --check
//! ```
//!
//! Same harness discipline as `crates/uva-harness/benches/hot_paths.rs`:
//! dependency-free (`harness = false`), a table by default, ratios under
//! `--check`, min-of-N passes, fixtures outside the measured loop. The
//! async codec runs on a current-thread tokio runtime (built once, never
//! inside a measured loop), which is the smallest host the stdio pump
//! can use.
//!
//! What the rows pin down, one per requirement in the performance-budget
//! spec:
//!
//! - **Parse once**: a frame's bytes deserialize straight into the typed
//!   request. The baseline is what the current dispatch path pays—parse
//!   into `serde_json::Value`, then `from_value` into the same struct.
//! - **Round trip**: `write_frame` → `read_frame` → typed parse, with
//!   the `Value` intermediate as the comparison.
//! - **Buffer reuse**: 1,000 small frames read through one caller-owned
//!   `BufReader` versus a fresh `BufReader` per frame.
//! - **Bounded frames**: a 1 MiB body rejected from its header under a
//!   small `max_frame_size` must stay far cheaper than reading it.

use std::{
    hint::black_box,
    io::{BufReader, Cursor, Seek, SeekFrom},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::Value;
use uva_app_server::rpc::framing::{
    DEFAULT_MAX_FRAME_SIZE,
    read_frame,
    read_frame_blocking,
    write_frame,
    write_frame_blocking,
};

/// Floors calibrated from the phase-0 table, set below the smallest
/// observed factor with a noise allowance; the gate compares ratios in
/// one process, not absolute time.
///
/// Phase-0 factors: parse 2.4-3.0x, round trip 1.8-1.9x, buffer reuse
/// 1.05-1.2x, oversized rejection 96-133x. The round-trip and oversized
/// floors leave a wider margin where timer noise dominates; the parse
/// floor is rounded down once more because its 300 ns fast side is where
/// noise shows up.
///
/// The buffer-reuse factor is the smallest of the set; its floor is
/// rounded down to 1.02 because its per-frame allocation is a small
/// share of the frame read—a floor at the 5% band (≈1.00) could not
/// distinguish "reuse removed" from the measurement's own noise.
const PARSE_FLOOR: f64 = 2.2;
const ROUND_TRIP_FLOOR: f64 = 1.65;
const BUFFER_REUSE_FLOOR: f64 = 1.02;
const OVERSIZE_FLOOR: f64 = 80.0;

/// One measured entry: total time over `iters` runs, reported per op.
struct Bench {
    label: &'static str,
    iters: u32,
}

impl Bench {
    fn run(self, note: &str, f: impl FnMut()) {
        let best = measure(self.iters, f);
        report(self.label, self.iters, best, note);
    }
}

fn measure(iters: u32, mut f: impl FnMut()) -> Duration {
    let warmup = (iters / 20).clamp(1, 200);
    for _ in 0..warmup {
        f();
    }

    const REPEATS: u32 = 5;
    let mut best = Duration::MAX;
    for _ in 0..REPEATS {
        let start = Instant::now();
        for _ in 0..iters {
            f();
        }
        best = best.min(start.elapsed());
    }
    best
}

fn ratio(
    label: &str,
    fast_iters: u32,
    fast: impl FnMut(),
    slow_iters: u32,
    slow: impl FnMut(),
    min_factor: f64,
) -> bool {
    let fast_per =
        measure(fast_iters, fast).as_secs_f64() / f64::from(fast_iters);
    let slow_per =
        measure(slow_iters, slow).as_secs_f64() / f64::from(slow_iters);
    let factor = slow_per / fast_per;
    let held = factor >= min_factor;
    println!(
        "{} {:<54}{factor:>7.1}x  (min {min_factor:>5.1}x; {:.1} ns vs {:.0} ns)",
        if held { "OK  " } else { "FAIL" },
        label,
        fast_per * 1e9,
        slow_per * 1e9,
    );
    held
}

// ---------------------------------------------------------------------
// The typed request the frame should deserialize into
//
// The shape is a real method call (`chat/send`): the envelope plus a
// params object. Reading the fields back is what keeps the compiler from
// eliding the deserialization, and it is what a handler does anyway.
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct WireRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: WireParams,
}

#[derive(Deserialize)]
struct WireParams {
    session_id: String,
    message: String,
    stream: bool,
}

/// One pass: parse the bytes into the typed request and touch every
/// field a dispatcher would.
fn parse_typed(bytes: &[u8]) -> usize {
    let request: WireRequest =
        serde_json::from_slice(bytes).expect("the fixture is a valid request");
    black_box(request.jsonrpc.len())
        + black_box(request.id as usize)
        + black_box(request.method.len())
        + black_box(request.params.session_id.len())
        + black_box(request.params.message.len())
        + usize::from(black_box(request.params.stream))
}

/// The current dispatch shape: parse into `Value`, then `from_value`
/// into the typed request (`RpcRouter::dispatch` + `params`).
fn parse_via_value(bytes: &[u8]) -> usize {
    let value: Value =
        serde_json::from_slice(bytes).expect("the fixture is a valid request");
    let request: WireRequest =
        serde_json::from_value(value).expect("the fixture is a valid request");
    black_box(request.jsonrpc.len())
        + black_box(request.id as usize)
        + black_box(request.method.len())
        + black_box(request.params.session_id.len())
        + black_box(request.params.message.len())
        + usize::from(black_box(request.params.stream))
}

/// Write one frame, read it back, then parse the payload.
fn round_trip(
    runtime: &tokio::runtime::Runtime,
    parse: fn(&[u8]) -> usize,
    payload: &[u8],
) -> usize {
    runtime.block_on(async {
        let mut buffer = Vec::with_capacity(payload.len() + 32);
        write_frame(&mut buffer, payload)
            .await
            .expect("write the frame");
        let mut reader: &[u8] = &buffer;
        let bytes = read_frame(&mut reader, DEFAULT_MAX_FRAME_SIZE)
            .await
            .expect("read the frame back");
        parse(&bytes)
    })
}

/// Frames are written once; the buffer-reuse rows replay these bytes.
fn frame_wire(payload: &[u8], count: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let mut frame = Vec::with_capacity(payload.len() + 32);
        write_frame_blocking(&mut frame, payload).expect("write the frame");
        frames.push(frame);
    }
    let concatenated = frames.concat();
    (frames, concatenated)
}

fn report(label: &str, iters: u32, elapsed: Duration, note: &str) {
    let per_op = elapsed.as_secs_f64() / f64::from(iters);
    let ns = per_op * 1e9;
    let ops = if per_op > 0.0 { 1.0 / per_op } else { 0.0 };
    println!("{label:<46}{ns:>10.1} ns{ops:>14.0} /s {note}");
}

/// `--check`: enforce the ratios instead of printing the table.
fn main_check() {
    println!(
        "\nframing invariants—each pair measured in this process, \n\
         so the factor is comparable across machines\n"
    );
    println!("{:<5}{:<54}{:>9}", "", "invariant", "measured");
    println!("{}", "-".repeat(96));

    let payload = payload_fixture();
    let frames = 500;
    let (wire_frames, wire) = frame_wire(&payload, frames);
    let mut reader = BufReader::new(Cursor::new(wire.as_slice()));
    let big = big_frame();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");

    let mut ok = true;
    ok &= ratio(
        "frame parse: typed once vs Value then typed",
        200_000,
        || {
            black_box(parse_typed(black_box(&payload)));
        },
        100_000,
        || {
            black_box(parse_via_value(black_box(&payload)));
        },
        PARSE_FLOOR,
    );
    ok &= ratio(
        "frame round trip: typed parse vs Value intermediate",
        20_000,
        || {
            black_box(round_trip(&runtime, parse_typed, black_box(&payload)));
        },
        20_000,
        || {
            black_box(round_trip(
                &runtime,
                parse_via_value,
                black_box(&payload),
            ));
        },
        ROUND_TRIP_FLOOR,
    );
    ok &= ratio(
        &format!("read {frames} small frames: one BufReader vs one per frame"),
        200,
        || {
            reader.seek(SeekFrom::Start(0)).expect("rewind the wire");
            for _ in 0..frames {
                let frame =
                    read_frame_blocking(&mut reader, DEFAULT_MAX_FRAME_SIZE)
                        .expect("a small frame");
                black_box(frame.len());
            }
        },
        200,
        || {
            for frame in &wire_frames {
                let mut one = BufReader::new(Cursor::new(frame.as_slice()));
                let bytes =
                    read_frame_blocking(&mut one, DEFAULT_MAX_FRAME_SIZE)
                        .expect("a small frame");
                black_box(bytes.len());
            }
        },
        BUFFER_REUSE_FLOOR,
    );
    ok &= ratio(
        "oversized frame: header rejection vs reading the body",
        2_000,
        || {
            let mut slice: &[u8] = big.as_slice();
            let error = runtime
                .block_on(read_frame(&mut slice, 100))
                .expect_err("100 bytes is below the declared body");
            black_box(error.kind());
        },
        200,
        || {
            let mut slice: &[u8] = big.as_slice();
            let body = runtime
                .block_on(read_frame(&mut slice, DEFAULT_MAX_FRAME_SIZE))
                .expect("the frame is within the default bound");
            black_box(body.len());
        },
        OVERSIZE_FLOOR,
    );
    if !ok {
        eprintln!(
            "\nFAIL: a framing invariant regressed. Re-run `make bench` for\n\
             the full table; if the change is intentional, adjust the\n\
             threshold in benches/framing.rs and say why in the commit."
        );
        std::process::exit(1);
    }
    println!("\nframing invariants hold");
}

/// A representative request: an envelope plus a 200-byte-ish params
/// object.
fn payload_fixture() -> Vec<u8> {
    br#"{"jsonrpc":"2.0","id":1,"method":"chat/send","params":{"session_id":"01J8Z0Z0Z0Z0Z0Z0Z0Z0Z0Z0Z0","message":"summarise the closed-over windows in this file and list the callers","stream":true}}"#.to_vec()
}

/// A frame whose declared body is 1 MiB, written once.
fn big_frame() -> Vec<u8> {
    let body = vec![b'x'; 1024 * 1024];
    let mut frame = Vec::with_capacity(body.len() + 32);
    write_frame_blocking(&mut frame, &body).expect("write the big frame");
    frame
}

fn main() {
    if std::env::args().any(|arg| arg == "--check") {
        main_check();
        return;
    }

    let payload = payload_fixture();
    let frames = 500;
    let (wire_frames, wire) = frame_wire(&payload, frames);
    let mut reader = BufReader::new(Cursor::new(wire.as_slice()));
    let big = big_frame();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");

    println!("\n{:<46}{:>13}{:>16}", "benchmark", "per op", "throughput");
    println!("{}", "-".repeat(96));

    Bench {
        label: "parse request (typed, one pass)",
        iters: 200_000,
    }
    .run("no intermediate Value", || {
        black_box(parse_typed(black_box(&payload)));
    });

    Bench {
        label: "parse request (Value then typed)",
        iters: 100_000,
    }
    .run("the current dispatch path", || {
        black_box(parse_via_value(black_box(&payload)));
    });

    Bench {
        label: "frame round trip (typed)",
        iters: 20_000,
    }
    .run("write + read + parse", || {
        black_box(round_trip(&runtime, parse_typed, black_box(&payload)));
    });

    Bench {
        label: "frame round trip (Value)",
        iters: 20_000,
    }
    .run("write + read + Value + parse", || {
        black_box(round_trip(&runtime, parse_via_value, black_box(&payload)));
    });

    Bench {
        label: "read 500 frames (one BufReader)",
        iters: 200,
    }
    .run("buffer reused across frames", || {
        reader.seek(SeekFrom::Start(0)).expect("rewind the wire");
        for _ in 0..frames {
            let frame =
                read_frame_blocking(&mut reader, DEFAULT_MAX_FRAME_SIZE)
                    .expect("a small frame");
            black_box(frame.len());
        }
    });

    Bench {
        label: "read 500 frames (fresh BufReader each)",
        iters: 200,
    }
    .run("per-frame reader allocation", || {
        for frame in &wire_frames {
            let mut one = BufReader::new(Cursor::new(frame.as_slice()));
            let bytes = read_frame_blocking(&mut one, DEFAULT_MAX_FRAME_SIZE)
                .expect("a small frame");
            black_box(bytes.len());
        }
    });

    Bench {
        label: "oversized frame (1 MiB, rejected from header)",
        iters: 2_000,
    }
    .run("max_frame_size enforced", || {
        let mut slice: &[u8] = big.as_slice();
        let error = runtime
            .block_on(read_frame(&mut slice, 100))
            .expect_err("100 bytes is below the declared body");
        black_box(error.kind());
    });

    Bench {
        label: "1 MiB frame (read in full)",
        iters: 200,
    }
    .run("the cost the bound avoids", || {
        let mut slice: &[u8] = big.as_slice();
        let body = runtime
            .block_on(read_frame(&mut slice, DEFAULT_MAX_FRAME_SIZE))
            .expect("the frame is within the default bound");
        black_box(body.len());
    });

    println!();
}
