# uva-tool-plugins

The shipped **tool plugins**, one module and one feature per tool:
`read`, `write`, `shell`, `todo`, `web`, `task`, `scheduler`, `search`,
`memo`.

The paradigm — the `Tool` trait, the registry, the exposure projection,
the `workspace` path confinement — lives in [`uva-tool`](../uva-tool).
This crate ships the *implementations*, so a consumer composes the tool
surface brick by brick instead of depending on a whole crate per brick.

> Tool seam: [`SEAMS.md`](../../SEAMS.md). Each plugin's full API is in
> its module docs (`src/<tool>/mod.rs`).

## Usage

```toml
[dependencies]
uva-tool-plugins = { version = "0.1", features = ["read", "write"] }
```

```rust
use uva_tool_plugins::read::ReadToolPlugin;

// Each tool ships as a kernel plugin that registers itself in the
// injected `tools` registry and withdraws exactly that registration
// when it is unloaded.
let plugin = ReadToolPlugin::new();
```

Through the facade, the same tools are one feature away:

```toml
uva = { version = "0.1", features = ["tool-read", "tool-write"] }
```

## The tools

### `read`

The `read` builtin: workspace file reader with line ranges. Reads a
UTF-8 text file within the workspace, optionally restricted to a
1-based line range. Output is prefixed with right-aligned line numbers.

```rust
use uva_tool_plugins::read::ReadTool;

let tool = ReadTool::default();
let out = tool.execute(ctx).await?;
```

### `write`

The `write` builtin: workspace file writer (overwrite / append).

### `shell`

The `shell` tool plugin: bounded command executor that owns its
execution policy (`shell::sandbox` module). The shell plugin is the
**only** tool with a process-level sandbox policy, because that's where
the policy actually matters. The `ExecutionPolicy` / `SandboxBackend` /
`BwrapBackend` / `effective_policy` etc. live here, not in
`uva-tool`, so the description the model sees and the argv the
harness actually spawns are guaranteed to come from the same value.

### `todo`

The `TodoWrite` builtin: structured task-list state the model can
maintain.

### `web`

The `web_fetch` builtin: the one builtin that talks to the network.

This is the **only** feature allowed to pull `reqwest` / `hyper` /
`rustls`. The `make check-mvp-deps` gate asserts that the other eight
tool features stay offline.

### `task`

The `task` tool plugin: multi-agent delegation by running registered
peers as sub-agents (depth cap, verification gates, worktree
isolation).

It is the first consumer of the
[`DelegatingTool`](../../SEAMS.md) seam — i.e. it needs the loop's
internal capability (event forwarding, cancellation, depth), so it
depends on `uva-harness` rather than just `uva-tool`.

### `scheduler`

The `schedule` tool plugin: let the model create and manage scheduled
jobs on top of `uva-scheduler::ScheduleStore`.

The `cron` parser support is opt-in: the facade `cron` feature flips
both `uva-scheduler` and this crate's `cron` feature together so
that the model can actually create cron jobs. Without `cron`,
`kind: "cron"` is refused outright (the parser simply doesn't exist, so
partial-feature configurations can't drift into a half-working state).

### `search`

The `search` tool plugin: cross-domain search over a host-built
`uva-search::Registry` (skills / memory / tools catalogs) with
deferred exposure. Cold start advertises only name + description; on
first call the schema upgrades to the actual `query` / `limit` /
`domain` shape.

### `memo`

Deterministic tool result memoization. A `Tool` wrapper that
short-circuits identical invocations through an LRU + TTL store, and
single-flights concurrent calls for the same key so the inner tool
runs at most once per cache slot.

This one is **opt-in** (`uva = { features = ["tool-memo"] }` in
the facade, or the `memo` feature here). It stays in the offline tier —
no HTTP, no DB, no OTel — and is covered by `make check-mvp-deps`.

Surface:

- `MemoStore` — pluggable storage trait (`lookup`, `store`,
  `invalidate`).
- `InMemoryMemoStore` — built-in, thread-safe LRU + TTL.
- `MemoizedTool` — `Tool` wrapper that consults the store on every
  call, single-flights concurrent calls per key, and respects
  `MemoScope` (process / workspace).
- `MemoConfig` — `max_entries`, `ttl`, `scope`,
  `non_deterministic_fingerprint_keys` (default none).
- `BLAKE3` keys derived from `tool.name + canonical_input_json +
  scope_path`. Reuses the same JSON shape the registry hands the
  tool (`serde_json::to_value(&input)`).

```rust
use std::sync::Arc;
use std::time::Duration;

use uva_tool::{ToolEntry, ToolRegistry};
use uva_tool_plugins::memo::{
    InMemoryMemoStore,
    MemoConfig,
    MemoScope,
    MemoizedTool,
};

let inner: Arc<dyn uva_tool::Tool> = Arc::new(ReadTool::new(...));
let memo = MemoizedTool::new(
    inner,
    MemoConfig::default()
        .max_entries(4096)
        .ttl(Duration::from_secs(5 * 60))
        .scope(MemoScope::Process),
    Arc::new(InMemoryMemoStore::default()),
    Arc::new(uva_core::SystemClock),
);
let registry = ToolRegistry::new();
registry.register_entry(ToolEntry::new(memo));
```

## Examples

| Example | Shows |
| :--- | :--- |
| `read_ranges` | 按行区间读文件 |
| `write_modes` | 覆写 / 追加两种写法 |
| `todo_list` | 任务清单的状态流转 |
| `sandbox_policy` | shell 的执行策略 |
| `allowlist` | `web_fetch` 的域名白名单 |
| `delegation_gate` | 子任务 gate 命令必须通过 |
| `worktree_isolation` | 每个子 agent 一个 `uva/agent-<ulid>` worktree |
| `schedule_jobs` | 创建与查询定时任务 |
| `search_tool_demo` | 跨域检索工具 |
| `tool_memo_demo` | 记忆层命中缓存 |

跑： `cargo run --example <name> -p uva-tool-plugins`。

## CI 契约

- 离线（无 HTTP / DB / OTel）：`read` / `write` / `shell` / `todo` /
  `task` / `scheduler` / `search` / `memo` 八个 feature 的组合由
  `make check-mvp-deps` 断言；只有 `web` 允许 `reqwest`；
- `cargo test -p uva-tool-plugins --all-features` 绿；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p uva-tool-plugins
  --all-features` 绿（门禁见 `AGENTS.md` §3.6）。
