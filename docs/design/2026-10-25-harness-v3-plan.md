# Harness V3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `uva-harness` 变成“日志真源 + 可撤销接缝 + 按调用高风险审批”的薄循环，并让 `uva-app-server` 端到端满足完整 agent 对话的验收剧本。

**Architecture:** 模型历史由 `uva-session` 的 `fold_surface` 派生；harness 只保留循环与接缝，所有插件注册返回可撤销句柄；审批由循环内的 `ApprovalPolicy` 在派发前强制，挂起写显式日志行，resume 由日志派生；`uva-app-server` 用 `follow` 基线、`resume`、`compact` 和会话级 usage 暴露这套语义。

**Tech Stack:** Rust 1.98、edition 2024、`futures`、`serde_json`、`thiserror`（库）/`anyhow`（server）。不新增依赖，不新增 cargo feature。

**Spec:** `docs/design/2026-10-25-harness-v3-design.md`
**Supporting evidence:** `docs/research/harness-comparison-pi-dsh-2026-10-25.md`、`docs/research/agent-conversation-experience-2026-10-25.md`

## Global Constraints

- 库 crate 只用 `thiserror`；`uva-app-server` 是 app crate，用 `anyhow`。
- 依赖版本一律 `major.minor`；新依赖只能来自根 `Cargo.toml` 的 `[workspace.dependencies]`。
- `max_width = 80`；禁通配符导入；`uva-harness` 生产代码无函数 >100 行、无嵌套 >4 层。
- 库代码不得直接 `chrono::Utc::now()`；墙钟走 `uva_core::Clock`，ID 走 `uva_core::IdGen`。
- 库 crate 的公开 API 不得出现具体运行时类型（`tokio::` 等）。
- `uva-harness` 不得有 `[features]` 表，不得依赖 tokio。
- 测试按 crate 分批执行，禁止 `cargo test --workspace`。
- 破坏性 API 一次切干净，不留 shim、别名或兼容分支。
- 每个任务结束：工作区能编译、`cargo +nightly fmt --all` 干净、该 crate 测试绿。
- 提交信息用英文祈使句。

## Review Focus

1. **审批通过后、工具副作用完成、结果行未落盘时崩溃。** 期望：resume 不重跑高风险工具，只写 `TOOL_OUTCOME_UNKNOWN`，模型看到未知结果。测试在 Task 7。
2. **steer 与 run 结算撞在同一时刻。** 期望：输入不丢，转成 queued turn；`Stop` 把未注入项退回队列。测试在 Task 4。
3. **挂起被 repair 误判为崩溃中断。** 期望：`turn/end{suspended}` 不被补 `TOOL_OUTCOME_UNKNOWN`，pending 审批仍在。测试在 Task 7。
4. **断线重连发生在 assistant 流式中途。** 期望：首帧基线整窗替换，durable 行替换 live 投影，不出现重复半段文本。测试在 Task 9。
5. **部署后 tool schema / prompt section 变化再 resume。** 期望：默认 `ConfigDrift` 阻断，`allow_drift` 才继续。测试在 Task 7。

---

## File Structure

**uva-session**

- Create `src/history.rs`：`derive_history`、`HistoryError`、行解析。
- Modify `src/events.rs`：`UserMessage.source`、审批/挂起/请求快照/插件事件变体与 builders。
- Modify `src/repair.rs`：识别 `turn/end{suspended}`。
- Modify `src/lib.rs`：导出 `derive_history`、`HistoryError`。
- Test `src/history/tests.rs` 或内联 `#[cfg(test)]`（<400 行）。

**uva-harness**

- Create `src/agent/history.rs`：`HistoryProjection`（seq 水位缓存）。
- Create `src/agent/host.rs`：`Host`、`Registration`、`PluginHandle`、能力 trait、`Plugin`。
- Create `src/agent/approval.rs`：`Risk`、`ApprovalPolicy`、`ApprovalRequest`、`ResumeToken`、`Suspension`。
- Modify `src/agent/re_act/loop/prepare.rs`：从日志派生历史。
- Modify `src/agent/re_act/loop/mod.rs`：step 边界刷新历史。
- Modify `src/agent/re_act/loop/tools/mod.rs`：gate 批次与高风险挂起。
- Rename `src/agent/re_act/synthetic.rs` → `delegation.rs`。
- Modify `src/input.rs`：`resume` 字段与 builder。
- Modify `src/events/mod.rs`：`SystemEvent::Suspended`、`SessionEndReason::{Suspended, ConfigDrift}`。
- Modify `src/lib.rs`、`src/agent/mod.rs`：导出。
- Tests `src/agent/re_act/tests/lifecycle/{steer,approval}.rs`。

**uva-steering**

- Modify `src/hook_map.rs`：`HookEvent::{BeforeStep, BeforeRequest}`、`HookDecision::{Ask, ReplaceParams, ReplaceResult}`。
- Modify `src/steering.rs` / `src/hook.rs`：派发返回决策，修注释漂移。

**uva-tool**

- Modify `src/traits.rs`：`Risk`、`Tool::risk(&self, input: &Value) -> Risk`。
- Modify `src/lib.rs`：导出 `Risk`。
- Modify `crates/uva-tool-shell/src/lib.rs`（或工具实现文件）：覆盖 `risk`。

**uva-app-server**

- Create `src/routes/follow.rs`：`follow` / `resume` / `compact` / `usage`。
- Modify `src/routes/chat.rs`：`delivery` 字段；`send_message` 的 steer 路径。
- Modify `src/session/controller/ops.rs`：`SessionState::AwaitingApproval`。
- Modify `src/session/controller/run_task.rs`：挂起时拒绝启动；steer 落盘。
- Modify `src/state/app_state/sessions.rs`：审批策略与 follow 基线。
- Test `src/routes/tests/follow.rs`、`src/routes/tests/approval.rs`。

**uva-web**

- Modify `src/api/chat-stream.ts`、`src/api/chat-message.ts`、`src/api/types.ts`。
- Modify `src/pages/ChatPage.tsx`。
- Create `src/components/chat/ApprovalCard.tsx`、`src/components/chat/SteerChip.tsx`、`src/components/chat/CompactionMarker.tsx`。
- Verify with `make check-web` + Playwright MCP（无 e2e 套件）。

**Docs**

- Create `docs/tutorial/complete-agent.md`。
- Modify `README.md`（教程入口）。

---

### Task 1: Remove dead public surface

**Files:**
- Modify: `crates/uva-harness/src/lib.rs`
- Modify: `crates/uva-harness/src/agent/re_act/agent.rs`
- Modify: `crates/uva-harness/src/agent/checkpoint.rs`
- Modify: `crates/uva-harness/src/agent/registry/mod.rs`
- Modify: `crates/uva-harness/src/agent/caps.rs`
- Test: shell assertions in the task

**Interfaces:**
- Consumes: nothing.
- Produces: a harness whose documented public list matches the code; no later task relies on removed names.

- [ ] **Step 1: Produce the candidate list**

Run:

```bash
grep -rn "with_run_inbox\|MpscInbox\|run_inbox\|with_clock\|with_tool_registry\|with_model_hint\|fn list_keys\|InMemoryCheckpointer::new\|fn names\|fn to_message\|fn as_str" crates/uva-harness/src
```

Expected: the command prints only private / crate-internal items. For each `pub` item, run:

```bash
grep -rn "<name>" crates --include='*.rs' | grep -v "crates/uva-harness/src"
```

Expected: no hits outside the harness. If a hit exists, keep the item and record it in the commit message.

- [ ] **Step 2: Record the removal checklist**

Write the verified names into the commit message body. The check is a shell assertion, not a source-text test:

```bash
for name in with_run_inbox MpscInbox with_tool_registry; do
  ! grep -rn "$name" crates/uva-harness/src
 done
```

- [ ] **Step 3: Delete the items and their tests**

Delete the re-exports, builder methods, fields, and any test-only modules that exist solely for them. Do not add `#[allow(dead_code)]`.

- [ ] **Step 4: Run the harness suite and doc budget**

Run:

```bash
cargo test -p uva-harness --lib
make check-harness-docs
make check-harness-features
make check-harness-shape
```

Expected: all pass; the docs baseline does not increase.

- [ ] **Step 5: Commit**

```bash
git add crates/uva-harness
git commit -m "refactor(harness): remove zero-reference public surface"
```

---

### Task 2: `derive_history` in `uva-session`

**Files:**
- Create: `crates/uva-session/src/history.rs`
- Modify: `crates/uva-session/src/lib.rs`
- Test: `crates/uva-session/src/history/tests.rs` or inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `surface::fold_surface(&[SessionEvent]) -> Result<FoldedSurface, FoldError>`.
- Produces:

```rust
pub struct HistoryError { /* Fold(FoldError) | Decode(serde_json::Error) */ }

/// Parse log rows (skipping metadata headers), fold the surface, and
/// deserialize each surface message into a provider `Message`.
pub fn derive_history(rows: &[Value]) -> Result<Vec<Message>, HistoryError>;
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn derive_history_replays_user_and_assistant_in_order() {
    let rows = vec![
        user_row(1, "hello"),
        assistant_row(2, "hi"),
    ];
    let history = derive_history(&rows).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].role, Role::Assistant);
}

#[test]
fn derive_history_applies_replace_provenance() {
    // user(1), assistant(2), compaction(3) replacing [0,2)
    let rows = vec![user_row(1, "a"), assistant_row(2, "b"), compaction_row(3, "summary", &[1, 2])];
    let history = derive_history(&rows).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text(), "summary");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p uva-session --lib derive_history`
Expected: FAIL with `derive_history` not found.

- [ ] **Step 3: Implement**

Parse each row with `serde_json::from_value::<SessionEvent>`, skipping rows that are metadata headers (reuse the existing helper used by `LogWriter::seed_ledger`). Call `fold_surface`. Deserialize each `FoldedSurface.messages` entry into `uva_provider::Message`. Map errors into `HistoryError`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p uva-session --lib derive_history`
Expected: PASS.

- [ ] **Step 5: Add the determinism test**

```rust
#[test]
fn derive_history_is_deterministic() {
    let rows = fixture_rows();
    assert_eq!(derive_history(&rows).unwrap(), derive_history(&rows).unwrap());
}
```

- [ ] **Step 6: Commit**

```bash
git add crates/uva-session
git commit -m "feat(session): derive model history from the log"
```

---

### Task 3: Loop reads the projection

**Files:**
- Create: `crates/uva-harness/src/agent/history.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/prepare.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/mod.rs`
- Modify: `crates/uva-harness/src/agent/re_act/state.rs`
- Modify: `crates/uva-harness/src/input.rs`
- Test: `crates/uva-harness/src/agent/re_act/tests/lifecycle/persistence.rs`

**Interfaces:**
- Consumes: `uva_session::derive_history(&[Value])`.
- Produces:

```rust
pub(crate) struct HistoryProjection {
    watermark: u64,
    messages: Vec<Message>,
}

impl HistoryProjection {
    pub(crate) fn from_rows(rows: &[Value]) -> Result<Self, HistoryError>;
    /// Returns true when the message list changed.
    pub(crate) fn refresh(&mut self, rows: &[Value]) -> Result<bool, HistoryError>;
    pub(crate) fn messages(&self) -> &[Message];
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_writer_backed_run_ignores_input_history() {
    // log has user(1)="from-log"; input.history says "from-input"
    // assert the first provider request contains "from-log"
}

#[tokio::test]
async fn conflicting_input_history_ends_the_run() {
    // log and input.history differ -> SessionEndReason::Error
}

#[tokio::test]
async fn a_writer_backed_run_does_not_duplicate_the_input() {
    // server already logged user(1); assert request 1 has one user message
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p uva-harness --lib a_writer_backed_run_ignores_input_history conflicting_input_history_ends_the_run a_writer_backed_run_does_not_duplicate_the_input`
Expected: FAIL (history still comes from `input.history`).

- [ ] **Step 3: Implement**

In `drive_setup`, when `caps.env.log_writer` is `Some`, read rows and build `HistoryProjection`; compare `input.history` (empty or equal) and fail closed on mismatch. Store the projection on `RunState`; `RunState.messages` becomes a clone for the current request. Remove the checkpoint's authority: it may pre-seed the projection only when its tail seq is not behind the log.

**Writer-backed runs do not append `input.content` again.** The host (server) writes the user turn before starting the run; the loop derives it from the log. `input.content` is used only when no writer is configured. Assert the first request contains the user message exactly once.

- [ ] **Step 4: Refresh at the step boundary**

In `drive_one_iteration`, before `sample`, call `projection.refresh(&writer.read().await?)`; if changed, rebuild `session.messages`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p uva-harness --lib`
Expected: PASS, including existing lifecycle tests.

- [ ] **Step 6: Commit**

```bash
git add crates/uva-harness
git commit -m "feat(harness): derive the transcript from the session log"
```

---

### Task 4: Steer at the step boundary

**Files:**
- Modify: `crates/uva-session/src/events.rs` (`UserMessage.source`)
- Modify: `crates/uva-session/src/typed_event_builders.rs`
- Modify: `crates/uva-harness/src/agent/history.rs` (refresh already covers it)
- Modify: `crates/uva-app-server/src/routes/chat.rs`
- Modify: `crates/uva-app-server/src/session/controller/run_task.rs`
- Test: `crates/uva-app-server/src/routes/tests/steer.rs`
- Test: `crates/uva-harness/src/agent/re_act/tests/lifecycle/steer.rs`

**Interfaces:**
- Consumes: `LogWriter::append_event(&SessionEvent) -> Result<u64, SessionError>`, `LogWriter::flush()`.
- Produces:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub source: Option<String>, // "steer" | None

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery { Queue, Steer }
```

- [ ] **Step 1: Write the failing tests**

Session backward compatibility:

```rust
#[test]
fn an_old_user_row_without_source_still_deserializes() {
    let row = json!({"type":"user_message","seq":1,"ts":"t","data":{...}});
    let event: SessionEvent = serde_json::from_value(row).unwrap();
    assert!(matches!(event, SessionEvent::UserMessage { source: None, .. }));
}
```

Harness:

```rust
#[tokio::test]
async fn a_steer_row_is_in_the_next_request_but_not_the_current_tool_batch() {
    // append steer after the tool call is dispatched, before the next sample
    // assert request 2 contains it; assert tool batch 1 still has one call
}
```

Server:

```rust
#[tokio::test]
async fn steer_is_durable_before_the_response() {
    // POST with delivery=steer, then read the log before the run advances
    // assert the user_message row is present
}
```

- [ ] **Step 2: Run them to verify they fail**

Run:

```bash
cargo test -p uva-session --lib an_old_user_row_without_source_still_deserializes
cargo test -p uva-harness --lib a_steer_row_is_in_the_next_request
cargo test -p uva-app-server --lib steer_is_durable_before_the_response
```

Expected: FAIL.

- [ ] **Step 3: Implement the durable input path**

In the session event, add `source`. In `send_message`, branch on `delivery`:

- `Queue`: existing controller queue.
- `Steer`: `writer.append_event(UserMessage { source: Some("steer"), .. })` then `writer.flush().await?`, then answer `200` — the durability promise is the flush, not the status code; both deliveries answer `200`, as the route's sibling endpoints do.

Do not add an in-memory steer queue. The loop's `HistoryProjection::refresh` is the only consumer.

The shared projection (`uva-session`'s `derive_history` / `surface_to_messages`) is **pairing-aware**: a steer that lands while a tool call is pending is held back until the matching tool results, so the transcript reads `[User, Assistant(tool_use), Tool(result), User(steer)]`. A user message between an assistant's tool call and its result is a transcript every provider rejects.

- [ ] **Step 4: Handle the settlement race**

In the controller, if the run is no longer running when the steer is appended, convert it to a queued turn and return `delivery: "queue"` in the response. A steer that no request carried stays in the log: the repair path closes the tool call it interrupted, and the message becomes the next turn's first user message.

The `steer_gate` serialises the decision against BOTH the run task's `Running` → `Idle` transition and the cancel path, so a steer and a cancel cannot interleave. "Live" means the run can still sample again: the run task flips to `Idle` as soon as its stream ends, before its flushes.

- [ ] **Step 5: Run the tests**

```bash
cargo test -p uva-session --lib user_message
cargo test -p uva-harness --lib steer
cargo test -p uva-app-server --lib steer
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/uva-session crates/uva-harness crates/uva-app-server
git commit -m "feat: add durable in-run steering"
```

---

### Task 5: Host, registrations, and hook decisions

**Files:**
- Create: `crates/uva-harness/src/agent/host.rs`
- Create: `crates/uva-harness/src/agent/approval.rs` (`Risk` re-export, `ApprovalRequest`)
- Modify: `crates/uva-tool/src/traits.rs`, `crates/uva-tool/src/lib.rs` (`Risk`)
- Modify: `crates/uva-steering/src/hook_map.rs`
- Modify: `crates/uva-steering/src/steering.rs`
- Modify: `crates/uva-harness/src/agent/re_act/agent.rs` (`with_host`)
- Modify: `crates/uva-harness/src/agent/caps.rs`
- Test: `crates/uva-harness/src/agent/host/tests.rs`

**Interfaces:**
- Consumes: existing `Steering`, `ToolRegistry`, `ToolAnnotations`.
- Produces:

```rust
pub enum Risk { Low, High, Unknown }

pub struct ApprovalRequest {
    pub id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments_hash: String,
    pub risk: Risk,
    pub agent: String,
}
```

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn install(&self, host: &mut Host) -> PluginHandle;
}

pub struct Host { /* ordered registrations */ }
pub struct PluginHandle { /* owns every Registration from one install */ }
pub struct Registration { /* unregister on drop */ }

impl Host {
    pub fn new() -> Self;
    pub fn install(&mut self, plugin: &dyn Plugin) -> PluginHandle;
    pub fn observe(&mut self, hook: Arc<dyn Hook>) -> Registration;
    pub fn gate(&mut self, gate: Arc<dyn Gate>) -> Registration;
    pub fn request_transformer(&mut self, t: Arc<dyn RequestTransformer>) -> Registration;
    pub fn result_transformer(&mut self, t: Arc<dyn ResultTransformer>) -> Registration;
    pub fn prompt_section(&mut self, s: PromptSection) -> Registration;
    pub fn tool(&mut self, tool: Arc<dyn Tool>) -> Registration;
    // `delegator` arrives with Task 6, together with the trait it accepts.
}
```

`uva-steering` owns the hook vocabulary, so it cannot name harness types. The decisions
therefore carry only what the steering layer can express; the harness owns the richer shapes:

```rust
// uva-steering
pub enum HookDecision {
    Allow,
    Block { reason: String },
    Ask { reason: String },
    ReplaceParams(RequestParams),
    Terminate { reason: String },
}
```

- `RequestParams` lives in `uva-steering` (it names only provider types: model, temperature,
  max_tokens, tool_choice, tools, and a read-only `&[Message]` view). It has **no** `system` field
  and the messages are not mutable — that is how "T2 cannot rewrite the system prompt" is
  guaranteed by the type, not by a convention.
- The approval *request* (call id, tool, arguments hash, risk) is a harness type: the gate site
  builds it from the tool call, so `HookDecision::Ask` carries a reason only.
- Tool-result rewriting is a harness-side trait (`ResultTransformer` takes `&mut ToolOutput`),
  not a steering decision variant — `uva-steering` does not depend on `uva-tool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn block_wins_over_ask_and_later_gates_do_not_run() { /* ... */ }

#[test]
fn a_panicking_gate_is_a_block() { /* ... */ }

#[test]
fn dropping_the_plugin_handle_unregisters_every_hook() { /* ... */ }

#[test]
fn unsupported_decision_at_a_point_is_a_noop_with_a_diagnostic() { /* ... */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p uva-harness --lib host::`
Expected: FAIL (module absent).

- [ ] **Step 3: Implement the registry and arbitration**

Registration order; `Block`/`Terminate` short-circuit; `Ask` collects; transformers chain. Gates fail closed on error/panic; observers/transformers fail open with a diagnostic. `Host` is per-agent; `ReActAgent::with_host` installs it into `AgentCaps.control`.

- [ ] **Step 4: Extend `HookEvent` / `HookDecision`**

Add `BeforeStep`, `BeforeRequest`; add the decision variants above. Dispatch through `Steering` must return the decision instead of `let _ =`.

- [ ] **Step 5: Run the tests**

```bash
cargo test -p uva-steering --lib
cargo test -p uva-harness --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/uva-steering crates/uva-harness
git commit -m "feat(harness): add reversible plugin host and hook arbitration"
```

---

### Task 6: `DelegatingTool`

**Files:**
- Rename: `crates/uva-harness/src/agent/re_act/synthetic.rs` → `delegation.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/tools/route.rs`
- Modify: `crates/uva-harness/src/agent/host.rs` (`Host::delegator`)
- Modify: `crates/uva-tool-task/src/**`
- Modify: `crates/uva-app-server/src/server/agent_assembly.rs` (or the assembly file)
- Test: existing `crates/uva-harness/src/agent/re_act/tests/synthetic_*`

**Interfaces:**
- Consumes: `DelegationCall`, `ToolContext`, `ExecutionMode`, `ToolAnnotations`.
- Produces:

```rust
pub trait DelegatingTool: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    fn annotations(&self) -> Option<ToolAnnotations> { None }
    fn execute(&self, call: DelegationCall<'_>) -> BoxFuture<'static, ToolOutput>;
}
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn delegator_is_found_by_name_not_by_linear_claims_scan() {
    // register two delegators; assert dispatch hits the named one
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p uva-harness --lib delegator_is_found_by_name`
Expected: FAIL.

- [ ] **Step 3: Implement**

Replace `claims()` scanning with a name-keyed map. Keep `emit` / `cancel` / `depth`; add `context`, `mode`, `annotations`. Migrate `uva-tool-task` in the same commit; update the server assembly.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p uva-harness --lib
cargo test -p uva-tool-task --lib
cargo test -p uva-app-server --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/uva-harness crates/uva-tool-task crates/uva-app-server
git commit -m "refactor(harness): replace SyntheticTool with DelegatingTool"
```

---

### Task 7: Per-call risk, suspension, and resume

**Files:**
- Modify: `crates/uva-tool/src/traits.rs`
- Modify: `crates/uva-tool/src/lib.rs`
- Modify: `crates/uva-tool-shell/src/**`
- Modify: `crates/uva-harness/src/agent/approval.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/tools/mod.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/mod.rs`
- Modify: `crates/uva-harness/src/events/mod.rs`
- Modify: `crates/uva-harness/src/input.rs`
- Modify: `crates/uva-session/src/events.rs`
- Modify: `crates/uva-session/src/repair.rs`
- Test: `crates/uva-harness/src/agent/re_act/tests/lifecycle/approval.rs`
- Test: `crates/uva-session/src/repair/tests.rs`

**Interfaces:**
- Consumes: `Risk`, `ApprovalRequest` (Task 5), `ToolAnnotations`, `LogWriter::flush`, `SessionEvent`.
- Produces:

```rust
pub trait Tool {
    fn risk(&self, input: &Value) -> Risk; // default from annotations
}

pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    pub ask_unknown: bool,
    pub overrides: BTreeMap<String, ApprovalMode>,
    pub grants: BTreeSet<GrantKey>,
}
pub enum ApprovalMode { AskHighRisk, Always, Never }

pub struct ResumeToken { /* last_seq + pending digest */ }

pub enum Suspension {
    AwaitingApproval { interrupts: Vec<ApprovalRequest>, resume: ResumeToken },
    Paused { resume: ResumeToken },
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn ls_is_low_risk_and_rm_is_high_risk() { /* ShellTool::risk */ }

#[test]
fn read_only_tools_are_never_asked_about() { /* policy */ }

#[tokio::test]
async fn suspension_closes_the_turn_with_suspended_not_interrupted() { /* repair */ }

#[tokio::test]
async fn a_crash_after_a_destructive_side_effect_does_not_reexecute() { /* outcome unknown */ }

#[tokio::test]
async fn resume_must_cover_every_open_interrupt() { /* reject partial */ }

#[tokio::test]
async fn a_changed_tool_schema_blocks_resume_with_config_drift() { /* allow_drift */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p uva-harness --lib approval::`
Expected: FAIL.

- [ ] **Step 3: Implement risk and policy**

Add `Risk` and `Tool::risk`; `ShellTool` classifies argv. Add `ApprovalPolicy` and load it from agent config in the server later.

- [ ] **Step 4: Implement suspension**

In `execute_tools`, run the gate phase on the whole batch first. Execute `Low`; collect `High`. If any high-risk remains, append `approval/asked` + `turn/suspended`, `flush` synchronously, emit `SystemEvent::Suspended`, and end the run with `SessionEndReason::Suspended`.

New `SessionEvent` variants (log-only unless stated; each carries `seq` and `ts` like the existing variants):

```rust
ApprovalAsked { id, call_id, tool, arguments_hash, risk, agent }
ApprovalDecided { id, call_id, status, by, payload }
ApprovalGranted { id, tool, scope, workspace_hash }
ToolDispatch { call_id, tool, arguments_hash }
RequestSnapshot { provider, model, params_hash, tools, system_hash, sections, policy_hash }
```

`Step` / `Turn` keep their `data: Value` payload; write
`{"phase":"end","status":"suspended"}` and `{"phase":"end","reason":"suspended"}` respectively.

`ApprovalGranted` is typed and has a builder only: **session-scope grants are deferred**
(no producer writes the row and nothing replays it into `ApprovalPolicy::grants`). The
shipped UI offers 批准一次 / 拒绝; `ApprovalPolicy::grants` carries host pre-authorizations
supplied when the agent is constructed.

- [ ] **Step 5: Implement resume**

Add `AgentInput::resume_decisions(token, decisions)`; the convenience `resume_approving_all(token)` writes one `Resolved` decision per pending call. An empty or partial decision list is refused (`ResumeCoverageError::Missing`), never widened to approve-all. Validate last seq, pending set, and `request_snapshot` fingerprint. Dispatch approved calls, append `approval/decided` + `tool_dispatch`, flush, execute, then `turn/start` and continue.

- [ ] **Step 6: Implement repair awareness**

`repair` must treat `turn/end{suspended}` as intentional; do not append `TOOL_OUTCOME_UNKNOWN` for its pending calls.

- [ ] **Step 7: Run the tests**

```bash
cargo test -p uva-tool --lib
cargo test -p uva-harness --lib approval
cargo test -p uva-session --lib repair
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/uva-tool crates/uva-tool-shell crates/uva-harness crates/uva-session
git commit -m "feat: suspend high-risk tool calls for approval"
```

---

### Task 8: Compaction seam

**Files:**
- Create: `crates/uva-session/src/compaction.rs`
- Modify: `crates/uva-session/src/lib.rs`
- Modify: `crates/uva-session/src/tool_pairing.rs`
- Modify: `crates/uva-harness/src/agent/caps.rs` (`compactor`)
- Modify: `crates/uva-harness/src/agent/re_act/loop/mod.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/prepare.rs`
- Modify: `crates/uva-harness/src/events/mod.rs`
- Modify: `crates/uva-harness/src/agent/re_act/agent.rs` (`with_compactor`)
- Test: `crates/uva-session/src/compaction/tests.rs`

**Interfaces:**
- Consumes: `Message`, `TokenBudget`, `SessionEvent::Compaction`, `fold_surface`, `tool_pairing`.
- Produces:

```rust
pub struct TokenBudget { pub window: usize, pub reserve: usize }
pub struct CompactionPlan {
    pub range: std::ops::Range<usize>,
    pub shadowed_seqs: Vec<u64>,
    pub keep_recent: usize,
}
pub struct CompactionOutcome {
    pub summary: String,
    pub shadowed_seqs: Vec<u64>,
    pub shadowed_tokens: usize,
}

pub trait Compactor: Send + Sync {
    fn plan(&self, history: &[Message], budget: TokenBudget) -> Option<CompactionPlan>;
    fn summarize(
        &self,
        plan: &CompactionPlan,
        cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>>;
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn no_plan_when_history_is_under_budget() { /* ... */ }

#[test]
fn plan_keeps_the_recent_tail_and_balances_tool_pairs() {
    // assert the boundary never splits call/result
}

#[test]
fn outcome_carries_every_shadowed_seq() { /* ... */ }

#[test]
fn pruner_runs_before_the_summary_call() { /* order assertion */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p uva-session --lib compaction::`
Expected: FAIL.

- [ ] **Step 3: Implement the trait and the default compactor**

`plan` measures with the existing token meter, keeps the recent tail, and calls the
`tool_pairing` boundary helpers. `summarize` calls the host-supplied model once and returns the
summary plus the exact shadowed set. `SummaryCompactor` is the default; without one installed the
loop never compacts.

- [ ] **Step 4: Wire the loop**

At the step boundary, before the request is assembled: if a compactor is installed and the run is
not suspended, run `plan` → `summarize` → append `SessionEvent::Compaction` with
`SurfaceOp::Replace` → refresh the projection. Emit `SystemEvent::CompactionStarted` /
`CompactionFinished` for the UI.

- [ ] **Step 5: Run the tests**

```bash
cargo test -p uva-session --lib compaction
cargo test -p uva-harness --lib compaction
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/uva-session crates/uva-harness
git commit -m "feat: add compaction seam with provenance and tool pairing"
```

---

### Task 9: Server follow, resume, compact, usage

**Files:**
- Create: `crates/uva-app-server/src/routes/follow.rs`
- Modify: `crates/uva-app-server/src/routes/mod.rs`
- Modify: `crates/uva-app-server/src/routes/chat.rs`
- Modify: `crates/uva-app-server/src/session/controller/ops.rs`
- Modify: `crates/uva-app-server/src/session/controller/run_task.rs`
- Modify: `crates/uva-app-server/src/state/app_state/sessions.rs`
- Modify: `crates/uva-harness/src/input.rs` and `crates/uva-harness/src/agent/approval.rs`
  (`ResumeDecision { id, status, payload }`; `AgentInput::resume_decisions(..)`)
- Test: `crates/uva-app-server/src/routes/tests/follow.rs`

**Interfaces:**
- Consumes: `LogWriter::read`, `derive_history`, `ResumeToken`, `ApprovalRequest`.
- Produces:

```rust
pub struct FollowSnapshot {
    pub header: SessionHeader,
    pub rows: Vec<Value>,
    pub cursor: u64,
    pub live: LiveTurn,
    pub context: ContextMeter,
    pub pending_interrupts: Vec<ApprovalRequest>,
    pub request_snapshot_hash: String,
}

/// One decision from the client. `Resolved` approves this call;
/// `Cancelled`/`Rejected` writes the model-visible is_error result
/// instead of executing it. The resume must cover every open
/// interrupt.
pub struct ResumeDecision {
    pub id: String,
    pub status: ResumeStatus,
    pub payload: Option<Value>,
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn follow_first_frame_is_a_complete_baseline() { /* rows + cursor + context */ }

#[tokio::test]
async fn reconnect_mid_stream_replaces_the_window_without_duplicates() { /* live vs durable */ }

#[tokio::test]
async fn compact_is_rejected_while_suspended() { /* 409 */ }

#[tokio::test]
async fn session_usage_is_scoped_to_the_session() { /* not global */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p uva-app-server --lib follow_`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add `SessionState::AwaitingApproval` (derived from the log on open). `maybe_start_run` refuses while awaiting. `follow` streams the baseline then rows. `resume` validates coverage and forwards the explicit decisions to `AgentInput::resume_decisions` (an empty list is refused, not approve-all). `compact` returns 409 when busy/suspended. `usage` reads the session's token meter.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p uva-app-server --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/uva-app-server
git commit -m "feat(server): add follow baseline, resume, compact, and session usage"
```

---

### Task 10: Web cutover

**Files:**
- Modify: `uva-web/src/api/chat-stream.ts`
- Modify: `uva-web/src/api/chat-message.ts`
- Modify: `uva-web/src/api/types.ts`
- Modify: `uva-web/src/pages/ChatPage.tsx`
- Create: `uva-web/src/components/chat/ApprovalCard.tsx`
- Create: `uva-web/src/components/chat/SteerChip.tsx`
- Create: `uva-web/src/components/chat/CompactionMarker.tsx`

**Interfaces:**
- Consumes: `follow` baseline + delta frames; `resume`; `delivery`.
- Produces: UI states `queue`, `steer accepted`, `injected step N`, `consumed`, `next turn`, `awaiting approval`.

- [ ] **Step 1: Update the wire types**

Replace the `/messages/stream` consumer with `follow`. Add `FollowSnapshot`, `live`, `pending_interrupts`, `context`.

- [ ] **Step 2: Implement the baseline replace**

On every `follow` open: replace the message window from the baseline, set the cursor, then append `seq > cursor`. Ignore `live_seq <= baseline`.

- [ ] **Step 3: Add the approval card**

Render it bound to the tool call; post `resume` with `{ interrupts, resume_token }`. Handle `rejected` / `cancelled`; disable the composer while pending.

- [ ] **Step 4: Add the steer chip**

Composer gets `Send` (queue) and `Steer` (inject). Show the state transitions. On `Stop`, return un-injected steers to the editor.

- [ ] **Step 5: Add the compaction marker and context meter**

Insert a timeline marker with the summary and shadowed range; footer shows session tokens/window.

- [ ] **Step 6: Verify**

Run:

```bash
cd uva-web && npm run build
make check-web
```

Then use Playwright MCP to walk the acceptance script A–G. No test files are added.

- [ ] **Step 7: Commit**

```bash
git add uva-web
git commit -m "feat(web): follow baseline, approval card, steer states, compaction marker"
```

---

### Task 11: Tutorial and acceptance

**Files:**
- Create: `docs/tutorial/complete-agent.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: a tutorial a new user can follow from an empty checkout, with a recorded acceptance pass.

- [ ] **Step 1: Write the tutorial**

Cover: start server, start web, configure provider, run A–G, what each step proves, how to inspect the log, how to install a plugin without touching harness, how to resume after a crash. Include the exact commands and the expected observation for each step.

- [ ] **Step 2: Record the acceptance pass**

Run the A–G script against the real server and web with Playwright MCP. Put the date, commit, provider/model, and each observed result in a “验收记录” section. A step that cannot be observed is marked failed, not omitted.

- [ ] **Step 3: Run the gates**

```bash
make ci
make check-web
```

Expected: both green.

- [ ] **Step 4: Commit**

```bash
git add docs/tutorial README.md
git commit -m "docs: add the complete agent conversation tutorial"
```

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| §6 日志契约 / resume | 2, 3 |
| §6.4 请求快照与配置漂移 | 7 |
| §7.1 历史投影 | 2, 3 |
| §7.2 压缩 seam | 8 |
| §7.3 压缩可见性 | 8, 9, 10 |
| §8 事件与插件接缝 | 5 |
| §8.5 委派 | 6 |
| §8.6 steer | 4 |
| §9 挂起与高风险审批 | 7 |
| §9.4 server 线协议 | 9, 10 |
| §10 清理与性能 | 1 |
| §11 验证 | 每个任务的测试 + Task 10/11 |

**Step scan:** every task ends with a test or a gate command and a commit. Cleanup is verified by a shell assertion; UI has no automated test suite by repo policy, so it uses `make check-web` + Playwright MCP.

**Type consistency:** `derive_history` is defined in Task 2 and consumed in Task 3; `HistoryProjection` is defined in Task 3 and used in Task 4; `Risk` / `ApprovalRequest` are defined in Task 5 and consumed in Task 7; `HookDecision::Ask` is defined in Task 5 and consumed in Task 7; `Compactor` is defined in Task 8 and consumed in Task 9; `ResumeToken` is defined in Task 7 and consumed in Task 9.

**Review Focus coverage:** items 1–5 map to Task 7 (1, 3, 5), Task 4 (2), and Task 9 (4).

**Proportion:** the plan names signatures and one representative test per behavior; it does not transcribe implementations.
