//! Wasm component call benchmarks—the opt-in runtime's per-call cost.
//!
//! ```bash
//! make bench-wasm # print the table (opt-in; builds wasmtime)
//! cargo bench -p elysper-wasm-runtime --bench wasm_calls -- --check
//! ```
//!
//! This file is deliberately **not** part of `make bench-check`: the
//! crate carries `wasmtime` (a JIT plus Cranelift), and the default CI
//! gate must stay fast. Run it when the Wasm call path changes, or when
//! a plugin's compilation/instantiation contract moves.
//!
//! Same harness discipline as the other hot-path benches:
//! dependency-free (`harness = false`), a table by default, ratios under
//! `--check`, min-of-N passes, fixtures outside the measured loop,
//! `std::hint::black_box`. The absolute numbers are only comparable on
//! one machine; every factor is measured in this process.
//!
//! The rows pin the performance-budget spec's Wasm contract:
//!
//! - **Compilation cache**: a second mount of the same bytes reuses the
//!   compiled component (content hash → `Weak<Component>`), so the hit
//!   must stay far cheaper than compiling again.
//! - **`describe()` once**: the schema returned at mount is cached; a
//!   call must not re-enter the guest to describe itself.
//! - **Instance reuse**: one mounted component keeps its store/instance
//!   pair across calls instead of re-instantiating per call.

use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use elysper_wasm_runtime::WasmRuntime;

/// Floors calibrated from the phase-0 table: each recorded factor minus
/// a 5% allowance, so a regression beyond 5% fails the gate.
const COMPILE_CACHE_FLOOR: f64 = 950.0;
const INSTANCE_REUSE_FLOOR: f64 = 75.0;

/// The cached schema read is below the timer's resolution (0.3 ns), so
/// the recorded factor (≈130 000x, bounded by the ~35 µs mount on the
/// other side) cannot support a 5% band. The floor is 1 000x: any change
/// that makes a call re-enter the guest to describe itself drops the
/// factor to ~1 and fails.
const DESCRIBE_FLOOR: f64 = 1_000.0;

/// What every echo fixture call receives.
const INPUT: &str = r#"{"value":"bench"}"#;

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
    // Compilation rows cannot afford a large warmup; the clamp keeps the
    // first-touch costs out without recompiling for seconds.
    let warmup = (iters / 20).clamp(1, 50);
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
// The fixture ABI
//
// A real WebAssembly component: `describe() -> string` returns a tool
// schema; `execute(input: string) -> string` echoes the input. The
// canonical ABI lifts strings through `cabi_realloc`; the bump allocator
// starts at 8192, above the static payload and the 8-byte return slot at
// 4096. The initial memory is 64 pages (4 MiB) rather than the tests'
// single page: the bench calls `execute` tens of thousands of times and
// the bump allocator never frees, so one page traps after a few thousand
// calls. It is the same fixture the integration tests use, reduced to the
// one call path this bench measures.
// ---------------------------------------------------------------------

fn wat_string(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn echo_component() -> Vec<u8> {
    const SCHEMA: &str = concat!(
        r#"{"name":"echo","description":"Echo the JSON input","#,
        r#""parameters":{"type":"object","properties":{"value":{"type":"string"}}}}"#,
    );
    let schema = wat_string(SCHEMA);
    format!(
        r#"(component
  (core module $lib
    (memory (export "memory") 64)
    (global $next (mut i32) (i32.const 8192))
    (data (i32.const 16) "{schema}")
    (func (export "cabi_realloc")
      (param $old_ptr i32) (param $old_size i32)
      (param $align i32) (param $new_size i32)
      (result i32)
      (local $ptr i32)
      (local $mask i32)
      (local.set $mask (i32.sub (local.get $align) (i32.const 1)))
      (local.set $ptr
        (i32.and
          (i32.add (global.get $next) (local.get $mask))
          (i32.xor (local.get $mask) (i32.const -1))))
      (global.set $next (i32.add (local.get $ptr) (local.get $new_size)))
      (if (i32.ne (local.get $old_size) (i32.const 0))
        (then
          (memory.copy (local.get $ptr) (local.get $old_ptr)
            (if (result i32)
              (i32.lt_u (local.get $old_size) (local.get $new_size))
              (then (local.get $old_size))
              (else (local.get $new_size))))))
      (local.get $ptr))
    (func (export "describe") (result i32)
      (i32.store (i32.const 4096) (i32.const 16))
      (i32.store offset=4 (i32.const 4096) (i32.const {schema_len}))
      (i32.const 4096))
    (func (export "execute")
      (param $ptr i32) (param $len i32) (result i32)
      (i32.store (i32.const 4096) (local.get $ptr))
      (i32.store offset=4 (i32.const 4096) (local.get $len))
      (i32.const 4096))
  )
  (core instance $lib_inst (instantiate $lib))
  (func (export "describe") (result string)
    (canon lift (core func $lib_inst "describe")
      (memory (core memory $lib_inst "memory"))
      (realloc (core func $lib_inst "cabi_realloc"))))
  (func (export "execute") (param "input" string) (result string)
    (canon lift (core func $lib_inst "execute")
      (memory (core memory $lib_inst "memory"))
      (realloc (core func $lib_inst "cabi_realloc"))))
)"#,
        schema_len = SCHEMA.len(),
    )
    .into_bytes()
}

fn report(label: &str, iters: u32, elapsed: Duration, note: &str) {
    let per_op = elapsed.as_secs_f64() / f64::from(iters);
    let ns = per_op * 1e9;
    let ops = if per_op > 0.0 { 1.0 / per_op } else { 0.0 };
    println!("{label:<46}{ns:>10.1} ns{ops:>14.0} /s {note}");
}

/// `--check`: enforce the ratios instead of printing the table.
fn main_check() {
    let wasm = echo_component();
    let runtime = Arc::new(WasmRuntime::new().expect("build the engine"));
    let component = runtime.mount(&wasm, &[]).expect("mount the fixture");

    println!(
        "\nwasm invariants—each pair measured in this process, \n\
         so the factor is comparable across machines\n"
    );
    println!("{:<5}{:<54}{:>9}", "", "invariant", "measured");
    println!("{}", "-".repeat(96));

    let mut ok = true;
    ok &= ratio(
        "module cache: content-hash hit vs Component::new",
        20_000,
        || {
            black_box(runtime.compile(black_box(&wasm)).expect("cached"));
        },
        3,
        || {
            black_box(
                wasmtime::component::Component::new(
                    runtime.engine(),
                    black_box(&wasm),
                )
                .expect("compiles"),
            );
        },
        COMPILE_CACHE_FLOOR,
    );
    ok &= ratio(
        "describe(): cached schema vs a fresh mount (instantiate + describe)",
        2_000_000,
        || {
            black_box(component.schema());
        },
        100,
        || {
            black_box(runtime.mount(black_box(&wasm), &[]).expect("mount"));
        },
        DESCRIBE_FLOOR,
    );
    ok &= ratio(
        "instance reuse: execute vs a fresh mount per call",
        2_000,
        || {
            black_box(component.execute(black_box(INPUT)).expect("execute"));
        },
        100,
        || {
            let fresh = runtime.mount(black_box(&wasm), &[]).expect("mount");
            black_box(fresh.execute(black_box(INPUT)).expect("execute"));
        },
        INSTANCE_REUSE_FLOOR,
    );
    if !ok {
        eprintln!(
            "\nFAIL: a Wasm call invariant regressed. Re-run `make bench-wasm`\n\
             for the full table; if the change is intentional, adjust the\n\
             threshold in benches/wasm_calls.rs and say why in the commit."
        );
        std::process::exit(1);
    }
    println!("\nwasm invariants hold");
}

fn main() {
    if std::env::args().any(|arg| arg == "--check") {
        main_check();
        return;
    }

    let wasm = echo_component();
    let runtime = Arc::new(WasmRuntime::new().expect("build the engine"));
    let component = runtime.mount(&wasm, &[]).expect("mount the fixture");

    println!("\n{:<46}{:>13}{:>16}", "benchmark", "per op", "throughput");
    println!("{}", "-".repeat(96));

    // Warm the cache once; the row measures the hit path.
    let _ = runtime.compile(&wasm);
    Bench {
        label: "module cache hit (content hash)",
        iters: 20_000,
    }
    .run("second mount reuses it", || {
        black_box(runtime.compile(black_box(&wasm)).expect("cached"));
    });

    Bench {
        label: "component compile (no cache)",
        iters: 3,
    }
    .run("the work the cache replaces", || {
        black_box(
            wasmtime::component::Component::new(
                runtime.engine(),
                black_box(&wasm),
            )
            .expect("compiles"),
        );
    });

    Bench {
        label: "mount (compile cached + instantiate + describe)",
        iters: 100,
    }
    .run("one store + one describe", || {
        black_box(runtime.mount(black_box(&wasm), &[]).expect("mount"));
    });

    Bench {
        label: "describe() (cached schema read)",
        iters: 2_000_000,
    }
    .run("no guest call", || {
        black_box(component.schema());
    });

    Bench {
        label: "execute (reused instance)",
        iters: 2_000,
    }
    .run("one guest call", || {
        black_box(component.execute(black_box(INPUT)).expect("execute"));
    });

    Bench {
        label: "execute (fresh mount per call)",
        iters: 100,
    }
    .run("instantiate + describe + call", || {
        let fresh = runtime.mount(black_box(&wasm), &[]).expect("mount");
        black_box(fresh.execute(black_box(INPUT)).expect("execute"));
    });

    println!();
}
