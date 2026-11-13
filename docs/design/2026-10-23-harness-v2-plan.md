# Harness V2 — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move seq numbering from the persistence sink into a harness-owned `LogWriter`, so the agent loop can append model-visible rows without blocking on disk while every row still carries an authoritative, gap-free ordinal.

**Architecture:** `LogWriter` lives in the harness as a private `log/` module. It owns a monotonic `AtomicU64`, stamps rows before they enter a bounded channel (backpressure, never drop), and exposes `flush()` — the only durability promise — which the loop calls once per step boundary. `SessionSink` keeps persisting rows but stops assigning ordinals.

**Tech Stack:** Rust 1.98, edition 2024, `futures::channel::mpsc`, `async-trait`, `thiserror`. No new dependencies. No cargo features.

**Spec:** `docs/design/2026-10-23-harness-v2-design.md` (§4.2, §4.3, §4.6, §7, §10)

## Global Constraints

- Library crates use `thiserror` only. Never `anyhow` outside `uva-app-server`.
- Every dependency version is `major.minor` (e.g. `1.1`, never `1.1.0`).
- New deps must come from `[workspace.dependencies]` in the root `Cargo.toml`; crate manifests use `dep = { workspace = true }`.
- `max_width = 80`. No wildcard imports (`use foo::*`).
- No function over 100 lines, no nesting over 4 levels, in `uva-harness` production code (`scripts/harness-shape/clippy.toml`).
- No `chrono::Utc::now()` in library production code — use `uva_core::Clock`.
- No `#[cfg(feature = ...)]` anywhere in `uva-harness`. No tokio in its `[dependencies]`.
- `scripts/harness-docs/baseline` is **56** and current is **56** — zero slack. Private-item `///` comments must be deleted or the gate fails.
- Run `cargo +nightly fmt --all` after each task; then fix every warning.
- Commit after every task. Each task's commits must leave the workspace compiling.

## Review Focus

These are failure modes the spec implies that a consumer would experience as a bug. Each has a test in the owning task.

1. **A second appender on the same session drifts the ordinal.** `dispatch.rs:313` (Feedback rows) and `inner.rs:162` (shutdown marker) write straight to `session_store`, bypassing `RunLog`. If the new counter lives only in `RunLog`, their rows get ordinals that collide with the run's — and `try_fold_log_surface` then rejects compaction provenance for the whole session. → Task 3.
2. **Queue saturation silently drops a durable row.** The current `TypedEventSink::record` does `let _ = tx.try_send(..)`; a dropped row leaves a gap that permanently disables compaction with no error. → Task 2.
3. **The step boundary flush is skipped on the terminating iteration.** A run that ends via `Completed` must still flush, or the final `tool_result` is lost. → Task 4.
4. **`stamp()` and `append()` are called separately, so a panic between them burns an ordinal.** Harmless for correctness (ordinals need only be gap-free in the log, and a burned ordinal never appears in a read), but the ledger must not record it. → Task 2.
5. **Compaction is in flight when the flush happens.** `Compaction.source_event_seqs` cites ordinals stamped but not yet durable. The citation is only legal after flush — otherwise the fold sees a prefix that does not exist. → Task 4.

---

## File Structure

**Created:**

- `crates/uva-harness/src/log/mod.rs` — `LogWriter`, `Durability`, `StampedRow`. The single append path.
- `crates/uva-harness/src/log/queue.rs` — bounded channel + backpressure sender.
- `crates/uva-harness/src/log/tests.rs` — `LogWriter` unit tests (reached via `#[cfg(test)] mod tests;` in `log/mod.rs`).

**Modified:**

- `crates/uva-harness/src/lib.rs` — declare `mod log;` (private, not `pub`).
- `crates/uva-harness/src/agent/sink.rs` — `EventSink::emit_typed` routes through `LogWriter`.
- `crates/uva-harness/src/agent/caps.rs` — `EnvCaps` gains `log_writer` / `durability`.
- `crates/uva-harness/src/agent/re_act/loop/finalize.rs` — call `flush()` at the step boundary.
- `crates/uva-session/src/sink.rs` — `append` signature `Result<u64, _>` → `Result<(), _>`; `alloc_seq()` added.
- `crates/uva-session/src/jsonl.rs`, `in_memory.rs` — implement the new signature.
- `crates/uva-app-server/src/session/controller/run_log.rs` — take ordinal from `alloc_seq()`.
- `crates/uva-app-server/src/session/controller/dispatch.rs`, `inner.rs` — route bypass appenders through the shared counter.

**Deleted** (Task 1, the documented dead set):
`ReActAgent::{with_run_inbox, run_inbox, with_clock, with_tool_registry, with_model_hint}`, `MpscInbox`, `RunInbox`, `RunInboxHandle`, `AgentRegistry::is_empty`, `Checkpointer::list_keys`, `InMemoryCheckpointer::new`, `CheckpointSnapshot::at`, `StrategyRegistry::names`, `AgentInput::to_message`, `SessionEndReason::as_str`.

`AgentCaps::defaults` is **kept and made private** — it has two crate-internal callers.

`crates/uva-harness/src/agent/re_act/tests/inbox_tests.rs` (1,009 lines) is deleted with `RunInbox`.

---

## Task 1: Delete the dead public surface

Frees the comment budget that `log/` will consume (Global Constraints: baseline 56, current 56). Must land first.

**Files:**

- Modify: `crates/uva-harness/src/agent/re_act/agent.rs`, `agent/run.rs`, `agent/registry/mod.rs`, `agent/checkpoint.rs`, `agent/strategy/mod.rs`, `input.rs`, `events/mod.rs`, `agent/caps.rs`, `agent/mod.rs`, `lib.rs`
- Delete: `crates/uva-harness/src/agent/re_act/tests/inbox_tests.rs`

**Interfaces:**

- Consumes: nothing.
- Produces: nothing. Later tasks depend only on the freed comment budget and on `cargo check` staying green.

- [ ] **Step 1: Re-verify each item is still unreferenced before deleting**

Run:

```bash
cd /home/crochee/workspace/uva
for s in with_run_inbox run_inbox with_clock with_tool_registry with_model_hint \
         AgentRegistry::is_empty list_keys CheckpointSnapshot::at \
         StrategyRegistry::names to_message as_str; do
  n=$(grep -rn "\b$s\b" --include='*.rs' crates/ \
      | grep -v '^crates/uva-harness/src/' | wc -l)
  echo "$s: $n"
done
```

Expected: `0` for every line. Two need care — `with_clock` has hits that are
`SchedulerTool::with_clock` (a different type) and `with_tool_registry` has one
doc mention in `uva-tool/src/restriction.rs:166`. If either count is nonzero,
**stop** and re-read the hit before deleting.

- [ ] **Step 2: Delete the items and their re-exports**

Remove the method bodies from `agent/re_act/agent.rs` (`with_run_inbox`, `run_inbox`,
`with_clock`, `with_tool_registry`, `with_model_hint`) and `agent/run.rs` (`MpscInbox`,
`RunInbox`, `RunInboxHandle` plus their `EnvCaps` field). Remove
`is_empty` (`agent/registry/mod.rs:104`), `list_keys` (`agent/checkpoint.rs:71`),
`InMemoryCheckpointer::new` (`agent/checkpoint.rs:84`), `CheckpointSnapshot::at`
(`agent/checkpoint.rs:27`), `StrategyRegistry::names` (`agent/strategy/mod.rs:167`),
`AgentInput::to_message` (`input.rs:106`), `SessionEndReason::as_str`
(`events/mod.rs:272`). Strip the matching names from the `pub use` blocks in
`agent/mod.rs` and `lib.rs`.

Change `AgentCaps::defaults` (`agent/caps.rs:70`) from `pub fn` to
`pub(crate) fn` — two internal callers remain.

- [ ] **Step 3: Delete `inbox_tests.rs` and its `mod` declaration**

`crates/uva-harness/src/agent/re_act/tests/inbox_tests.rs` (1,009 lines) exists only
to test the removed `RunInbox`. Delete the file and its declaration in
`tests/mod.rs`.

- [ ] **Step 4: Fix call sites that used the removed setters**

`agent/caps.rs` drops the `inbox: Option<Arc<dyn RunInbox>>` field from `EnvCaps`.
`EventSink` construction in `agent/run.rs` loses its inbox parameter. Compile and fix.

- [ ] **Step 5: Build and run the gates**

Run:

```bash
cargo build -p uva-harness && cargo test -p uva-harness --lib
make check-harness-docs
```

Expected: build clean, tests pass, and `check-harness-docs` reports **strictly below 56**.

- [ ] **Step 6: Commit**

```bash
git add -A crates/uva-harness
git commit -m "refactor(harness): delete dead public surface and RunInbox"
```

---

## Task 2: `LogWriter` with backpressure and a harness-owned counter

**Files:**

- Create: `crates/uva-harness/src/log/mod.rs`, `log/queue.rs`, `log/tests.rs`
- Modify: `crates/uva-harness/src/lib.rs` (add `mod log;`, private)

**Interfaces:**

- Consumes: `uva_session::{SessionEvent, SessionSink, SessionError, SessionSnapshot}`.
- Produces:

  ```rust
  // log/mod.rs
  pub(crate) enum Durability { StepBoundary, Batched { flush_rows: usize, flush_every: Duration },
                               Sync, Off }

  pub(crate) struct StampedRow { pub seq: u64, pub event: SessionEvent }

  pub(crate) struct LogWriter { /* … */ }
  impl LogWriter {
      pub(crate) fn new(sink: Arc<dyn SessionSink>, durability: Durability) -> Self;
      /// Monotonic ordinal. Synchronous, never blocks. Single source of seq truth.
      pub(crate) fn stamp(&self) -> u64;
      /// Enqueue an already-stamped row. Suspends the caller when the queue is full.
      pub(crate) async fn append(&self, row: StampedRow);
      /// The only durability promise. Collects the channel into one `append_many`.
      pub(crate) async fn flush(&self);
      /// Resume-side repair: append closers for unterminated work.
      pub(crate) async fn repair(&self, closers: Vec<SessionEvent>);
      pub(crate) fn durability(&self) -> Durability;
  }
  ```

  `log/queue.rs` provides the bounded sender whose `send` yields rather than drops.

- [ ] **Step 1: Write the failing test for gap-free saturation**

In `crates/uva-harness/src/log/tests.rs`:

```rust
#[tokio::test]
async fn a_saturated_queue_never_drops_a_durable_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let w = LogWriter::with_capacity(sink.clone(), 1);
    for i in 0..64u64 {
        w.append(StampedRow { seq: w.stamp(), event: usage_row(i) }).await;
    }
    w.flush().await;
    let rows = sink.read().await.unwrap();
    let seqs: Vec<u64> = rows.iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (1..=64).collect::<Vec<_>>());
}
```

`usage_row` is a local helper building `SessionEvent::Usage`. The capacity-1 queue
forces the slow path on every iteration after the first — this is the regression test
for Review Focus #2. The loop `await`s `append`, which is the point: backpressure, not
dropping.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p uva-harness --log 2>/dev/null; cargo test -p uva-harness saturated -- --nocapture`

Expected: FAIL — `LogWriter` does not exist yet.

- [ ] **Step 3: Write the ordinal-is-not-sink's failing test**

```rust
#[tokio::test]
async fn stamp_and_append_are_independent_of_sink_ordering() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let w = LogWriter::new(sink.clone(), Durability::Off);
    let a = StampedRow { seq: w.stamp(), event: usage_row(1) };
    let b = StampedRow { seq: w.stamp(), event: usage_row(2) };
    w.append(b).await;
    w.append(a).await;
    w.flush().await;
    let seqs: Vec<u64> = sink.read().await.unwrap()
        .iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2], "read order follows the stamped ordinal, not arrival");
}
```

This is spec §10 test 6 — it is what makes "the sink no longer numbers rows" true.

- [ ] **Step 4: Implement `log/queue.rs`**

A bounded `futures::channel::mpsc` sender plus an `AtomicU64` for stamped rows. The
send path must use `Sender::send().await` (yields under pressure) and **must not**
`try_send`. Write a private-item doc on the queue type recording why: `try_send` is
what produced the silent gap, and the choice is a durability invariant, not a
performance tweak.

- [ ] **Step 5: Implement `log/mod.rs`**

`LogWriter` holds `Arc<dyn SessionSink>`, the queue sender, the `AtomicU64`, and a
buffered `Vec<StampedRow>` for pending flush. `append` enqueues. `flush` drains the
channel into the buffer and issues one `sink.append_many(..)` — a single fsync.
`Durability::Off` skips the sink call. `Durability::Sync` calls `append` per row.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p uva-harness --lib log`

Expected: PASS, both tests green.

- [ ] **Step 7: Commit**

```bash
git add -A crates/uva-harness
git commit -m "feat(harness): LogWriter with backpressure and harness-owned seq"
```

---

## Task 3: Move numbering out of `SessionSink`, and unify every appender

The atomic cutover. **This task must land as one commit** — a tree where the sink and
the new writer both number rows produces duplicate ordinals and silently breaks
compaction provenance.

**Files:**

- Modify: `crates/uva-session/src/sink.rs`, `jsonl.rs`, `in_memory.rs`
- Modify: `crates/uva-app-server/src/session/controller/run_log.rs`, `dispatch.rs`, `inner.rs`
- Modify: `crates/uva-harness/src/agent/caps.rs`, `agent/sink.rs`
- Test: `crates/uva-session/src/sink.rs` inline tests; `crates/uva-app-server/src/session/controller/tests/`

**Interfaces:**

- Consumes: `uva_session::SessionSink` as it exists today.
- Produces:

  ```rust
  // uva-session/src/sink.rs — the trait after this task
  pub trait SessionSink: Send + Sync {
      /// Allocate the next ordinal. Synchronous, monotonic, gap-free.
      /// The sink retains the counter so that callers which bypass
      /// `RunLog` still draw from the same sequence.
      fn alloc_seq(&self) -> u64;
      /// Persist a row whose `seq` the caller already stamped.
      /// No longer returns an ordinal.
      async fn append(&self, event: &Value) -> Result<(), SessionError>;
      async fn read(&self) -> Result<Vec<Value>, SessionError>;
      // … unchanged: read_from, snapshot, close, append_with_key, append_many
  }
  ```

  `uva-harness::agent::EnvCaps` gains `log_writer: Option<Arc<LogWriter>>` and
  `durability: Durability`.

- [ ] **Step 1: Write the failing test for Review Focus #1 — the bypass appender**

This is the constraint the design doc did not record: `dispatch.rs:313` and
`inner.rs:162` append straight to `session_store`, deliberately bypassing `RunLog`
(whose own comment at `run_log.rs:56` says a local counter drifts because of them).
A counter inside `RunLog` alone therefore collides. The fix is that the counter
lives in the **sink**, and every appender draws from it.

In `crates/uva-app-server/src/session/controller/tests/`, add:

```rust
#[tokio::test]
async fn bypass_appenders_draw_from_the_same_sequence_as_the_run_log() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let mut log = RunLog::new(sink.clone(), ledger(), sink.alloc_seq());
    let first = log.append(&json!({"kind": "a"})).await.unwrap();
    // dispatch.rs's feedback path writes here, not through RunLog:
    sink.append(&json!({"kind": "feedback"})).await.unwrap();
    let third = log.append(&json!({"kind": "c"})).await.unwrap();
    assert_eq!((first, third), (1, 3), "the bypassed row consumed ordinal 2");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p uva-app-server bypass_appenders`

Expected: FAIL — `RunLog::new` does not take a starting ordinal, and `append` returns
the sink-assigned value rather than a caller-stamped one.

- [ ] **Step 3: Change the `SessionSink` trait**

In `sink.rs`, add `fn alloc_seq(&self) -> u64;` and change
`async fn append(&self, event: &Value) -> Result<u64, SessionError>` to
`-> Result<(), SessionError>`. Update `append_with_key` and `append_many` accordingly
— they no longer report an ordinal; `append_many` returns `Result<(), SessionError>`.

Move the ordinal counter into the sink implementation: `JsonlSessionSink` and
`InMemorySessionSink` each hold `AtomicU64` next to the state they already guard.
Delete the per-row reservation the sink used to do.

- [ ] **Step 4: Update the harness `LogWriter` to draw from `alloc_seq`**

`LogWriter::stamp` becomes `self.sink.alloc_seq()`; drop its own `AtomicU64`. This is
what makes Review Focus #1 hold for the harness path too. Update Task 2's tests to
construct the writer against a sink rather than a local counter.

- [ ] **Step 5: Update `RunLog` to take a starting ordinal**

`RunLog::new(store, ledger, next_seq)` takes the current tail from
`store.alloc_seq()`; its `append` stamps the row with that value before calling
`sink.append`, records the ledger entry, and returns the ordinal it used. Rewrite the
doc comment on `RunLog::append` — the existing one argues the sink is the *only* thing
that can report the ordinal, which is no longer the reasoning; the counter is the sink's,
but the caller stamps.

- [ ] **Step 6: Route `persist_and_broadcast`'s SSE cursor through the stamp**

`append_if_durable` (`persist.rs:181`) currently returns the sink's `stream_index`.
It returns the ordinal `RunLog::append` stamped instead. `last_stream_index` semantics
are unchanged.

- [ ] **Step 7: Build the whole workspace and fix every call site**

Run: `cargo build --workspace`

Expect breakage in `uva-app-server`, the strategy crates, and any example that
implements `SessionSink` — including
`crates/uva-session/examples/compaction_checkpoint.rs:84`. Fix each.

- [ ] **Step 8: Run the tests**

Run:

```bash
cargo test -p uva-session --lib
cargo test -p uva-app-server --lib
cargo test -p uva-harness --lib
```

Expected: all PASS.

- [ ] **Step 9: Commit — one commit, this is the atomic cutover**

```bash
git add -A
git commit -m "refactor(session): number rows via alloc_seq, not the sink's append return"
```

---

## Task 4: Flush at the step boundary

**Files:**

- Modify: `crates/uva-harness/src/agent/re_act/loop/finalize.rs`, `loop/mod.rs`
- Modify: `crates/uva-harness/src/agent/sink.rs` (`emit_typed` routes through `LogWriter`)
- Test: `crates/uva-harness/src/log/tests.rs`, `agent/re_act/tests/lifecycle/`

**Interfaces:**

- Consumes: `LogWriter` from Task 2/3; `EnvCaps.log_writer`.
- Produces: `EventSink::emit_typed(&self, event: SessionEvent)` stamps and enqueues
  instead of calling `TypedEventSink::record`. `TypedEventSink` and
  `TypedEventReceiver` become unused by the harness and are deleted in this task.

- [ ] **Step 1: Write the failing test for Review Focus #3 — the terminating flush**

In `agent/re_act/tests/lifecycle/`:

```rust
#[tokio::test]
async fn a_completed_run_flushes_its_final_tool_result() {
    let (sink, provider) = one_tool_call_then_final_answer();
    let log = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(log.clone(), Durability::StepBoundary);
    let agent = ReActAgent::new(provider, registry())
        .with_log_writer(writer.clone());
    let mut stream = agent.run(AgentInput::text("go"), cancel());
    while stream.next().await.is_some() {}
    assert!(log.read().await.unwrap().iter()
        .any(|r| r["type"] == json!("tool_result")));
}
```

Without the boundary flush this fails: the last row is still in the channel.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p uva-harness completed_run_flushes`

Expected: FAIL — no `with_log_writer` exists.

- [ ] **Step 2b: Write the failing test for Review Focus #5 — compaction cites only durable ordinals**

```rust
#[tokio::test]
async fn compaction_cites_only_ordinals_that_are_durable() {
    let (sink, provider) = run_with_compaction(4);
    let log = Arc::new(InMemorySessionSink::new("s"));
    let agent = ReActAgent::new(provider, registry()).with_log_writer(
        LogWriter::new(log.clone(), Durability::StepBoundary));
    let mut stream = agent.run(AgentInput::text("go"), cancel());
    while stream.next().await.is_some() {}
    let rows = log.read().await.unwrap();
    let durable: std::collections::HashSet<u64> =
        rows.iter().filter_map(|r| r["seq"].as_u64()).collect();
    for r in rows.iter().filter(|r| r["type"] == json!("compaction")) {
        for cited in r["data"]["source_event_seqs"].as_array().unwrap() {
            assert!(durable.contains(&cited.as_u64().unwrap()),
                    "compaction cites ordinal {cited} that is not in the durable log");
        }
    }
    assert!(try_fold_log_surface(&rows).is_ok(), "the log still folds");
}
```

This is the property that the current `try_send`-and-drop bug silently destroys.

- [ ] **Step 3: Add `with_log_writer` and route `emit_typed`**

`EnvCaps` gains `log_writer: Option<Arc<LogWriter>>` and `durability: Durability`.
`ReActAgent::with_log_writer(Arc<LogWriter>)` installs it. `EventSink::emit_typed`
calls `stamp()` then `append(..).await`. Delete `EventSink::has_typed` and the
`typed_sink` field; delete `TypedEventSink` / `TypedEventReceiver` and the
`with_typed_event_sink` setter, along with `uva-session/src/typed_event_sink.rs`.

- [ ] **Step 4: Call `flush()` at the step boundary**

In `loop/mod.rs`, `drive_one_iteration` awaits `flush()` after the step's rows are
committed and before the next iteration begins. In `loop/finalize.rs`, `end()` awaits
`flush()` on every terminating path including `Completed` and `Failed`, so
Review Focus #3 holds. When `durability` is `Off`, skip.

- [ ] **Step 5: Delete the `try_send` path**

Confirm no `try_send` remains in `crates/uva-harness/src` or
`crates/uva-session/src/typed_event_sink.rs`'s dependents. Run:

```bash
grep -rn "try_send" crates/uva-harness/src crates/uva-session/src
```

Expected: no hits.

- [ ] **Step 6: Run the tests and the gates**

Run:

```bash
cargo test -p uva-harness --lib
make check-harness-docs && make check-public-api-runtime && make check-harness-features
```

Expected: all green. `check-harness-docs` must stay at or under 56 — if the new private
docs pushed it over, delete a stale private comment rather than raising the baseline.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(harness): flush at the step boundary; drop the drop-on-full path"
```

---

## Verification

Run after the final task:

```bash
cargo +nightly fmt --all
make ci
```

`make ci` must be green. Additionally:

- `cargo run --example runtime_agnostic -p uva-harness` completes a full turn on a
  non-tokio executor — the zero-runtime invariant.
- `cargo bench -p uva-harness` shows no regression against the pre-change baseline
  in `benches/hot_paths.rs`.

## Follow-on plans

This plan covers only the foundation (spec §4.2, §4.3, §4.6, §7). The remaining spec
sections become their own plans, in dependency order:

1. **Plugin system** — spec §5. `plugin/` module, T1–T3 tiers, `DelegatingTool`,
   `Host` registration, `TaskDelegator` migration.
2. **Loop derives from the fold** — spec §4.1. `RunState` becomes a read-only
   projection of `fold_surface`; drops its mutable fields.
3. **Suspension and resume** — spec §6. `Suspension` / `ResumeToken`, and the
   `Agent::run` signature change that ripples through the three
   `uva-strategy-*` crates.
