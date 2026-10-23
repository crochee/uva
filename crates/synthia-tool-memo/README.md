# synthia-tool-memo

Deterministic tool result memoization. A `Tool` wrapper that
short-circuits identical invocations through an LRU + TTL store, and
single-flights concurrent calls for the same key so the inner tool
runs at most once per cache slot.

This crate is **opt-in** (`synthia = { features = ["tool-memo"] }` in
the facade, or `cargo add synthia-tool-memo` for direct consumers).
It stays in the offline tier — no HTTP, no DB, no OTel — and is
covered by `make check-mvp-deps`.

## Surface

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

## Wiring

```rust
use std::sync::Arc;
use std::time::Duration;

use synthia_tool::{ToolEntry, ToolRegistry};
use synthia_tool_memo::{InMemoryMemoStore, MemoConfig, MemoScope, MemoizedTool};

let inner: Arc<dyn synthia_tool::Tool> = Arc::new(ReadTool::new(...));
let memo = MemoizedTool::new(
    inner,
    MemoConfig::default()
        .max_entries(4096)
        .ttl(Duration::from_secs(5 * 60))
        .scope(MemoScope::Process),
    Arc::new(InMemoryMemoStore::default()),
);
let registry = ToolRegistry::new();
registry.register_entry(ToolEntry::new(memo));
```

## Demo

```
cargo run --example tool_memo_demo -p synthia-tool-memo
```

Expected tail: `OK: 1 invocation, 4 cache-hits`.

## Design notes

- Non-deterministic tools opt out by overriding the wrapper's
  `is_memoizable`; the default is "always memoizable" and the test
  suite pins this. A real `shell` tool, for instance, returns false
  when argv contains pipes / subshells; `web_fetch` returns false
  unless the caller passes `cacheable: true`.
- Single-flight via `tokio::sync::OnceCell` collapsed through
  `Arc<Mutex<HashMap<Key, OnceCell<ToolOutput>>>>` — concurrent calls
  for the same key await the first caller's future.
- `MemoScope::Workspace(path)` adds the canonicalised path to the
  key, so two workspaces do not share cache.
- No edits to `synthia-tool` — composition only.

See `crates/synthia-tool-memo/src/lib.rs` for the full API.