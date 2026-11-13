//! Kernel hot-path benchmarks—the work plugin dispatch repeats.
//!
//! ```bash
//! make bench # print the table
//! make bench-check # enforce the ratios (CI)
//! cargo bench -p uva-kernel --bench hot_paths -- --check
//! ```
//!
//! Same harness discipline as `crates/uva-harness/benches/hot_paths.rs`:
//! dependency-free (`harness = false`), one table in the default mode,
//! ratios in `--check`, min-of-N passes, fixtures outside the measured
//! loop, `std::hint::black_box` to keep the optimiser honest.
//!
//! Three questions, one per seam the performance-budget spec pins:
//!
//! - **Assembly is near-linear**: `mount_all` over 4 / 8 / 16 plugins.
//!   The gate is a *ceiling* rather than a floor: four times the plugins
//!   may not cost more than four times the time (plus the 5% allowance).
//! - **An empty chain does no work**: `Waterfall::run` with no middleware
//!   must stay cheaper than the same call with one layer, which is the
//!   path that builds `Next` wrappers and boxes a future per layer.
//! - **Dispatch reads a registration snapshot**: an event with
//!   pre-registered listeners must stay far cheaper than registering the
//!   listeners for every emit—the pattern the snapshot replaced.
//!
//! The absolute nanoseconds are only comparable on the same machine;
//! every factor below is measured in this process, which is what lets
//! `--check` run on a shared CI runner.

use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use uva_kernel::{
    BoxFuture,
    Context,
    EffectHandle,
    EventValue,
    Inner,
    Outcome,
    PLUGIN_PROTOCOL_VERSION,
    Plugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
    Waterfall,
    WaterfallCtx,
    WaterfallError,
    mount_all,
};

/// The floor for the dispatch-snapshot row. Phase-0 baseline was
/// 7.9-10.3x; the floor is the smallest factor minus the 5% regression
/// allowance, so re-registering listeners per emit (which lands near 1x)
/// fails the gate.
const EVENT_DISPATCH_FLOOR: f64 = 7.5;

/// The ceiling for the 16-vs-4 assembly row. Observed 4.3-5.4x across
/// runs (a fully linear 4x, plus fiber and context overhead); the
/// ceiling leaves headroom for timer noise. A quadratic walk lands far
/// above it (16x).
const MOUNT_LINEAR_CEILING: f64 = 6.0;

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

/// Time `iters` runs of `f`, keeping the **minimum** of several passes.
///
/// The minimum is the estimator for "how fast can this run when nothing
/// interferes"; `--check` compares two such measurements taken here, in
/// one process, which is what makes its ratios portable across runners.
fn measure(iters: u32, mut f: impl FnMut()) -> Duration {
    // First-touch page faults and branch predictor state belong to the
    // harness, not to the measurement.
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

/// One enforceable floor: `fast` must beat `slow` by at least
/// `min_factor`. Both sides are measured in this process.
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

/// One enforceable ceiling: the larger case may cost at most
/// `max_factor` times the smaller one. Used where the invariant is
/// "grows no faster than linearly", which a floor cannot express.
fn ceiling(
    label: &str,
    small_per: f64,
    large_per: f64,
    max_factor: f64,
) -> bool {
    let factor = large_per / small_per;
    let held = factor <= max_factor;
    println!(
        "{} {:<54}{factor:>7.2}x  (max {max_factor:>5.2}x; {:.1} µs vs {:.1} µs)",
        if held { "OK  " } else { "FAIL" },
        label,
        small_per * 1e6,
        large_per * 1e6,
    );
    held
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

/// A plugin group for the assembly rows.
///
/// Plugin `i` provides `bench.svc.i` and injects `bench.svc.{i-1}`: the
/// group is a chain, so `mount_all` exercises validation, the inject
/// check, topological ordering, and one `require` per consumer. That is
/// the realistic shape—independent plugins would make ordering trivial
/// and hide a quadratic regression in the graph walk.
fn plugin_group(count: usize) -> Vec<Arc<dyn Plugin>> {
    (0..count)
        .map(|index| {
            let provides = vec![format!("bench.svc.{index}")];
            let inject = if index == 0 {
                Vec::new()
            } else {
                vec![format!("bench.svc.{}", index - 1)]
            };
            Arc::new(BenchPlugin {
                manifest: PluginManifest {
                    id: format!("bench-{index}"),
                    version: PLUGIN_PROTOCOL_VERSION.to_owned(),
                    runtime: RuntimeKind::Native,
                    provides,
                    inject,
                    capabilities: Vec::new(),
                    config_schema: None,
                },
                value: Arc::new(index),
            }) as Arc<dyn Plugin>
        })
        .collect()
}

struct BenchPlugin {
    manifest: PluginManifest,
    value: Arc<usize>,
}

impl Plugin for BenchPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let provided = ctx.provide_arc(
            self.manifest.provides[0].clone(),
            Arc::clone(&self.value),
        )?;
        if let Some(dependency) = self.manifest.inject.first() {
            black_box(ctx.require::<usize>(dependency)?);
        }
        Ok(vec![provided])
    }
}

/// The number of iterations the assembly rows run. The larger group
/// costs more per iteration, so it runs fewer times; `measure` divides
/// by the count, and the scaling gate compares per-plugin times.
const MOUNT_4_ITERS: u32 = 200;
const MOUNT_8_ITERS: u32 = 100;
const MOUNT_16_ITERS: u32 = 50;

const WATERFALL_ITERS: u32 = 200_000;
const EVENT_ITERS: u32 = 50_000;
const EVENT_REGISTER_ITERS: u32 = 10_000;

/// Mount `count` plugins and unload them, `MOUNT_*_ITERS` times.
fn bench_mount_scale(count: usize) -> (Duration, u32) {
    let iters = match count {
        0..=4 => MOUNT_4_ITERS,
        5..=8 => MOUNT_8_ITERS,
        _ => MOUNT_16_ITERS,
    };
    let context = Context::new();
    let plugins = plugin_group(count);
    let elapsed = measure(iters, || {
        let handle = mount_all(black_box(&context), black_box(&plugins))
            .expect("the fixture group mounts");
        black_box(handle.len());
    });
    (elapsed, iters)
}

/// The waterfall fixtures: one isolated context + request view, and the
/// innermost callback the chain wraps.
struct WaterfallFixture {
    waterfall: Waterfall,
    request: Arc<WaterfallCtx>,
    inner: Inner,
    // Held so the registrations stay installed for the fixture's life.
    _effects: Vec<EffectHandle>,
}

fn waterfall_fixture(middleware: usize) -> WaterfallFixture {
    let context = Context::new().isolate("bench-waterfall");
    let waterfall = Waterfall::new(&context);
    let mut effects = Vec::with_capacity(middleware);
    for _ in 0..middleware {
        let effect = waterfall
            .register(Arc::new(
                |ctx: Arc<WaterfallCtx>,
                 next: uva_kernel::Next|
                 -> BoxFuture<Result<Outcome, WaterfallError>> {
                    Box::pin(async move { next(ctx).await })
                },
            ))
            .expect("register the bench middleware");
        effects.push(effect);
    }
    WaterfallFixture {
        waterfall,
        request: Arc::new(WaterfallCtx::new(
            "bench/waterfall",
            EventValue::new(()),
            context,
        )),
        inner: Arc::new(|_ctx| Box::pin(async { Ok(Outcome::Continue) })),
        _effects: effects,
    }
}

/// Run one chain pass to completion.
fn run_waterfall(fixture: &WaterfallFixture) {
    let outcome = futures::executor::block_on(
        fixture
            .waterfall
            .run(Arc::clone(&fixture.request), Arc::clone(&fixture.inner)),
    )
    .expect("the bench chain never fails");
    black_box(matches!(outcome, Outcome::Continue));
}

/// A root context with `listeners` registered on `bench/ping`, plus a
/// child scope kept alive so the registration cannot be revoked while
/// the emit rows run.
fn event_fixture(listeners: usize) -> (Context, Context, Vec<EffectHandle>) {
    let root = Context::new();
    let scope = root.isolate("bench-events");
    let mut effects = Vec::with_capacity(listeners);
    for _ in 0..listeners {
        let effect = scope
            .on("bench/ping", |_event| Ok(None))
            .expect("register the bench listener");
        effects.push(effect);
    }
    (root, scope, effects)
}

fn emit(root: &Context) {
    root.inner()
        .emit(black_box("bench/ping"), [])
        .expect("emit");
}

fn report(label: &str, iters: u32, elapsed: Duration, note: &str) {
    let per_op = elapsed.as_secs_f64() / f64::from(iters);
    let ns = per_op * 1e9;
    let ops = if per_op > 0.0 { 1.0 / per_op } else { 0.0 };
    println!("{label:<46}{ns:>10.1} ns{ops:>14.0} /s {note}");
}

/// `--check`: enforce the ratios instead of printing the table. Exits
/// non-zero on the first failed invariant, so CI reports it as a gate.
fn main_check() {
    println!(
        "\nkernel invariants—each pair measured in this process, \n\
         so the factor is comparable across machines\n"
    );
    println!("{:<5}{:<54}{:>9}", "", "invariant", "measured");
    println!("{}", "-".repeat(96));

    let mut ok = true;

    // Assembly growth: 16 plugins may cost at most 4× the 4-plugin time
    // plus the 5% allowance. A quadratic walk or a re-scan per plugin
    // pushes this well past the ceiling.
    let (mount_4, iters_4) = bench_mount_scale(4);
    let (mount_16, iters_16) = bench_mount_scale(16);
    ok &= ceiling(
        "mount_all: 16 plugins vs 4 (linear-growth ceiling)",
        mount_4.as_secs_f64() / f64::from(iters_4),
        mount_16.as_secs_f64() / f64::from(iters_16),
        MOUNT_LINEAR_CEILING,
    );

    let empty = waterfall_fixture(0);
    let one = waterfall_fixture(1);
    ok &= ratio(
        "waterfall: empty chain vs one middleware",
        WATERFALL_ITERS,
        || run_waterfall(&empty),
        WATERFALL_ITERS,
        || run_waterfall(&one),
        // Phase-0 baseline 1.8-2.2x; the floor is the smallest factor
        // minus 5%. Losing the empty-chain fast path lands near 1x.
        1.7,
    );

    let (snapshot_root, _snapshot_scope, _snapshot_effects) = event_fixture(8);
    ok &= ratio(
        "event dispatch: registration snapshot vs re-registering per emit",
        EVENT_ITERS,
        || emit(&snapshot_root),
        EVENT_REGISTER_ITERS,
        || {
            let (root, scope, _effects) = event_fixture(8);
            emit(&root);
            drop(scope);
        },
        EVENT_DISPATCH_FLOOR,
    );

    if !ok {
        eprintln!(
            "\nFAIL: a kernel hot-path invariant regressed. Re-run `make bench`\n\
             for the full table; if the change is intentional, adjust the\n\
             threshold in benches/hot_paths.rs and say why in the commit."
        );
        std::process::exit(1);
    }
    println!("\nall kernel performance invariants hold");
}

fn main() {
    if std::env::args().any(|arg| arg == "--check") {
        main_check();
        return;
    }

    println!("\n{:<46}{:>13}{:>16}", "benchmark", "per op", "throughput");
    println!("{}", "-".repeat(96));

    // ---- assembly growth --------------------------------------------
    for (count, iters) in
        [(4, MOUNT_4_ITERS), (8, MOUNT_8_ITERS), (16, MOUNT_16_ITERS)]
    {
        let (elapsed, _) = bench_mount_scale(count);
        report(
            &format!("mount_all ({count} plugins)"),
            iters,
            elapsed,
            "mount + unload",
        );
    }

    // ---- waterfall chain --------------------------------------------
    let empty = waterfall_fixture(0);
    Bench {
        label: "Waterfall::run (empty chain)",
        iters: WATERFALL_ITERS,
    }
    .run("direct inner call", || run_waterfall(&empty));

    let one = waterfall_fixture(1);
    Bench {
        label: "Waterfall::run (1 middleware)",
        iters: WATERFALL_ITERS,
    }
    .run("one Next wrapper", || run_waterfall(&one));

    // ---- event dispatch ---------------------------------------------
    let (snapshot_root, _snapshot_scope, _snapshot_effects) = event_fixture(8);
    Bench {
        label: "event emit (8 listeners, pre-registered)",
        iters: EVENT_ITERS,
    }
    .run("registration snapshot", || emit(&snapshot_root));

    Bench {
        label: "event emit (8 listeners, per-emit registration)",
        iters: EVENT_REGISTER_ITERS,
    }
    .run("the pattern the snapshot replaced", || {
        let (root, scope, _effects) = event_fixture(8);
        emit(&root);
        drop(scope);
    });

    println!();
}
