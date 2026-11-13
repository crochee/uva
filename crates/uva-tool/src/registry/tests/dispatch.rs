//! Dispatch concern (per-tool truncation, streaming-as-
//! primary-execution-path semantics, and the
//! provenance-bearing snapshot projection).
//!
//! Six tests exercise:
//! - per-call truncation bound applied to a too-large output
//!   (`dispatch_applies_tool_truncate`)
//! - streaming dispatch collects the final `Result` while
//!   dropping progress items
//! - a stream that yields no `Result` is a contract violation
//!   (two shapes: empty stream, progress-only stream)
//! - `snapshot_with_provenance` carries the
//!   `ToolProvenance::Dynamic` flag and filters + sorts
//!
//! Local fixtures (`LargeOutputTool`, `StreamingTool`,
//! `EmptyStreamTool`, `ProgressOnlyTool`) move with the
//! section; they are referenced only by these tests.
//!
//! `use super::*;` brings in the parent block's `Arc`,
//! `PathBuf`, `ToolRegistry`, `ToolEntry`, `ToolOutput`,
//! `Context`, `ToolProvenance`, `uva_provider::ToolUse`,
//! `collect_results`, the `Tool` trait, and `Tool7` (the
//! crate-public `Tool` alias the parent `use` brings into
//! scope). `async_trait` and `futures::stream` are repeated
//! because the sub-module derives `Tool`/`Tool7` impls and
//! builds stream bodies locally.

use async_trait::async_trait;
use futures::{StreamExt, stream};

use super::*;
// `dispatch_one` tests reference the `StreamOutput` enum directly
// (the test streams need to pattern-match `Progress` vs `Result`).
use crate::traits::StreamOutput;

#[derive(Debug)]
struct LargeOutputTool;

#[async_trait]
impl Tool7 for LargeOutputTool {
    fn name(&self) -> &str {
        "large_output"
    }

    fn description(&self) -> &str {
        "Returns output larger than the configured bound"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        ToolOutput::text("line\n".repeat(100))
    }
}

#[tokio::test]
async fn dispatch_applies_tool_truncate() {
    let registry = ToolRegistry::new();
    assert!(registry.register_entry(ToolEntry::new(Arc::new(LargeOutputTool))));
    let mut context =
        Context::new("truncate-session".to_string(), std::env::temp_dir());
    context.output_bound.per_call_max_bytes = 80;
    context.output_bound.per_call_max_lines = 10;
    context.output_bound.managed_dir = tempfile::tempdir().unwrap().keep();
    let outputs = collect_results(
        registry.run_stream(
            vec![uva_provider::ToolUse {
                id: "call-1".to_string(),
                name: "large_output".to_string(),
                input: serde_json::json!({}),
            }],
            context,
        ),
        1,
    )
    .await;
    assert!(outputs[0].1.truncated_by.is_some());
    assert!(outputs[0].1.metadata.contains_key("managed_path"));
}

#[derive(Debug)]
struct StreamingTool {
    progress_count: usize,
}

#[async_trait]
impl Tool for StreamingTool {
    fn name(&self) -> &str {
        "streaming"
    }

    fn description(&self) -> &str {
        "Yields N progress items then a final Result"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        panic!("streaming tool should override stream, not call")
    }

    fn stream<'a>(
        &'a self,
        _input: serde_json::Value,
        _context: &'a Context,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = crate::traits::StreamOutput> + Send + 'a,
        >,
    > {
        let n = self.progress_count;
        Box::pin(
            stream::iter((0..n).map(|i| {
                crate::traits::StreamOutput::Progress(ToolOutput::text(
                    format!("step {i}"),
                ))
            }))
            .chain(stream::once(async {
                crate::traits::StreamOutput::Result(ToolOutput::text("done"))
            })),
        )
    }
}

#[tokio::test]
async fn dispatch_consumes_stream_collects_final_result() {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(StreamingTool {
        progress_count: 3,
    })));

    let results = collect_results(
        registry.run_stream(
            vec![uva_provider::ToolUse {
                id: "call-1".to_string(),
                name: "streaming".to_string(),
                input: serde_json::json!({}),
            }],
            Context::new("s1".to_string(), PathBuf::from("/tmp")),
        ),
        1,
    )
    .await;

    assert_eq!(results.len(), 1);
    // Progress items dropped, only the final Result surfaces.
    let text = results[0].1.content[0].text().unwrap();
    assert_eq!(text, "done");
    assert!(results[0].1.is_error.is_none());
}

#[derive(Debug)]
struct EmptyStreamTool;

#[async_trait]
impl Tool for EmptyStreamTool {
    fn name(&self) -> &str {
        "empty_stream"
    }

    fn description(&self) -> &str {
        "Stream that never yields a Result"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        ToolOutput::text("unused")
    }

    fn stream<'a>(
        &'a self,
        _input: serde_json::Value,
        _context: &'a Context,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = crate::traits::StreamOutput> + Send + 'a,
        >,
    > {
        Box::pin(stream::empty::<crate::traits::StreamOutput>())
    }
}

#[tokio::test]
async fn dispatch_returns_error_when_stream_yields_no_result() {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(EmptyStreamTool)));

    let results = collect_results(
        registry.run_stream(
            vec![uva_provider::ToolUse {
                id: "call-1".to_string(),
                name: "empty_stream".to_string(),
                input: serde_json::json!({}),
            }],
            Context::new("s1".to_string(), PathBuf::from("/tmp")),
        ),
        1,
    )
    .await;

    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_error.unwrap_or(false));
    let text = results[0].1.content[0].text().unwrap();
    assert!(text.contains("contract violation"), "got: {text}");
}

#[derive(Debug)]
struct ProgressOnlyTool;

#[async_trait]
impl Tool for ProgressOnlyTool {
    fn name(&self) -> &str {
        "progress_only"
    }

    fn description(&self) -> &str {
        "Stream that yields only Progress items, no Result"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        ToolOutput::text("unused")
    }

    fn stream<'a>(
        &'a self,
        _input: serde_json::Value,
        _context: &'a Context,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = crate::traits::StreamOutput> + Send + 'a,
        >,
    > {
        Box::pin(stream::iter([
            crate::traits::StreamOutput::Progress(ToolOutput::text("a")),
            crate::traits::StreamOutput::Progress(ToolOutput::text("b")),
        ]))
    }
}

#[tokio::test]
async fn dispatch_treats_no_result_as_contract_violation() {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(ProgressOnlyTool)));

    let results = collect_results(
        registry.run_stream(
            vec![uva_provider::ToolUse {
                id: "call-1".to_string(),
                name: "progress_only".to_string(),
                input: serde_json::json!({}),
            }],
            Context::new("s1".to_string(), PathBuf::from("/tmp")),
        ),
        1,
    )
    .await;

    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_error.unwrap_or(false));
}

/// `snapshot_with_provenance` returns one record per visible tool
/// with `provenance: ToolProvenance::Dynamic` (since `register_entry`
/// always wraps as Dynamic).
#[test]
fn snapshot_with_provenance_returns_records_with_provenance() {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(NamedTool("alpha"))));
    let snap = registry.snapshot_with_provenance();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].metadata.name, "alpha");
    assert_eq!(snap[0].provenance, ToolProvenance::Dynamic);
}

/// `snapshot_with_provenance` filters hidden entries (same as
/// `snapshot`) and returns records sorted by name.
#[test]
fn snapshot_with_provenance_skips_hidden_and_sorts() {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(NamedTool("zoo"))));
    registry.register_entry(
        ToolEntry::new(Arc::new(NamedTool("apple"))).with_is_hidden(true),
    );
    registry.register_entry(ToolEntry::new(Arc::new(NamedTool("mango"))));
    let snap = registry.snapshot_with_provenance();
    let names: Vec<&str> =
        snap.iter().map(|r| r.metadata.name.as_str()).collect();
    assert_eq!(names, vec!["mango", "zoo"]);
}

// ── dispatch_one ──────────────────────────────────────────────
//
// `dispatch_one(call, ctx)` is the per-call sibling of `run_stream`:
// same hidden-refusal + JSON-schema-validation contract, same
// panic-recovery + missing-`Result` synthesis, but no batch plumbing
// (no semaphore, no `call_id`, no `select_all`). The tests below
// pin the contract so the two paths cannot drift.

/// `dispatch_one` returns exactly one terminal `Result` per call, no
/// matter what the underlying `Tool::stream` does: streaming
/// progress + final result, a single direct result, or a contract
/// violation (no result at all) must all surface as one `Result`
/// item so the caller can `await` the stream and trust the next
/// item it sees is the final outcome.
#[tokio::test]
async fn dispatch_one_yields_exactly_one_terminal_result() {
    use futures::StreamExt;
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(StreamingTool {
        progress_count: 2,
    })));
    let context =
        Context::new("dispatch_one".to_string(), std::env::temp_dir());
    let mut stream = registry.dispatch_one(
        uva_provider::ToolUse {
            id: "c1".to_string(),
            name: "streaming".to_string(),
            input: serde_json::json!({}),
        },
        &context,
    );
    let mut progress_count = 0;
    let mut result_count = 0;
    let mut result_text = String::new();
    while let Some(item) = stream.next().await {
        match item {
            StreamOutput::Progress(_) => progress_count += 1,
            StreamOutput::Result(out) => {
                result_count += 1;
                if let Some(text) = out.content.iter().find_map(|p| match p {
                    uva_provider::types::ContentPart::Text(t) => {
                        Some(t.text.clone())
                    }
                    _ => None,
                }) {
                    result_text = text;
                }
            }
        }
    }
    assert_eq!(progress_count, 2, "StreamingTool emits 2 Progress items");
    assert_eq!(result_count, 1, "exactly one terminal Result");
    assert!(result_text.contains("done"));
}

/// `dispatch_one` with an unknown tool name surfaces a
/// "tool not found" `Result` rather than an empty stream — the
/// caller should never have to special-case "no items ever arrived".
#[tokio::test]
async fn dispatch_one_synthesises_not_found_for_unknown_tool() {
    use futures::StreamExt;
    let registry = ToolRegistry::new();
    let context =
        Context::new("dispatch_one_miss".to_string(), std::env::temp_dir());
    let mut stream = registry.dispatch_one(
        uva_provider::ToolUse {
            id: "c1".to_string(),
            name: "ghost".to_string(),
            input: serde_json::json!({}),
        },
        &context,
    );
    let mut result_count = 0;
    let mut last_text = String::new();
    while let Some(item) = stream.next().await {
        if let StreamOutput::Result(out) = item {
            result_count += 1;
            last_text = out
                .content
                .iter()
                .find_map(|p| match p {
                    uva_provider::types::ContentPart::Text(t) => {
                        Some(t.text.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
        }
    }
    assert_eq!(result_count, 1);
    assert!(
        last_text.contains("tool not found"),
        "missing tool gets a 'tool not found' Result: {last_text:?}"
    );
}

/// `dispatch_one` honours `is_hidden` exactly the way `run_stream`
/// does: a hidden tool is not advertised and the registry refuses
/// the call, returning the same "tool not found" shape so a caller
/// cannot distinguish a hidden tool from an absent one.
#[tokio::test]
async fn dispatch_one_refuses_hidden_tool_with_not_found_shape() {
    use futures::StreamExt;
    let registry = ToolRegistry::new();
    registry.register_entry(
        ToolEntry::new(Arc::new(NamedTool("private"))).with_is_hidden(true),
    );
    let context =
        Context::new("dispatch_one_hidden".to_string(), std::env::temp_dir());
    let mut stream = registry.dispatch_one(
        uva_provider::ToolUse {
            id: "c1".to_string(),
            name: "private".to_string(),
            input: serde_json::json!({}),
        },
        &context,
    );
    let mut last_text = String::new();
    while let Some(item) = stream.next().await {
        if let StreamOutput::Result(out) = item {
            last_text = out
                .content
                .iter()
                .find_map(|p| match p {
                    uva_provider::types::ContentPart::Text(t) => {
                        Some(t.text.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
        }
    }
    assert!(
        last_text.contains("tool not found"),
        "a hidden tool is refused with the not-found shape: {last_text:?}"
    );
}

/// `dispatch_one` with a tool that yields no `Result` synthesises
/// the same contract-violation error that `run_stream` does, so
/// neither path can silently drop a broken tool.
#[tokio::test]
async fn dispatch_one_synthesises_contract_violation_on_empty_stream() {
    use futures::StreamExt;
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(EmptyStreamTool)));
    let context =
        Context::new("dispatch_one_empty".to_string(), std::env::temp_dir());
    let mut stream = registry.dispatch_one(
        uva_provider::ToolUse {
            id: "c1".to_string(),
            name: "empty_stream".to_string(),
            input: serde_json::json!({}),
        },
        &context,
    );
    let mut last_text = String::new();
    let mut result_count = 0;
    while let Some(item) = stream.next().await {
        if let StreamOutput::Result(out) = item {
            result_count += 1;
            last_text = out
                .content
                .iter()
                .find_map(|p| match p {
                    uva_provider::types::ContentPart::Text(t) => {
                        Some(t.text.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
        }
    }
    assert_eq!(result_count, 1, "exactly one terminal Result");
    assert!(
        last_text.contains("contract violation"),
        "an empty stream surfaces as a contract violation: {last_text:?}"
    );
}
