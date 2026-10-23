//! Server-side run factory glue: [`CompactionEmitters`] and
//! [`context_manager_for_compaction_with_emitters`] for hand-
//! assembled run factories.
//!
//! The harness crate ships the canonical `ReActAgent` builder; the
//! server's `SessionController` drives runs through that builder
//! directly. A run factory that builds agents by hand (rather than
//! through `ReActAgent`'s `with_context_manager` setter) calls
//! `context_manager_for_compaction_with_emitters` to honour the
//! `agents.<name>.compaction` config. Both types moved out of
//! `synthia-harness` (R126 refactor) — they describe how *this
//! server* drives a compaction-armed run, not what the agent loop
//! itself needs.

use std::sync::Arc;

use synthia::{
    context::{
        CompactionLifecycle,
        CompactionRecord,
        CompactionSettings,
        ContextManager,
        SummariseFn,
        SummarizingContextManager,
        TruncatingContextManager,
    },
    provider::{CompletionRequest, Message, traits::ModelProvider},
    session::{CompactionLifecycleView, CompactionRecordView},
};

pub use super::agent_run_config_types::{AgentRunConfig, RunOverrides};

/// The two emitters a durable compaction checkpoint installs on a
/// summarising context manager. Built from a
/// a `CompactionCheckpoint`'s callbacks (or any pair
/// of callbacks with the same shape). The server wires this when a
/// run is armed with a compaction policy but no external checkpoint.
#[derive(Clone)]
pub struct CompactionEmitters {
    /// Invoked once per successful compaction record.
    pub record: Arc<dyn Fn(CompactionRecordView) + Send + Sync>,
    /// Invoked once per compaction lifecycle step.
    pub lifecycle: Arc<dyn Fn(CompactionLifecycleView) + Send + Sync>,
}

impl CompactionEmitters {
    /// Build an emitter pair from two `Arc<dyn Fn>` callbacks.
    pub fn new(
        record: Arc<dyn Fn(CompactionRecordView) + Send + Sync>,
        lifecycle: Arc<dyn Fn(CompactionLifecycleView) + Send + Sync>,
    ) -> Self {
        Self { record, lifecycle }
    }
}

/// Public entry point for the decision core, with the durable
/// emitters installed. Builds a provider-backed summarising manager
/// when the policy is enabled + valid, otherwise returns a
/// truncating manager.
///
/// Server-side run factories that build agents by hand (rather than
/// through `ReActAgent`'s `with_context_manager` setter) call
/// this to honour the `agents.<name>.compaction` config.
#[must_use]
pub fn context_manager_for_compaction_with_emitters(
    provider: Arc<dyn ModelProvider>,
    settings: CompactionSettings,
    emitters: Option<CompactionEmitters>,
) -> Arc<dyn ContextManager> {
    resolve(
        Some(settings),
        Arc::new(TruncatingContextManager),
        &provider,
        emitters,
    )
}

fn resolve(
    settings: Option<CompactionSettings>,
    default: Arc<dyn ContextManager>,
    provider: &Arc<dyn ModelProvider>,
    emitters: Option<CompactionEmitters>,
) -> Arc<dyn ContextManager> {
    let Some(settings) = settings else {
        return default;
    };
    if !settings.enabled {
        tracing::debug!("compaction disabled; keeping default manager");
        return default;
    }
    if let Err(e) = settings.validate() {
        tracing::warn!(error = %e, "invalid CompactionSettings; keeping default manager");
        return default;
    }

    let provider = Arc::clone(provider);
    let summarise: SummariseFn = Arc::new(move |batch| {
        let provider = Arc::clone(&provider);
        let batch = batch.to_string();
        Box::pin(async move {
            let request = CompletionRequest {
                messages: Arc::new(vec![Message::user(format!(
                    "Summarise the following tool-call batch. Preserve \
                     actionable facts, file paths, and outcomes; drop \
                     raw payloads:\n\n{batch}"
                ))]),
                ..Default::default()
            };
            match provider.complete(request).await {
                Ok(resp) => resp.content.extract_text(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "compaction summarise call failed; skipping batch"
                    );
                    None
                }
            }
        })
    });
    let manager =
        SummarizingContextManager::new(summarise).with_settings(settings);
    match emitters {
        Some(emitters) => Arc::new(emitters.install(manager)),
        None => Arc::new(manager),
    }
}

impl CompactionEmitters {
    fn install(
        self,
        manager: SummarizingContextManager,
    ) -> SummarizingContextManager {
        let record = self.record;
        let lifecycle = self.lifecycle;
        manager
            .with_compaction_emitter(move |rec: CompactionRecord| {
                record(record_view(&rec));
            })
            .with_compaction_lifecycle_emitter(
                move |step: CompactionLifecycle| {
                    lifecycle(lifecycle_view(&step));
                },
            )
    }
}

fn record_view(record: &CompactionRecord) -> CompactionRecordView {
    CompactionRecordView {
        start: record.start,
        end: record.end,
        source_indices: record.source_indices.clone(),
        source_keys: record.source_keys.clone(),
        summary_text: record.summary_text.clone(),
    }
}

fn lifecycle_view(step: &CompactionLifecycle) -> CompactionLifecycleView {
    match step {
        CompactionLifecycle::Start { token } => {
            CompactionLifecycleView::Start {
                token: token.as_str().to_string(),
            }
        }
        CompactionLifecycle::Summary {
            token,
            summary,
            shadowed_positions,
        } => CompactionLifecycleView::Summary {
            token: token.as_str().to_string(),
            summary: summary.clone(),
            shadowed_positions: shadowed_positions.clone(),
        },
        CompactionLifecycle::End { token, outcome } => {
            CompactionLifecycleView::End {
                token: token.as_str().to_string(),
                outcome: *outcome,
            }
        }
    }
}
