//! LLM-summarising context-window manager — Rust port of
//! [`pi-context-prune`](https://github.com/championswimmer/pi-context-prune).
//!
//! ## Behaviour
//!
//! `prepare` walks the message list, gathers contiguous blocks of
//! tool-result messages, hands each block to the supplied
//! summariser callback (a cheap LLM call), and replaces the block
//! with a single compact assistant summary message that names the
//! summarised tool calls by short alias (`t1`, `t2`, …). The
//! originals are kept in an in-memory [`ToolCallArchive`] keyed by
//! `tool_call_id` so the agent can recover them at any time via the
//! [`context_tree_query`](SummarizingContextManager::archive) helper
//! (callers register the corresponding `Tool` against the registry;
//! this crate only owns the index).
//!
//! This mirrors the design of `pi-context-prune/src/pruner.ts`:
//!
//! - Only the *tool-result* messages are pruned; assistant turns
//!   that *contain* tool calls stay intact so the model still
//!   sees its own call/return shape.
//! - Summaries are emitted as assistant turns because that is the
//!   only role the LLM trusts to author content; a system-prompt
//!   marker (`<context-prune-summary>`) lets the renderer hide them
//!   from the chat surface while keeping them in the LLM context.
//! - When the summary text is *larger* than the original tool
//!   outputs it would replace, pruning is skipped for that block
//!   to avoid inflating the context.
//!
//! ## Failure modes
//!
//! - Summariser returns `None` (LLM error / timeout) — the block
//!   is left untouched and `state.last_truncated` stays `false`.
//! - Empty `messages` — no-op.
//! - Zero tool results in the conversation — no-op.

//!
//! ## Module layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `archive` | [`ToolCallArchive`] + [`ToolCallRecord`] — the recoverable originals. |
//! | `lifecycle` | [`CompactionRecord`] + [`CompactionLifecycle`] — the emit surface. |
//! | `prompt` | Batch serialisation for the summariser (XML-fenced) + the R29-C details line. |
//! | `refs` | Short aliases (`t1`, `t2`, …) and the summary message that footers them. |
//! | `manager` | [`SummarizingContextManager`] — the `ContextManager` impl. |

use std::sync::Arc;

mod archive;
mod hook;
mod lifecycle;
mod manager;
mod prompt;
mod refs;

pub use archive::{ToolCallArchive, ToolCallRecord, ToolCallRecordKind};
pub use hook::{CompactionHookFn, as_summarise_fn};
pub use lifecycle::{CompactionLifecycle, CompactionRecord};
pub use manager::SummarizingContextManager;

/// XML marker that wraps every synthesised summary message. The
/// frontend renderer matches on this tag to hide the message from
/// the chat surface; the LLM still sees it because it lands in
/// the `messages` vector passed to the provider.
pub const SUMMARY_TAG: &str = "context-prune-summary";

/// Async callback that turns a serialised tool-result batch into
/// a short summary string. Implementations should return `None`
/// when the LLM call fails so the manager can skip the prune
/// rather than corrupt the context.
///
/// The signature mirrors
/// `pi-context-prune/src/summarizer.ts:summarizeBatch` so porting
/// new LLM providers is mechanical: build a system prompt +
/// user prompt, call the LLM, return the assistant text or
/// `None`.
pub type SummariseFn = Arc<
    dyn Fn(&str) -> futures::future::BoxFuture<'static, Option<String>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use parking_lot::Mutex;
    use synthia_provider::{
        Content,
        ContentPart,
        Message,
        ReasoningContent,
        Role,
        TextContent,
        ToolResult,
        ToolUse,
    };
    use synthia_session::CompactionLifecycleOutcome;

    use super::*;
    use crate::{
        compaction_settings::CompactionDetails,
        context_manager::{AgentState, ContextManager},
    };

    fn tool_msg(id: &str, name: &str, text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: Content::Single(ContentPart::ToolResult(ToolResult {
                tool_use_id: id.to_string(),
                tool_name: Some(name.to_string()),
                content: vec![ContentPart::Text(TextContent {
                    text: text.to_string(),
                    cache_control: None,
                })],
                structured_content: None,
                is_error: Some(false),
                metadata: serde_json::Map::new(),
                truncated_by: None,
            })),
            tool_call_id: Some(id.to_string()),
            name: None,
            tool_result_cleared_at: None,
        }
    }

    fn summarise_that_returns(s: String) -> SummariseFn {
        Arc::new(move |_input: &str| {
            let s = s.clone();
            Box::pin(async move { Some(s) })
        })
    }

    /// An assistant turn that issues `ids` in one message, carrying
    /// prose and thinking the widening splice must not drop.
    fn assistant_with_calls(
        text: &str,
        thinking: &str,
        ids: &[&str],
    ) -> Message {
        let mut parts = vec![
            ContentPart::Text(TextContent {
                text: text.to_string(),
                cache_control: None,
            }),
            ContentPart::Reasoning(ReasoningContent {
                text: thinking.to_string(),
                signature: None,
            }),
        ];
        parts.extend(ids.iter().map(|id| {
            ContentPart::ToolUse(ToolUse {
                id: (*id).to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({ "path": "notes.txt" }),
            })
        }));
        Message::new(Role::Assistant, Content::parts(parts))
    }

    /// The invariant the automatic splice used to violate: every
    /// `tool_use` has its `tool_result` and every result its call.
    fn assert_tool_calls_paired(history: &[Message]) {
        let mut calls = HashSet::new();
        let mut results = HashSet::new();
        for message in history {
            for part in &message.content {
                match part {
                    ContentPart::ToolUse(call) => {
                        calls.insert(call.id.clone());
                    }
                    ContentPart::ToolResult(result) => {
                        results.insert(result.tool_use_id.clone());
                    }
                    _ => {}
                }
            }
            if message.role == Role::Tool
                && let Some(id) = message.tool_call_id.clone()
            {
                results.insert(id);
            }
        }
        assert_eq!(
            calls, results,
            "every tool_use must keep its tool_result and every \
             tool_result its tool_use"
        );
    }

    #[tokio::test]
    async fn no_tool_results_leaves_messages_alone() {
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "ignored".into(),
        ));
        let mut msgs = vec![
            Message::system("sys"),
            Message::user("hi"),
            Message::assistant("hello"),
        ];
        let before = msgs.clone();
        let mut state = AgentState::with_window(0); // tiny window
        mgr.prepare(&mut msgs, &mut state).await;
        assert_eq!(msgs, before);
        assert!(!state.last_truncated);
        assert_eq!(mgr.archive().len(), 0);
    }

    #[tokio::test]
    async fn summariser_failure_leaves_block_untouched() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let fail: SummariseFn = Arc::new(move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { None })
        });
        let mgr = SummarizingContextManager::new(fail);
        let mut msgs = vec![
            Message::system("sys"),
            Message::user("do it"),
            tool_msg("c1", "shell", "long output that should NOT be replaced"),
        ];
        let mut state = AgentState::with_window(0);
        mgr.prepare(&mut msgs, &mut state).await;
        assert_eq!(msgs.len(), 3);
        assert!(!state.last_truncated);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oversized_summary_skips_prune() {
        // Summariser returns something *bigger* than the raw
        // output — manager must skip the block.
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "this summary is longer than the tool result".into(),
        ));
        let mut msgs =
            vec![Message::system("sys"), tool_msg("c1", "shell", "short")];
        let mut state = AgentState::with_window(0);
        mgr.prepare(&mut msgs, &mut state).await;
        assert_eq!(msgs.len(), 2, "block must be left in place");
        assert!(!state.last_truncated);
    }

    #[tokio::test]
    async fn successful_summary_replaces_block_and_archives_record() {
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "short summary".into(),
        ));
        let mut msgs = vec![
            Message::system("sys"),
            Message::user("read"),
            tool_msg("c1", "read_file", "raw output bytes"),
            tool_msg("c2", "read_file", "more raw bytes"),
        ];
        let before_len = msgs.len();
        let mut state = AgentState::with_window(10_000);
        mgr.prepare(&mut msgs, &mut state).await;

        assert!(state.last_truncated);
        // Two tool-results collapsed to one summary message.
        assert_eq!(msgs.len(), before_len - 1);
        let summary = &msgs[2];
        assert_eq!(summary.role, Role::Assistant);
        assert!(matches!(summary.content, Content::Single(_)));
        if let Content::Single(ContentPart::Text(tc)) = &summary.content {
            assert!(tc.text.contains(SUMMARY_TAG));
            assert!(tc.text.contains("`t1`"));
            assert!(tc.text.contains("`t2`"));
        } else {
            panic!("summary must be a Text content part");
        }
        assert_eq!(mgr.archive().len(), 2);
        let c1 = mgr
            .archive()
            .get("c1")
            .expect("c1 must be archived under its real id");
        assert_eq!(c1.tool_name, "read_file");
        assert_eq!(c1.result_text, "raw output bytes");
    }

    /// R1/R2/R5: under the tutorial's 3000-token window, a batch
    /// that covers only the two tool *results* must widen to the
    /// assistant turn that issued them. The old splice shadowed the
    /// results and left the `tool_use` blocks behind, which the
    /// provider rejects as a protocol violation for the rest of the
    /// session.
    #[tokio::test]
    async fn compaction_never_splits_a_tool_call_from_its_result() {
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "read notes.txt, then listed the directory".into(),
        ))
        .with_settings(crate::CompactionSettings {
            reserve_tokens: 800,
            keep_recent_tokens: 500,
            min_messages_between_compaction: 1,
            ..crate::CompactionSettings::default()
        });
        let payload = "x".repeat(5_000);
        let mut msgs = vec![
            Message::system("sys"),
            Message::user("read notes.txt, then run ls -la ."),
            assistant_with_calls(
                "I will read the file, then list the directory.",
                "two independent tool calls",
                &["c1", "c2"],
            ),
            tool_msg("c1", "read_file", &payload),
            tool_msg("c2", "shell", &payload),
            Message::assistant("done"),
        ];
        let mut state = AgentState::with_window(3000);
        mgr.prepare(&mut msgs, &mut state).await;

        assert!(state.last_truncated, "the 3000-token window must compact");
        assert_tool_calls_paired(&msgs);
        assert_eq!(
            msgs.len(),
            4,
            "the issuing turn and both results collapse into one summary"
        );
        // R2: the assistant prose the widening swallowed is still
        // recoverable, thinking included.
        let archived = mgr.archive().list();
        let turn = archived
            .iter()
            .find(|record| record.kind == ToolCallRecordKind::AssistantTurn)
            .expect("the issuing assistant turn must be archived");
        assert!(
            turn.result_text.contains("I will read the file"),
            "archived turn lost its text: {}",
            turn.result_text
        );
        assert!(
            turn.result_text.contains("two independent tool calls"),
            "archived turn lost its thinking: {}",
            turn.result_text
        );
    }

    /// R3: the emitted record covers the range that actually left the
    /// history, not the narrower tool-result run.
    #[tokio::test]
    async fn compaction_record_covers_the_extended_range() {
        use std::sync::Mutex;
        let records: Arc<Mutex<Vec<CompactionRecord>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&records);
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "short".into(),
        ))
        .with_compaction_emitter(move |record| {
            sink.lock().expect("records mutex").push(record);
        });
        let mut msgs = vec![
            Message::system("sys"),
            Message::user("go"),
            assistant_with_calls("working", "", &["c1", "c2"]),
            tool_msg("c1", "read_file", "raw output bytes"),
            tool_msg("c2", "shell", "another tool output"),
            Message::assistant("done"),
        ];
        let before = msgs.clone();
        let mut state = AgentState::with_window(3000);
        mgr.prepare(&mut msgs, &mut state).await;

        let captured = records.lock().expect("records mutex").clone();
        assert_eq!(captured.len(), 1, "exactly one compaction splice");
        let record = &captured[0];
        assert_eq!(record.start, 2, "the issuing assistant turn is included");
        assert_eq!(record.end, 5);
        assert_eq!(record.source_indices, vec![2, 3, 4]);
        assert_eq!(
            record.source_keys,
            vec![
                Some("assistant:c1".to_string()),
                Some("tool:c1".to_string()),
                Some("tool:c2".to_string()),
            ],
            "every shadowed row carries a durable identity: the turn \
             that made the calls, and the results that answer them",
        );
        // What the record claims is exactly what left the history.
        assert_eq!(&msgs[..record.start], &before[..record.start]);
        assert_eq!(&msgs[record.start + 1..], &before[record.end..]);
        assert_eq!(msgs.len(), before.len() - (record.end - record.start) + 1);
        assert!(
            !msgs.iter().any(|message| {
                message
                    .content
                    .extract_text()
                    .is_some_and(|text| text.contains("working"))
            }),
            "the shadowed assistant prose must not survive verbatim"
        );
    }

    #[tokio::test]
    async fn archive_lookup_resolves_short_aliases() {
        // The manager should expose the archive by alias as well
        // as by canonical id; but aliases live in the summary
        // message's metadata, not the archive itself. The
        // `context_tree_query` tool maps alias→id at lookup time.
        // Here we just confirm the canonical id path works.
        let mgr =
            SummarizingContextManager::new(summarise_that_returns("ok".into()));
        let mut msgs = vec![tool_msg("call-xyz", "shell", "hi")];
        let mut state = AgentState::with_window(10_000);
        mgr.prepare(&mut msgs, &mut state).await;
        assert!(mgr.archive().get("call-xyz").is_some());
    }

    /// R5-8: when a `compaction_emitter` is registered, the
    /// manager fires the callback for every splice with the
    /// pre-splice `start..end` indices and the summary text.
    /// The agent loop wraps this callback with code that
    /// appends a typed `SessionEvent::Compaction` to the
    /// session log.
    #[tokio::test]
    async fn compaction_emitter_fires_with_pre_splice_indices() {
        use std::sync::Mutex;
        let records: Arc<Mutex<Vec<CompactionRecord>>> =
            Arc::new(Mutex::new(Vec::new()));
        let records_inside = Arc::clone(&records);
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "short".into(),
        ))
        .with_compaction_emitter(move |rec| {
            records_inside.lock().expect("records mutex").push(rec);
        });
        let mut msgs = vec![
            tool_msg("c1", "read_file", "raw output bytes"),
            tool_msg("c2", "shell", "another tool output"),
        ];
        let mut state = AgentState::with_window(10_000);
        mgr.prepare(&mut msgs, &mut state).await;
        let emitted = records.lock().expect("records mutex").clone();
        assert_eq!(emitted.len(), 1, "exactly one compaction splice");
        let rec = &emitted[0];
        assert_eq!(rec.start, 0);
        assert_eq!(rec.end, 2);
        assert_eq!(rec.source_indices, vec![0, 1]);
        assert_eq!(rec.summary_text, "short");
    }

    /// R15-1: with a policy installed and a roomy window, the
    /// utilisation threshold blocks pruning — the batch stays
    /// even though the summariser would happily run.
    #[tokio::test]
    async fn settings_gate_blocks_prune_under_threshold() {
        use crate::CompactionSettings;

        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "tiny".into(),
        ))
        .with_settings(CompactionSettings::default());
        let mut msgs = vec![
            Message::user("q"),
            tool_msg("t1", "shell", &"x".repeat(500)),
        ];
        let mut state = crate::AgentState::with_window(1_000_000);
        let before = msgs.len();
        mgr.prepare(&mut msgs, &mut state).await;
        assert_eq!(
            msgs.len(),
            before,
            "roomy window must block the prune (threshold not met)"
        );
        assert!(!state.last_truncated);
    }

    /// R15-1: over the threshold (window - reserve), the prune
    /// fires and the throttle counter resets.
    #[tokio::test]
    async fn settings_gate_allows_prune_over_threshold() {
        use crate::CompactionSettings;

        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "tiny".into(),
        ))
        .with_settings(CompactionSettings::default());
        // Tiny window forces `estimated > window - reserve`; the
        // message count (4) clears the min_messages throttle.
        let mut msgs = vec![
            Message::user("q"),
            tool_msg("t1", "shell", &"x".repeat(500)),
            tool_msg("t2", "shell", &"x".repeat(500)),
            tool_msg("t3", "shell", &"x".repeat(500)),
        ];
        let mut state = crate::AgentState::with_window(10);
        mgr.prepare(&mut msgs, &mut state).await;
        assert!(state.last_truncated, "over-threshold run must prune");
        assert!(
            msgs.len() < 2 || msgs.iter().any(|m| m.role != Role::Tool),
            "the tool batch must have been replaced"
        );
        // Throttle reset: the counter reads 0 after the splice.
        assert_eq!(
            mgr.messages_since_compaction
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    /// R15-1: `enabled = false` never prunes regardless of
    /// utilisation.
    #[tokio::test]
    async fn settings_disabled_never_prunes() {
        use crate::CompactionSettings;

        let disabled = CompactionSettings {
            enabled: false,
            ..CompactionSettings::default()
        };
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "tiny".into(),
        ))
        .with_settings(disabled);
        let mut msgs = vec![
            Message::user("q"),
            tool_msg("t1", "shell", &"x".repeat(500)),
        ];
        let mut state = crate::AgentState::with_window(10);
        let before = msgs.len();
        mgr.prepare(&mut msgs, &mut state).await;
        assert_eq!(msgs.len(), before, "disabled must never prune");
        assert!(!state.last_truncated);
    }

    /// R29-C: details attached via `with_compaction_details` must
    /// reach the prompt handed to the summariser; the getter must
    /// return them; and the details line must be omitted when no
    /// details are set.
    #[tokio::test]
    async fn compaction_details_reach_summariser_prompt() {
        let captured: Arc<Mutex<Vec<String>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let summarise: SummariseFn = Arc::new(move |input: &str| {
            sink.lock().push(input.to_string());
            Box::pin(async move { Some("tiny".into()) })
        });
        let details = CompactionDetails {
            read_files: vec![PathBuf::from("a.rs")],
            modified_files: vec![PathBuf::from("b.rs")],
        };
        let mgr = SummarizingContextManager::new(summarise)
            .with_compaction_details(details.clone());
        assert_eq!(mgr.compaction_details(), Some(&details));

        let mut msgs = vec![
            Message::user("q"),
            tool_msg("t1", "shell", &"x".repeat(500)),
        ];
        let mut state = crate::AgentState::with_window(0);
        mgr.prepare(&mut msgs, &mut state).await;

        {
            let prompts = captured.lock();
            assert_eq!(prompts.len(), 1, "one batch → one summariser call");
            let prompt = &prompts[0];
            assert!(
                prompt.contains("Files touched since the last compaction"),
                "prompt missing details header: {prompt}"
            );
            assert!(prompt.contains("read [a.rs]"), "read segment: {prompt}");
            assert!(
                prompt.contains("modified [b.rs]"),
                "modified segment: {prompt}"
            );
        }

        // Control: without details the prompt is the legacy one.
        let plain: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&plain);
        let summarise: SummariseFn = Arc::new(move |input: &str| {
            sink.lock().push(input.to_string());
            Box::pin(async move { Some("tiny".into()) })
        });
        let bare = SummarizingContextManager::new(summarise);
        let mut msgs = vec![
            Message::user("q"),
            tool_msg("t1", "shell", &"x".repeat(500)),
        ];
        bare.prepare(&mut msgs, &mut state).await;
        assert!(
            !plain.lock()[0].contains("Files touched"),
            "details line must be omitted without details"
        );
    }

    /// R29-Phase-J: a committed compaction fires exactly
    /// `Start → Summary → End(Committed)`, all sharing one token.
    #[tokio::test]
    async fn compaction_lifecycle_commits_in_order() {
        let steps: Arc<Mutex<Vec<CompactionLifecycle>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&steps);
        let mgr = SummarizingContextManager::new(summarise_that_returns(
            "short".into(),
        ))
        .with_compaction_lifecycle_emitter(move |step| {
            sink.lock().push(step);
        });

        let mut msgs = vec![
            tool_msg("c1", "read_file", "raw output bytes"),
            tool_msg("c2", "shell", "another tool output"),
        ];
        let mut state = crate::AgentState::with_window(10_000);
        mgr.prepare(&mut msgs, &mut state).await;

        let captured = steps.lock().clone();
        assert_eq!(
            captured.len(),
            3,
            "committed compaction must fire exactly three steps; got {captured:?}"
        );
        let token = match &captured[0] {
            CompactionLifecycle::Start { token } => token.clone(),
            other => panic!("first step must be Start, got {other:?}"),
        };
        match &captured[1] {
            CompactionLifecycle::Summary {
                token: t,
                summary,
                shadowed_positions,
            } => {
                assert_eq!(t, &token, "summary must share the start token");
                assert_eq!(summary, "short");
                assert_eq!(
                    shadowed_positions,
                    &vec![0u64, 1],
                    "summary must cite the pre-splice positions it shadows"
                );
            }
            other => panic!("second step must be Summary, got {other:?}"),
        }
        match &captured[2] {
            CompactionLifecycle::End { token: t, outcome } => {
                assert_eq!(t, &token, "end must share the start token");
                assert_eq!(*outcome, CompactionLifecycleOutcome::Committed);
            }
            other => panic!("third step must be End, got {other:?}"),
        }
    }

    /// R29-Phase-J: a failed summariser still closes the
    /// lifecycle. The log then shows `Start → End(Failed)` — no
    /// `Summary` — which is what distinguishes a *failed*
    /// compaction from a *crashed* one (unmatched `Start`).
    #[tokio::test]
    async fn compaction_lifecycle_failure_emits_end_without_summary() {
        let steps: Arc<Mutex<Vec<CompactionLifecycle>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&steps);
        let summarise: SummariseFn =
            Arc::new(|_: &str| Box::pin(async move { None }));
        let mgr = SummarizingContextManager::new(summarise)
            .with_compaction_lifecycle_emitter(move |step| {
                sink.lock().push(step);
            });

        let mut msgs = vec![
            Message::user("q"),
            tool_msg("t1", "shell", &"x".repeat(500)),
        ];
        let mut state = crate::AgentState::with_window(10_000);
        let before = msgs.len();
        mgr.prepare(&mut msgs, &mut state).await;

        let captured = steps.lock().clone();
        assert_eq!(
            captured.len(),
            2,
            "failed compaction must fire Start + End only; got {captured:?}"
        );
        assert!(matches!(captured[0], CompactionLifecycle::Start { .. }));
        match &captured[1] {
            CompactionLifecycle::End { outcome, .. } => {
                assert_eq!(*outcome, CompactionLifecycleOutcome::Failed);
            }
            other => panic!("expected End(Failed), got {other:?}"),
        }
        // The batch is untouched — a failed compaction never
        // loses information.
        assert_eq!(msgs.len(), before);
    }
}
