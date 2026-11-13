//! Hot-path benchmarks—the work a turn repeats, measured.
//!
//! ```bash
//! make bench # print the table
//! make bench-check # enforce the ratios (CI)
//! cargo bench -p uva-harness --bench hot_paths -- --check
//! ```
//!
//! Two modes. The default prints the table—compare an entry before and
//! after a change, on the same machine. `--check` enforces the **ratios**
//! that the table's absolute times cannot: every optimised path must stay
//! measurably faster than the code it replaced, both measured in the same
//! process. Ratios survive a loaded or different CI runner where absolute
//! nanoseconds do not, so those are the numbers a gate can be built on.
//!
//! The catalog memos (R43, R51) are compared against the expression they
//! replaced, spelled out inline. Two older wins—R42's ASCII fast path
//! and R48's token-unit accumulator—replaced code that is no longer in
//! the tree, so faithful copies of the old algorithms live in this file
//! (see "Reference implementations"): without them the gate could not tell
//! whether those optimisations are still there.
//!
//! Every entry is a function the agent loop (or the provider/tool layer
//! it calls) runs *per iteration* or *per streamed delta*, so a
//! regression here is a regression in every turn. The harness is
//! deliberately dependency-free (`harness = false`, no criterion): the
//! number that matters is the shape of the curve—allocation-heavy
//! paths scale badly with message count, and a wrapper library would
//! only tell us the same thing with a bigger dependency tree.
//!
//! Fixtures are built once, outside the measured loops, and
//! `std::hint::black_box` keeps the optimiser from folding a bench into
//! nothing. Each entry runs several passes and reports the **minimum**
//! per-op time (the estimator that ignores scheduler interference), so
//! compare a number against the same machine before the change, not
//! across machines.

use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use uva_context::{AgentState, ContextManager, TruncatingContextManager};
use uva_core::{
    cap_to_char_boundary,
    schema::validate_against_schema,
    token::estimate_token_count,
};
use uva_kernel::{Context as KernelContext, EffectHandle};
use uva_provider::{
    BlockAssembler,
    Content,
    ContentPart,
    Message,
    Role,
    StreamChunk,
    TextContent,
    ToolResult,
    estimate_messages_token_count,
    json_repair::{parse_tool_input, repair_json},
};
use uva_tool::{
    ToolCategory,
    ToolDescriptor,
    ToolExposure,
    called_tool_names,
    project_tool_definitions,
};

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
/// This machine is shared, so a single pass measures the scheduler as
/// much as the code. The minimum is the standard estimator for "how fast
/// can this run when nothing interferes"—compare it before and after a
/// change, never across machines. `--check` compares two measurements
/// taken here, in one process, which is what makes its ratios portable.
fn measure(iters: u32, mut f: impl FnMut()) -> Duration {
    // Warmup: first-touch page faults and branch predictor state belong
    // to the harness, not to the measurement.
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

/// One enforceable performance invariant: `fast` must beat `slow` by at
/// least `min_factor`. Both are measured in this process, so the *factor*
/// is comparable across machines even though the nanoseconds are not.
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
// Kernel seam rows (performance-budget spec)
//
// The plugin kernel is where a turn's repeated work now crosses a seam:
// a plugin resolves the `Arc` handles it needs once, at fiber activation,
// and event dispatch reads a snapshot built at registration time. These
// helpers build the two fixtures the rows below compare against—the
// per-call `Context::require` and the per-emit listener registration the
// spec forbids.
// ---------------------------------------------------------------------

/// The shape of a resolved service handle: a small `Arc` payload the
/// loop keeps for the life of its fiber.
struct BenchService {
    generation: usize,
}

/// A kernel context with one provided service, isolated so the bench
/// does not touch another fixture's key space.
fn resolved_service_fixture() -> (KernelContext, Arc<BenchService>, EffectHandle)
{
    let context = KernelContext::new().isolate("bench-service");
    let service = Arc::new(BenchService { generation: 7 });
    let effect = context
        .provide_arc("bench.service", Arc::clone(&service))
        .expect("provide the bench service");
    (context, service, effect)
}

/// A root context whose event bus has `listeners` registered on
/// `bench/ping` (through a child scope, so the caller emits from the
/// root exactly as a plugin would).
fn listened_events_fixture(
    listeners: usize,
) -> (KernelContext, Vec<EffectHandle>) {
    let root = KernelContext::new();
    let scope = root.isolate("bench-events");
    let mut effects = Vec::with_capacity(listeners);
    for _ in 0..listeners {
        let effect = scope
            .on("bench/ping", |_event| Ok(None))
            .expect("register the bench listener");
        effects.push(effect);
    }
    (root, effects)
}

// ---------------------------------------------------------------------
// Reference implementations
//
// Two of the framework's hot paths are only *known* to be fast relative
// to what they replaced, and the replacement is no longer in the tree.
// Keeping a faithful copy of the old code here is what turns "R42 made
// this 200× faster" from a claim in a report into an invariant `--check`
// can enforce: if either optimisation is ever dropped, its factor
// collapses and the gate fails.
//
// They are deliberately the *old* algorithms, kept bit-compatible with
// the current ones' results (the current code pins that equivalence in
// its own tests: `token_units_accumulate_to_the_same_estimate`).
// ---------------------------------------------------------------------

/// The pre-R42 estimator: a per-character walk with the CJK range check
/// and no `str::is_ascii` fast path.
fn slow_estimate_token_count(text: &str) -> usize {
    fn is_cjk(ch: char) -> bool {
        matches!(ch,
            '\u{4E00}'..='\u{9FFF}' |
            '\u{3400}'..='\u{4DBF}' |
            '\u{20000}'..='\u{2A6DF}' |
            '\u{2A700}'..='\u{2B73F}' |
            '\u{2B740}'..='\u{2B81F}' |
            '\u{2B820}'..='\u{2CEAF}' |
            '\u{F900}'..='\u{FAFF}' |
            '\u{2F800}'..='\u{2FA1F}' |
            '\u{3000}'..='\u{303F}' |
            '\u{3040}'..='\u{309F}' |
            '\u{30A0}'..='\u{30FF}' |
            '\u{AC00}'..='\u{D7AF}'
        )
    }

    let mut ascii_bytes = 0usize;
    let mut cjk_chars = 0usize;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk_chars += 1;
        } else {
            ascii_bytes += ch.len_utf8();
        }
    }
    let text_tokens =
        (ascii_bytes as f64 / 4.0 + cjk_chars as f64 / 1.5) as usize;
    text_tokens + (text_tokens as f64 * 0.05) as usize
}

/// The pre-R48 message estimator: build one `String` per message and
/// measure that, instead of folding `TokenUnits`.
///
/// It calls the *current* [`estimate_token_count`] (i.e. the post-R42
/// one) on purpose: the pair must isolate the accumulator change, not
/// re-measure R42's fast path inside R48's comparison.
fn slow_estimate_messages_token_count(messages: &[Message]) -> usize {
    use uva_provider::ContentPart;

    messages
        .iter()
        .map(|message| {
            let mut buffer = String::new();
            for part in message.content.iter() {
                match part {
                    ContentPart::Text(tc) => buffer.push_str(&tc.text),
                    ContentPart::Reasoning(rc) => buffer.push_str(&rc.text),
                    ContentPart::ToolResult(tr) => {
                        for inner in &tr.content {
                            if let ContentPart::Text(tc) = inner {
                                buffer.push_str(&tc.text);
                            }
                        }
                    }
                    ContentPart::ToolUse(tu) => {
                        buffer.push_str(&tu.name);
                        if let Ok(json) = serde_json::to_string(&tu.input) {
                            buffer.push_str(&json);
                        }
                    }
                    _ => {}
                }
            }
            estimate_token_count(&buffer)
        })
        .sum()
}

/// The ratios that stand in for "the memo is still there, and still
/// worth it". Each pair is *the same question answered twice*—the
/// memoised path against the expression it replaced—so a regression in
/// either direction (a cache that stopped hitting, or a rewrite that
/// reintroduced the scan) shows up as a failed factor rather than as a
/// number nobody compares.
fn check_ratios() -> bool {
    let registry = tool_registry(40);

    let mut ok = true;
    ok &= ratio(
        "descriptors_cached vs descriptors",
        2_000_000,
        || {
            black_box(registry.descriptors_cached());
        },
        5_000,
        || {
            black_box(registry.descriptors());
        },
        100.0,
    );
    ok &= ratio(
        "snapshot_cached vs snapshot",
        2_000_000,
        || {
            black_box(registry.snapshot_cached());
        },
        10_000,
        || {
            black_box(registry.snapshot());
        },
        100.0,
    );
    // R42's ASCII fast path and R48's unit accumulator, each against the
    // algorithm it replaced (kept above). The keep-alive string is not
    // pinned by the checker; what matters is that the fast path stays
    // overwhelmingly faster than the walk it removed.
    let ascii_4k = "the quick brown fox jumps over the lazy dog. ".repeat(96);
    ok &= ratio(
        "estimate_token_count (ASCII) vs the pre-R42 char walk",
        200_000,
        || {
            black_box(estimate_token_count(black_box(&ascii_4k)));
        },
        100,
        || {
            black_box(slow_estimate_token_count(black_box(&ascii_4k)));
        },
        50.0,
    );
    let history = tool_heavy_history(100);
    ok &= ratio(
        "estimate_messages_token_count vs the pre-R48 String-per-message",
        20_000,
        || {
            black_box(estimate_messages_token_count(black_box(&history)));
        },
        10_000,
        || {
            black_box(slow_estimate_messages_token_count(black_box(&history)));
        },
        // The floor is deliberately close to 1: dropping the accumulator
        // lands *at* 1.0, and the observed factor moves 1.9-6.3x with
        // machine state, so a higher floor would fail on a busy runner
        // without catching anything the 1.3 floor misses.
        1.3,
    );
    // The caps-sharing invariant: the loop must hold the shared
    // capability bundle by `Arc`, paying one refcount bump per
    // run, not a deep copy. The slow side is one full
    // `AgentCaps::clone()` (every plane's Arc refcount bump plus
    // the descriptor's `String`s); the fast side is an
    // `Arc::clone` plus two field reads through the `Deref` —
    // exactly what `ReActLoop` now does per run.
    let caps = Arc::new(uva_harness::AgentCaps::for_provider(Arc::new(
        ScriptedProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        },
    )
        as Arc<dyn uva_provider::ModelProvider>));
    ok &= ratio(
        "per-run Arc clone vs AgentCaps::clone",
        2_000_000,
        || {
            let caps = black_box(Arc::clone(&caps));
            black_box(&caps.model.provider);
            black_box(&caps.model.descriptor.name);
        },
        200_000,
        || {
            // `as_ref()` first: `.clone()` on `&Arc<AgentCaps>`
            // auto-derefs to `Arc::clone`, which is the fast side.
            black_box(black_box(caps.as_ref()).clone());
        },
        // Observed 27.5x / 36.0x / 38.7x over three runs: the slow
        // side is stable near 175 ns, the 5 ns fast side is small
        // enough that timer noise moves the ratio. The floor sits
        // below the observed minimum with the same headroom this
        // file's other cases use (`min 2.0` for an observed 6.2x),
        // so the gate still fires if the loop ever goes back to
        // copying the bundle per run — that lands near 1x.
        20.0,
    );

    // ---- V5 closure: the real-harness rows ------------------------
    //
    // Four scenarios, each measured against a baseline in this
    // process so the factor survives a different runner. The first
    // slot always holds the *scenario* (the thing under test) and
    // the second the *baseline* it is compared against.
    //
    // Two of them read backwards at first glance, so the rule is
    // worth stating: when the baseline is the cheaper side the
    // factor lands below 1, and the floor then acts as a ceiling on
    // the scenario's cost *relative to* the baseline. The gate
    // still fires when the scenario gets relatively more expensive —
    // which is the regression these rows exist to catch — and it
    // stays quiet when the scenario gets cheaper, which is the
    // direction an optimisation moves it.
    //
    // Every floor below is the smallest factor observed over three
    // runs of the whole benchmark, halved. That is the same
    // discipline the rows above use.
    let long_history = tool_heavy_history(100);
    let history_agent = scripted_turn_agent();
    ok &= ratio(
        "ReActAgent turn: 401-message history vs empty history",
        200,
        || {
            let input = history_input(long_history.clone());
            black_box(run_turn(&history_agent, input));
        },
        200,
        || {
            let input = history_input(Vec::new());
            black_box(run_turn(&history_agent, input));
        },
        // Observed 0.185x / 0.171x / 0.182x over three runs of the
        // whole benchmark: the 401-message turn costs about 5.5x the
        // empty one. The floor is half the observed minimum, so the
        // scenario may grow to roughly twice that relative cost
        // before the gate fires — which is what a reintroduced
        // transcript copy would do.
        0.08,
    );

    let workers =
        Arc::new(futures::executor::ThreadPool::new().expect("thread pool"));
    let mut parallel_registrations = Vec::new();
    let parallel_agent = batch_agent(
        20,
        slow_tool_registry(20, Duration::from_millis(1), Arc::clone(&workers)),
        None,
        &mut parallel_registrations,
    );
    let mut serial_registrations = Vec::new();
    let serial_agent = batch_agent(
        20,
        slow_tool_registry(20, Duration::from_millis(1), Arc::clone(&workers)),
        Some(1),
        &mut serial_registrations,
    );
    ok &= ratio(
        "ReActAgent turn: 20 parallel tools vs the same batch capped at 1",
        20,
        || {
            let input = history_input(Vec::new());
            black_box(run_turn(&parallel_agent, input));
        },
        20,
        || {
            let input = history_input(Vec::new());
            black_box(run_turn(&serial_agent, input));
        },
        // Observed 9.1x / 9.0x / 9.1x over three runs: 20 tools
        // that each take a millisecond of wall time cost 2.6 ms
        // together and 23.7 ms one after the other. The floor is
        // half the observed minimum, so the batch must stay clearly
        // more than serial — a change that serialised it lands near
        // 1x and fails.
        4.5,
    );

    let mut hook_registrations = Vec::new();
    let hooked = hooked_agent(8, &mut hook_registrations);
    let mut plain_registrations = Vec::new();
    let plain = hooked_agent(0, &mut plain_registrations);
    ok &= ratio(
        "ReActAgent turn: 8 host hooks vs none",
        2_000,
        || {
            let input = history_input(Vec::new());
            black_box(run_turn(&hooked, input));
        },
        2_000,
        || {
            let input = history_input(Vec::new());
            black_box(run_turn(&plain, input));
        },
        // Observed 0.925x / 0.945x / 0.931x over three runs: eight
        // observers cost about 7% over a turn with none. The floor
        // is half the observed minimum, so the observers may cost
        // roughly twice their measured share before the gate fires.
        0.46,
    );

    ok &= ratio(
        "TruncatingContextManager over-window vs NoopContextManager",
        2_000,
        || {
            let mut messages = long_history.clone();
            let mut state = AgentState::with_window(2_000);
            futures::executor::block_on(
                TruncatingContextManager.prepare(&mut messages, &mut state),
            );
            black_box(messages.len());
        },
        2_000,
        || {
            let mut messages = long_history.clone();
            let mut state = AgentState::with_window(2_000);
            futures::executor::block_on(
                uva_context::NoopContextManager
                    .prepare(&mut messages, &mut state),
            );
            black_box(messages.len());
        },
        // Observed 0.272x / 0.272x / 0.277x over three runs: the
        // over-window prune costs about 3.7x the estimate-only
        // baseline. The floor is half the observed minimum, so the
        // prune may cost roughly twice its measured share before the
        // gate fires.
        0.13,
    );

    // ---- kernel seams -----------------------------------------------
    // The service handle is resolved once at activation; a regression
    // that reintroduces a per-call lookup collapses this factor toward
    // the cost of `require` itself.
    let (service_context, cached_service, _service_effect) =
        resolved_service_fixture();
    ok &= ratio(
        "service handle: cached Arc vs Context::require per call",
        5_000_000,
        || {
            let handle = black_box(Arc::clone(&cached_service));
            black_box(handle.generation);
        },
        500_000,
        || {
            black_box(
                service_context
                    .require::<BenchService>(black_box("bench.service"))
                    .expect("the fixture provides it"),
            );
        },
        // Observed 45-57x across runs (the 3.3 ns fast side is where
        // timer noise shows up). The floor stays far above 1x, which is
        // what a per-call lookup would land at.
        40.0,
    );

    // An empty dispatch must stay cheaper than one that clones a
    // callback and drives a boxed future. A regression that makes the
    // empty path build the chain lands near 1.0 and fails the floor.
    let quiet = KernelContext::new();
    let (listened, _listeners) = listened_events_fixture(1);
    ok &= ratio(
        "event dispatch: 0 listeners vs 1 listener",
        500_000,
        || {
            quiet
                .inner()
                .emit(black_box("bench/ping"), [])
                .expect("emit");
        },
        500_000,
        || {
            listened
                .inner()
                .emit(black_box("bench/ping"), [])
                .expect("emit");
        },
        // The no-listener path skips the callback clone and the boxed
        // future; observed 1.9-3.1x on this machine. The floor only has
        // to catch "the empty path builds the chain anyway" (near 1.0).
        1.5,
    );
    ok
}

/// `--check`: enforce the ratios instead of printing the table. Exits
/// non-zero on the first failed invariant, so CI reports it as a gate.
fn main_check() {
    println!(
        "\nperformance invariants—each pair measured in this process, \n\
         so the factor is comparable across machines\n"
    );
    println!("{:<5}{:<54}{:>9}", "", "invariant", "measured");
    println!("{}", "-".repeat(96));
    if !check_ratios() {
        eprintln!(
            "\nFAIL: a memoised path lost its advantage. Re-run `make bench`\n\
             for the full table; if the change is intentional, adjust the\n\
             minimum factor in benches/hot_paths.rs and say why in the commit."
        );
        std::process::exit(1);
    }
    println!("\nall performance invariants hold");
}

fn report(label: &str, iters: u32, elapsed: Duration, note: &str) {
    let per_op = elapsed.as_secs_f64() / f64::from(iters);
    let ns = per_op * 1e9;
    let ops = if per_op > 0.0 { 1.0 / per_op } else { 0.0 };
    println!("{label:<46}{ns:>10.1} ns{ops:>14.0} /s {note}");
}

fn main() {
    if std::env::args().any(|arg| arg == "--check") {
        main_check();
        return;
    }

    let text_4k = "the quick brown fox jumps over the lazy dog. ".repeat(96);
    let text_200k = "abcdefghij".repeat(20_000);

    // ---- fixtures ---------------------------------------------------
    let messages = tool_heavy_history(100);
    let descriptors = tool_descriptors(40);
    let visible: std::collections::HashSet<String> = descriptors
        .iter()
        .filter(|d| d.exposure != ToolExposure::Hidden)
        .map(|d| d.name.clone())
        .collect();
    let called: std::collections::HashSet<String> = descriptors
        .iter()
        .filter(|d| d.exposure == ToolExposure::Deferred)
        .take(2)
        .map(|d| d.name.clone())
        .collect();

    let schema = serde_json::json!({
        "type": "object",
        "properties": (0..20).map(|i| (format!("field_{i}"), serde_json::json!({ "type": "string" }))).collect::<serde_json::Map<_, _>>(),
        "required": (0..10).map(|i| format!("field_{i}")).collect::<Vec<_>>(),
    });
    let valid_value = serde_json::Value::Object(
        (0..20)
            .map(|i| {
                (
                    format!("field_{i}"),
                    serde_json::Value::String("value".to_string()),
                )
            })
            .collect(),
    );
    let strict_json = nested_tool_args(120);
    let defective_json = format!("{} trailing", strict_json);

    println!("\n{:<46}{:>13}{:>16}", "benchmark", "per op", "throughput");
    println!("{}", "-".repeat(96));

    // ---- token accounting -------------------------------------------
    Bench {
        label: "estimate_token_count (4 KB ascii)",
        iters: 200_000,
    }
    .run("core heuristic", || {
        black_box(estimate_token_count(black_box(&text_4k)));
    });

    Bench {
        label: "estimate_messages_token_count (401 msgs)",
        iters: 20_000,
    }
    .run("compaction gate", || {
        black_box(estimate_messages_token_count(black_box(&messages)));
    });

    // ---- streaming assembly -----------------------------------------
    Bench {
        label: "BlockAssembler fold (400 deltas)",
        iters: 20_000,
    }
    .run("per-delta cost", || {
        let mut assembler = BlockAssembler::new();
        for i in 0..400 {
            assembler.push(StreamChunk::Content(ContentPart::Text(
                TextContent {
                    text: format!("delta-{i} "),
                    cache_control: None,
                },
            )));
        }
        black_box(assembler.finalize_assistant());
    });

    // ---- wire repair ------------------------------------------------
    Bench {
        label: "parse_tool_input (7.5 KB strict)",
        iters: 50_000,
    }
    .run("happy path", || {
        black_box(parse_tool_input(black_box(&strict_json)));
    });

    Bench {
        label: "repair_json (7.5 KB defective)",
        iters: 20_000,
    }
    .run("salvage path", || {
        black_box(repair_json(black_box(&defective_json)));
    });

    // ---- schema validation ------------------------------------------
    Bench {
        label: "validate_against_schema (20 fields)",
        iters: 100_000,
    }
    .run("structured output", || {
        black_box(validate_against_schema(
            black_box(&schema),
            black_box(&valid_value),
        ))
        .ok();
    });

    // ---- tool-surface projection ------------------------------------
    Bench {
        label: "project_tool_definitions (40 tools)",
        iters: 50_000,
    }
    .run("per request", || {
        black_box(project_tool_definitions(
            black_box(&descriptors),
            black_box(&called),
            Some(black_box(&visible)),
        ));
    });

    // ---- the per-iteration prep the loop does before caching -------
    Bench {
        label: "called_tool_names (401-message transcript)",
        iters: 20_000,
    }
    .run("deferred promotion", || {
        black_box(called_tool_names(black_box(&messages)));
    });

    let registry = tool_registry(40);
    Bench {
        label: "ToolRegistry::descriptors (40 tools)",
        iters: 20_000,
    }
    .run("per iteration, uncached", || {
        black_box(black_box(&registry).descriptors());
    });

    // Warm the version-keyed memo, then measure the loop's accessor.
    let _ = registry.descriptors_cached();
    Bench {
        label: "ToolRegistry::descriptors_cached (40 tools)",
        iters: 200_000,
    }
    .run("per iteration, warm", || {
        black_box(black_box(&registry).descriptors_cached());
    });

    // ---- the visible catalog ----------------------------------------
    // Three consumers answer "what should the model be told about?" and
    // every one of them starts from a registry read. These are the
    // numbers that say whether that read is a scan-and-clone or a memo.
    Bench {
        label: "ToolRegistry::snapshot (40 tools)",
        iters: 20_000,
    }
    .run("name+description clones + sort", || {
        black_box(black_box(&registry).snapshot());
    });

    // Warm the memo, then measure the accessor the catalog consumers use.
    let _ = registry.snapshot_cached();
    Bench {
        label: "ToolRegistry::snapshot_cached (40 tools)",
        iters: 2_000_000,
    }
    .run("per request, warm", || {
        black_box(black_box(&registry).snapshot_cached());
    });

    // ---- context window ---------------------------------------------
    // What the old `Arc::new(messages.clone())` inside the timed closures
    // was charging to the manager: one deep copy of the 401-message
    // fixture. Priced here so the report can say how much of the old
    // numbers was the fixture rather than the manager.
    Bench {
        label: "Vec<Message> clone (401 msgs)",
        iters: 5_000,
    }
    .run("the copy the old rows timed", || {
        black_box(black_box(&messages).clone());
    });

    // The history is `tool_heavy_history(100)` = **401** messages: one
    // system message plus four per pair, not the 200 these rows used
    // to claim. One copy is built here, outside the timed closures, so
    // the number is the manager's own work instead of "the manager plus
    // a full deep copy per iteration".
    //
    // Sharing it is also the production shape: the loop keeps the
    // history alive, so the manager's `Arc::try_unwrap` fails and it
    // materialises a fresh list when truncation fires. That copy *is*
    // the manager's cost, priced in the row above.
    let history = Arc::new(messages.clone());
    Bench {
        label: "TruncatingContextManager (401 msgs, over window)",
        iters: 2_000,
    }
    .run("fits check + evict", || {
        let mut state = AgentState::with_window(2_000);
        black_box(futures::executor::block_on(
            TruncatingContextManager
                .prepare_arc(Arc::clone(&history), &mut state),
        ));
    });

    Bench {
        label: "TruncatingContextManager (401 msgs, no eviction)",
        iters: 20_000,
    }
    .run("fast path", || {
        let mut state = AgentState::with_window(usize::MAX / 2);
        black_box(futures::executor::block_on(
            TruncatingContextManager
                .prepare_arc(Arc::clone(&history), &mut state),
        ));
    });

    // ---- one whole turn ---------------------------------------------
    // The number a user feels: a complete run—system prompt, one tool
    // round trip, final answer—with the provider scripted so the
    // measurement is the framework's own work (context prep, tool
    // dispatch, event stream, typed sink).
    let turn_agent = scripted_turn_agent();
    Bench {
        label: "ReActAgent turn (scripted, 1 tool call)",
        iters: 500,
    }
    .run("end to end", || {
        black_box(run_one_turn(&turn_agent));
    });

    // ---- the one clone the dispatch path still pays -----------------
    // `route.rs` hands the registry a `serde_json::Value` by value, so
    // the call's input is copied once per tool call. `guard.rs` copies
    // again only when a hook or gate is registered (its event is built
    // lazily). Switching `ToolUse.input` to `Arc<Value>` would remove
    // the remaining copy at the price of a workspace-wide refactor
    // (≈250 `ToolUse {` construction sites and ≈200 `.input` reads,
    // grep over `crates/`, tests included), so measure first: these
    // two rows are what the deferral costs against the turn above.
    //
    // The rows time `serde_json::Value::clone`—the exact call
    // `route.rs:39` makes—not a whole `ToolUse` (its `id` / `name`
    // `String`s are not part of it). The small fixture mirrors the
    // `read` call hard-coded in `tool_heavy_history`; the scripted
    // turn above actually sends `{"text": "bench"}`, which is
    // smaller, so the small number is an upper bound for that turn.
    let small_input = serde_json::json!({
        "path": "/workspace/file_0.rs",
        "offset": 0,
        "limit": 200,
    });
    Bench {
        label: "Value::clone (tool input, path + 2 ints)",
        iters: 2_000_000,
    }
    .run("per tool call", || {
        black_box(black_box(&small_input).clone());
    });

    // The same copy on a call carrying a large argument payload—the
    // shape where an `Arc` would actually earn its ripple.
    let nested_input: serde_json::Value =
        serde_json::from_str(&strict_json).expect("nested fixture");
    Bench {
        label: "Value::clone (tool input, 7.5 KB nested)",
        iters: 20_000,
    }
    .run("per tool call", || {
        black_box(black_box(&nested_input).clone());
    });

    // ---- text truncation --------------------------------------------
    // `cap_to_char_boundary` truncates in place, so the measured unit is
    // "own a fresh 200 KB buffer and cut it to 100 KB"—what a tool
    // result buffer actually does. The clone is part of the number and
    // is called out in the note.
    Bench {
        label: "cap_to_char_boundary (200 KB → 100 KB)",
        iters: 50_000,
    }
    .run("clone + utf-8 safe cut", || {
        let mut buffer = text_200k.clone();
        cap_to_char_boundary(&mut buffer, 100_000);
        black_box(buffer.len());
    });

    // ---- : compose_tool_definitions -----------
    // The shipping path is `pub(crate)` (it's an internal
    // tool-list assembler). The bench measures the same
    // shape from the public surface: a single `project`
    // call against the live descriptor list, which is the
    // inner loop Step 4 collapsed the previous 4-walk
    // shape into. The plan's reported numbers measure the
    // synthesised improvement against `compose_walk_v1` /
    // `compose_walk_v2`, but those reference shapes are
    // part of the framework's history now—what matters
    // is this row stays under 250 ns / call on the same
    // machine the prior 450 ns row lived on.
    let registry = build_harness_registry(40);
    let descriptors: std::sync::Arc<Vec<uva_tool::ToolDescriptor>> =
        registry.descriptors_cached();
    let called_small: std::collections::HashSet<String> =
        (0..2).map(|i| format!("tool_{}", i + 1)).collect();
    Bench {
        label: "compose_tool_definitions (single-pass)",
        iters: 200_000,
    }
    .run("v2", || {
        let _ = black_box(uva_tool::project_tool_definitions(
            black_box(&descriptors),
            black_box(&called_small),
            None,
        ));
    });

    // ---- : real harness — R1: a turn over a long history ----
    // G1: the request builder deep-copied the whole transcript
    // every turn. The row reports what one turn over a
    // 401-message history costs; the gate compares it against the
    // same turn with an empty history.
    let history_agent = scripted_turn_agent();
    let long_history = tool_heavy_history(100);
    Bench {
        label: "ReActAgent turn (401-message history)",
        iters: 200,
    }
    .run("one turn", || {
        let input = history_input(long_history.clone());
        black_box(run_turn(&history_agent, input));
    });

    // ---- : real harness — R2: one batch of 20 parallel tools ----
    // G3: the batch's concurrency. The tools take real time, so a
    // cap on the concurrency is visible in the wall clock. The
    // capped baseline lives in the `--check` gate, not in the table.
    let workers =
        Arc::new(futures::executor::ThreadPool::new().expect("thread pool"));
    let mut registrations = Vec::new();
    let parallel_agent = batch_agent(
        20,
        slow_tool_registry(20, Duration::from_millis(1), Arc::clone(&workers)),
        None,
        &mut registrations,
    );
    Bench {
        label: "ReActAgent turn (20 parallel tool calls)",
        iters: 20,
    }
    .run("one turn", || {
        black_box(run_turn(&parallel_agent, history_input(Vec::new())));
    });

    // ---- : real harness — R3: a turn with eight host hooks ----
    // The observer itself does nothing; the row measures the
    // dispatch around it. The hook-free baseline lives in the
    // `--check` gate, not in the table.
    let mut hook_registrations = Vec::new();
    let hooked = hooked_agent(8, &mut hook_registrations);
    Bench {
        label: "ReActAgent turn (8 host hooks)",
        iters: 2_000,
    }
    .run("one turn", || {
        black_box(run_turn(&hooked, history_input(Vec::new())));
    });

    // ---- : real harness — R4: the over-window context prune ----
    // G6: the ~305 µs the over-window path costs, unattributed
    // until V4. The row drives the manager directly, with a window
    // far below the estimate, so the whole prune is in view.
    let over_window_history = tool_heavy_history(100);
    Bench {
        label: "TruncatingContextManager over-window (401 msgs)",
        iters: 2_000,
    }
    .run("one prune", || {
        let mut messages = over_window_history.clone();
        let mut state = AgentState::with_window(2_000);
        futures::executor::block_on(
            TruncatingContextManager.prepare(&mut messages, &mut state),
        );
        black_box(messages.len());
    });

    // ---- kernel seams -----------------------------------------------
    // A plugin caches the `Arc` it resolves at fiber activation; the row
    // prices that cached read against the per-call `Context::require` it
    // replaced. Element/handler dispatch is per tool call, so the
    // difference is paid on every call of a turn.
    let (context, service, _effect) = resolved_service_fixture();
    Bench {
        label: "service handle resolve (cached Arc)",
        iters: 5_000_000,
    }
    .run("fiber activation, once", || {
        let handle = black_box(Arc::clone(&service));
        black_box(handle.generation);
    });

    Bench {
        label: "service handle resolve (Context::require)",
        iters: 500_000,
    }
    .run("per call, by key", || {
        black_box(
            context
                .require::<BenchService>(black_box("bench.service"))
                .expect("the fixture provides it"),
        );
    });

    // The no-listener fast path: an event with no listeners must return
    // without cloning a callback or boxing a future. The rows differ
    // only in whether the bus has a registration.
    let quiet = KernelContext::new();
    let (listened, _listeners) = listened_events_fixture(1);
    Bench {
        label: "event emit (0 listeners)",
        iters: 500_000,
    }
    .run("empty dispatch snapshot", || {
        quiet
            .inner()
            .emit(black_box("bench/ping"), [])
            .expect("emit");
    });
    Bench {
        label: "event emit (1 listener)",
        iters: 500_000,
    }
    .run("one callback + future", || {
        listened
            .inner()
            .emit(black_box("bench/ping"), [])
            .expect("emit");
    });

    println!();
}

/// A history shaped like a real tool-using turn: system prompt, then
/// user/assistant pairs whose assistant turns carry tool calls and whose
/// tool turns carry the results.
fn tool_heavy_history(pairs: usize) -> Vec<Message> {
    let mut messages = vec![Message::system("You are Uva. ".repeat(20))];
    for i in 0..pairs {
        messages.push(Message::user(format!(
            "Step {i}: read the file and summarise it. {}",
            "context ".repeat(10)
        )));
        messages.push(Message::new(
            Role::Assistant,
            Content::parts(vec![ContentPart::ToolUse(
                uva_provider::ToolUse {
                    id: format!("call-{i}"),
                    name: "read".to_string(),
                    input: serde_json::json!({ "path": format!("/workspace/file_{i}.rs"), "offset": i, "limit": 200 }),
                },
            )]),
        ));
        messages.push(Message::new(
            Role::Tool,
            Content::Single(ContentPart::ToolResult(ToolResult {
                tool_use_id: format!("call-{i}"),
                tool_name: Some("read".to_string()),
                content: vec![ContentPart::Text(TextContent {
                    text: "line of file content ".repeat(20),
                    cache_control: None,
                })],
                structured_content: None,
                is_error: None,
                metadata: serde_json::Map::new(),
                truncated_by: None,
            })),
        ));
        messages.push(Message::assistant(format!(
            "Summarised step {i}. {}",
            "summary ".repeat(10)
        )));
    }
    messages
}

/// A tool catalog with the three exposure levels represented, as the
/// server's registry produces them.
fn tool_descriptors(count: usize) -> Vec<ToolDescriptor> {
    (0..count)
        .map(|i| ToolDescriptor {
            name: format!("tool_{i}"),
            description: format!(
                "Tool number {i} does a thing. {}",
                "detail ".repeat(8)
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer" },
                },
                "required": ["path"],
            }),
            category: ToolCategory::Utility,
            is_hidden: false,
            exposure: if i % 10 == 3 {
                ToolExposure::Deferred
            } else if i % 10 == 7 {
                ToolExposure::Hidden
            } else {
                ToolExposure::Direct
            },
            annotations: None,
        })
        .collect()
}

/// A nested tool-argument payload: about 64 bytes per entry, so the
/// bench's `nested_tool_args(120)` is ≈7.5 KB (7,701 bytes), not the
/// "4 KB" the first version of this fixture was labelled with.
fn nested_tool_args(depth: usize) -> String {
    let mut out = String::from("{");
    for i in 0..depth {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "\"key_{i}\":{{\"nested\":{{\"value\":\"{}\",\"index\":{i}}}}}",
            "x".repeat(20)
        ));
    }
    out.push('}');
    out
}

// ---------------------------------------------------------------------
// One whole turn: a scripted provider that asks for one tool and then
// answers, a registry large enough to make the tool-surface projection
// real work, and a spawner that does not need a runtime (the loop
// detaches its run through `Spawner`).
// ---------------------------------------------------------------------

/// Runs detached tasks on a small pooled executor—closer to a real
/// deployment than a fresh OS thread per turn, and still not tokio.
struct ThreadSpawner {
    pool: futures::executor::ThreadPool,
}

impl ThreadSpawner {
    fn new() -> Self {
        Self {
            pool: futures::executor::ThreadPool::new().expect("thread pool"),
        }
    }
}

impl uva_core::spawn::Spawner for ThreadSpawner {
    fn spawn(&self, task: uva_core::spawn::BoxFuture<()>) {
        self.pool.spawn_ok(task);
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        std::thread::spawn(f);
    }
}

/// Provider that asks for `echo` on the first call and answers on the
/// second—one tool round trip per turn.
struct ScriptedProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl uva_provider::ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted-bench"
    }

    fn model_config(&self) -> uva_provider::ModelConfig {
        uva_provider::ModelConfig {
            name: "scripted-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: uva_provider::CompletionRequest,
    ) -> Result<uva_provider::CompletionResponse, uva_core::Error> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let content = if call.is_multiple_of(2) {
            Content::parts(vec![ContentPart::ToolUse(uva_provider::ToolUse {
                id: format!("call-{call}"),
                name: "echo".to_string(),
                input: serde_json::json!({ "text": "bench" }),
            })])
        } else {
            Content::text("done")
        };
        Ok(uva_provider::CompletionResponse {
            content,
            ..uva_provider::CompletionResponse::default()
        })
    }
}

/// A registry with `echo` plus a realistic number of other tools, so
/// projection and `descriptors()` do representative work.
fn tool_registry(count: usize) -> uva_tool::ToolRegistry {
    let registry = uva_tool::ToolRegistry::new();
    for i in 0..count {
        let name = if i == 0 {
            "echo".to_string()
        } else {
            format!("tool_{i}")
        };
        registry.register_entry(uva_tool::ToolEntry::new(Arc::new(
            uva_test_support::FakeTool::new(&name, "ok"),
        )));
    }
    registry
}

// ---------------------------------------------------------------------
// Bench helpers (Step 4 / 5 / 6)
//
// Each helper below stands in for one shape the bench
// reports on. They are *bench*, not production: correctness
// is whatever shape matches the plan, and the projection
// answer is asserted by the matching unit test, not the
// bench. They are deliberately cheap to build once and
// share between rows so the table is comparable run-to-run.
// ---------------------------------------------------------------------

fn build_harness_registry(count: usize) -> Arc<uva_tool::ToolRegistry> {
    Arc::new(tool_registry(count))
}

fn scripted_turn_agent() -> uva_harness::ReActAgent {
    let provider: Arc<dyn uva_provider::ModelProvider> =
        Arc::new(ScriptedProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
    uva_harness::ReActAgent::new(provider, Arc::new(tool_registry(40)))
        .with_workspace(".")
        .with_spawner(
            Arc::new(ThreadSpawner::new()) as Arc<dyn uva_core::spawn::Spawner>
        )
        .with_max_iterations(4)
        .with_name("bench")
}

/// Drive one turn to completion and return the number of events seen.
fn run_one_turn(agent: &uva_harness::ReActAgent) -> usize {
    run_turn(
        agent,
        uva_harness::AgentInput::text("echo something, then answer"),
    )
}

/// Drive one turn with a caller-built input.
fn run_turn(
    agent: &uva_harness::ReActAgent,
    input: uva_harness::AgentInput,
) -> usize {
    use uva_core::{AtomicCancelToken, CancelToken};
    use uva_harness::Agent as _;

    let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();
    let mut stream = futures::executor::block_on(agent.run(input, cancel));
    let mut events = 0usize;
    futures::executor::block_on(async {
        while futures::StreamExt::next(&mut stream).await.is_some() {
            events += 1;
        }
    });
    events
}

// ---------------------------------------------------------------------
// Real-harness rows (V5 closure round)
//
// The rows above measure a *shape*: a token estimator, a projection
// helper, a truncation primitive. Those are still worth a row each,
// but they are not the turn.
//
// The rows below drive the shipping loop through its public API —
// `ReActAgent::new(...).run(AgentInput, cancel)` — so the number
// they report is the cost a real turn pays, not the cost of a
// re-implementation that happens to live in this file. Each one has a
// `--check` gate in `check_ratios` with the same discipline as the
// rows above it: two measurements taken in this process, one factor,
// one floor derived from the observed minimum.
//
// Four scenarios, one per gap the closure round set out to measure:
//
// | row   | isolates                                          |
// |-------|---------------------------------------------------|
// | R1    | G1 — the per-turn transcript copy                 |
// | R2    | G3 — the tool batch's concurrency                 |
// | R3    | the host's per-dispatch hook overhead             |
// | R4    | G6 — the over-window context prune                |
// ---------------------------------------------------------------------

/// A provider that asks for `count` tools in its first response and
/// answers in its second — one batch, one round trip.
struct BatchProvider {
    count: usize,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl uva_provider::ModelProvider for BatchProvider {
    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "batch-bench"
    }

    fn model_config(&self) -> uva_provider::ModelConfig {
        uva_provider::ModelConfig {
            name: "batch-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: uva_provider::CompletionRequest,
    ) -> Result<uva_provider::CompletionResponse, uva_core::Error> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Every turn is one batch round trip: the tool batch, then the
        // answer. The parity (not a one-shot) is what makes the second
        // turn of a measured run ask for the batch again — a provider
        // that only batched on its first call would leave every later
        // iteration of the measurement with nothing to dispatch.
        let content = if call.is_multiple_of(2) {
            Content::parts(
                (0..self.count)
                    .map(|i| {
                        ContentPart::ToolUse(uva_provider::ToolUse {
                            id: format!("batch-{i}"),
                            name: format!("bench_{i}"),
                            input: serde_json::json!({}),
                        })
                    })
                    .collect(),
            )
        } else {
            Content::text("done")
        };
        Ok(uva_provider::CompletionResponse {
            content,
            ..uva_provider::CompletionResponse::default()
        })
    }
}

/// An observer that decides nothing. The cost the R3 row measures is
/// the *dispatch* — the read lock, the `Arc` clone and the call — not
/// the observer's own work, so the observer does no work.
struct EmptyHook;

#[async_trait::async_trait]
impl uva_harness::Hook for EmptyHook {
    fn name(&self) -> &str {
        "empty"
    }

    async fn observe(
        &self,
        _event: &uva_steering::HookEvent,
        _ctx: &uva_harness::HookContext<'_>,
    ) -> uva_steering::HookDecision {
        uva_steering::HookDecision::Allow
    }
}

/// A tracker that caps the batch's concurrency.
struct CappedTracker {
    cap: usize,
}

impl uva_steering::tracker::Tracker for CappedTracker {
    fn on_iteration(&self, _state: &AgentState) {}

    fn on_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _state: &AgentState,
    ) {
    }

    fn on_llm_response(
        &self,
        _usage: &uva_provider::TokenUsage,
        _state: &AgentState,
    ) {
    }

    fn recommended_concurrency(&self, _state: &AgentState) -> usize {
        self.cap
    }
}

/// A tool whose call only completes after a worker thread has waited
/// out a fixed delay.
///
/// The delay is what makes the batch's concurrency *observable*: on
/// the bench's single-threaded executor two calls that never yield
/// cost the same whether they run together or one after the other,
/// so a concurrency cap would be invisible without a call that
/// genuinely takes time. The workers are a thread pool, so the
/// unbounded batch really does overlap.
struct SlowTool {
    name: String,
    delay: Duration,
    workers: Arc<futures::executor::ThreadPool>,
}

#[async_trait::async_trait]
impl uva_tool::Tool for SlowTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "a tool that takes a fixed amount of wall time"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &uva_tool::Context,
    ) -> uva_tool::ToolOutput {
        let (tx, rx) = futures::channel::oneshot::channel();
        let delay = self.delay;
        self.workers.spawn_ok(async move {
            std::thread::sleep(delay);
            let _ = tx.send(());
        });
        let _ = rx.await;
        uva_tool::ToolOutput::text("ok")
    }
}

/// The registry the R2 row drives: `count` tools that each take
/// `delay` of wall time.
fn slow_tool_registry(
    count: usize,
    delay: Duration,
    workers: Arc<futures::executor::ThreadPool>,
) -> uva_tool::ToolRegistry {
    let registry = uva_tool::ToolRegistry::new();
    for i in 0..count {
        registry.register_entry(uva_tool::ToolEntry::new(Arc::new(SlowTool {
            name: format!("bench_{i}"),
            delay,
            workers: Arc::clone(&workers),
        })));
    }
    registry
}

/// The agent the R2 row drives, with the batch's concurrency capped
/// when `cap` is `Some`.
fn batch_agent(
    count: usize,
    registry: uva_tool::ToolRegistry,
    cap: Option<usize>,
    registrations: &mut Vec<uva_harness::Registration>,
) -> uva_harness::ReActAgent {
    let provider: Arc<dyn uva_provider::ModelProvider> =
        Arc::new(BatchProvider {
            count,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
    let mut host = uva_harness::Host::new();
    if let Some(cap) = cap {
        registrations.push(host.tracker(Arc::new(CappedTracker { cap })));
    }
    uva_harness::ReActAgent::new(provider, Arc::new(registry))
        .with_workspace(".")
        .with_spawner(
            Arc::new(ThreadSpawner::new()) as Arc<dyn uva_core::spawn::Spawner>
        )
        .with_max_iterations(4)
        .with_host(host)
        .expect("the bench host binds no conflicting tools")
        .with_name("bench-batch")
}

/// The agent the R3 row drives, with `hooks` observers installed.
fn hooked_agent(
    hooks: usize,
    registrations: &mut Vec<uva_harness::Registration>,
) -> uva_harness::ReActAgent {
    let provider: Arc<dyn uva_provider::ModelProvider> =
        Arc::new(ScriptedProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
    let mut host = uva_harness::Host::new();
    for _ in 0..hooks {
        registrations.push(host.hook(Arc::new(EmptyHook)));
    }
    uva_harness::ReActAgent::new(provider, Arc::new(tool_registry(40)))
        .with_workspace(".")
        .with_spawner(
            Arc::new(ThreadSpawner::new()) as Arc<dyn uva_core::spawn::Spawner>
        )
        .with_max_iterations(4)
        .with_host(host)
        .expect("the bench host binds no conflicting tools")
        .with_name("bench-hooked")
}

/// The R1 input: a turn whose prior transcript is `history`.
fn history_input(history: Vec<Message>) -> uva_harness::AgentInput {
    let mut input =
        uva_harness::AgentInput::text("echo something, then answer");
    input.history = history;
    input
}
