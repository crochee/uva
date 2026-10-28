//! # elysper-tool-memo
//!
//! Deterministic tool result memoization.
//!
//! A [`MemoizedTool`] wraps any [`elysper_tool::Tool`] and
//! short-circuits identical invocations through an LRU + TTL store
//! keyed on `BLAKE3(tool.name + canonical_input_json + scope_path)`.
//! Concurrent calls for the same key are single-flighted through an
//! internal `OnceCell`-equivalent so the inner tool runs at most
//! once per cache slot.
//!
//! ```
//! use std::sync::Arc;
//!
//! use elysper_tool::{Context, Tool, ToolOutput};
//! use elysper_tool_memo::{
//!     InMemoryMemoStore,
//!     MemoConfig,
//!     MemoScope,
//!     MemoizedTool,
//! };
//!
//! struct Echo;
//!
//! #[async_trait::async_trait]
//! impl Tool for Echo {
//!     fn name(&self) -> &str {
//!         "echo"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "Echo the input back."
//!     }
//!
//!     fn parameters(&self) -> serde_json::Value {
//!         serde_json::json!({ "type": "object" })
//!     }
//!
//!     async fn call(
//!         &self,
//!         input: serde_json::Value,
//!         _context: &Context,
//!     ) -> ToolOutput {
//!         ToolOutput::text(input.to_string())
//!     }
//! }
//!
//! let inner: Arc<dyn Tool> = Arc::new(Echo);
//! let memo = MemoizedTool::new(
//!     inner,
//!     MemoConfig::default().scope(MemoScope::Process),
//!     Arc::new(InMemoryMemoStore::default()),
//!     Arc::new(elysper_core::SystemClock),
//! );
//!
//! // The second identical call is served from the store.
//! futures::executor::block_on(async {
//!     let context = Context::default();
//!     let first = memo.call(serde_json::json!({ "x": 1 }), &context).await;
//!     let second = memo.call(serde_json::json!({ "x": 1 }), &context).await;
//!     assert_eq!(first, second);
//!     assert_eq!(memo.miss_count(), 1);
//!     assert_eq!(memo.hit_count(), 1);
//! });
//! ```
//!
//! ## Layering
//!
//! The wrapper sits between the dispatcher and the inner tool:
//!
//! ```text
//!   ToolRegistry::dispatch_one
//!     └─ MemoizedTool   ← lookup(store) → hit → return cached
//!          └─ inner tool ← miss → call, then store
//! ```
//!
//! `Tool::name` / `description` / `parameters` / `annotations` /
//! `mode` / `output_definition` / `stream` / `truncate` are forwarded
//! unchanged; only `call` consults the store. Streaming tools that
//! override `stream` keep their override — the memoization only
//! affects the simple path through `call` and the default
//! `stream`-from-`call` projection.
//!
//! ## Storage
//!
//! [`InMemoryMemoStore`] is the default LRU + TTL store. Custom
//! implementations plug in through [`MemoStore`] (two methods +
//! `invalidate`).
//!
//! ## Non-deterministic opt-out
//!
//! Tools that include wall-clock time, randomness, or environment
//! probes in their outputs override [`MemoizedTool::is_memoizable`]
//! to return `false` for those invocations. The default is `true`;
//! the test suite pins this so a wrapping consumer never silently
//! caches a non-deterministic result.

pub mod store;
mod wrapper;

pub use store::{InMemoryMemoStore, MemoEntry, MemoKey, MemoScope, MemoStore};
// Re-exports for ergonomic plumbing.
pub use elysper_core::Clock;
pub use elysper_tool::{Context, Tool, ToolEntry, ToolOutput};
pub use wrapper::{MemoConfig, MemoizedTool};

// No public `MemoError` or `Result` alias: the wrapper surfaces its
// inner tool's `ToolOutput` and never returns a custom error.
// Internal call sites use `elysper_core::Result<T, Error>` or
// `core::result::Result<T, E>` directly.

/// Hash the canonical JSON form of `input` plus the tool name and
/// the optional scope path. Stable across runs (the JSON form is what
/// the registry hands the tool, so the cache key is the same shape
/// the tool sees).
#[must_use]
pub fn memo_key(
    tool_name: &str,
    input: &serde_json::Value,
    scope: &MemoScope,
) -> MemoKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(&[0xff]);
    // Canonical JSON: serde_json emits a deterministic byte order
    // when the value is a `Value` (numbers preserved, maps
    // BTreeMap-style in alphabetic order). Tools that carry
    // nondeterministic structures (HashMap of String) produce
    // different bytes across runs; the test suite covers this.
    let bytes = serde_json::to_vec(input).unwrap_or_default();
    hasher.update(&bytes);
    hasher.update(&[0xff]);
    match scope {
        MemoScope::Process => {
            hasher.update(b"process");
        }
        MemoScope::Workspace(p) => {
            hasher.update(b"workspace");
            hasher.update(p.as_os_str().as_encoded_bytes());
        }
    }
    let hash = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hash.as_bytes()[..32]);
    MemoKey(bytes)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn memo_key_distinguishes_names() {
        let k1 = memo_key("a", &serde_json::json!({}), &MemoScope::Process);
        let k2 = memo_key("b", &serde_json::json!({}), &MemoScope::Process);
        assert_ne!(k1, k2);
    }

    #[test]
    fn memo_key_distinguishes_inputs() {
        let k1 =
            memo_key("t", &serde_json::json!({"x": 1}), &MemoScope::Process);
        let k2 =
            memo_key("t", &serde_json::json!({"x": 2}), &MemoScope::Process);
        assert_ne!(k1, k2);
    }

    #[test]
    fn memo_key_distinguishes_scopes() {
        let k1 = memo_key(
            "t",
            &serde_json::json!({}),
            &MemoScope::Workspace(std::path::PathBuf::from("/a")),
        );
        let k2 = memo_key(
            "t",
            &serde_json::json!({}),
            &MemoScope::Workspace(std::path::PathBuf::from("/b")),
        );
        assert_ne!(k1, k2);
    }
}
