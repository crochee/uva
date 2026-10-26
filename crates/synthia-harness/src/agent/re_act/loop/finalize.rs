//! Post-loop finalize: the max-iterations warning, checkpoint persistence, the terminal `SessionEnded` emission, and output assembly.
use synthia_core::Clock;
use synthia_steering::HookEvent;

use super::{super::state::RunState, ReActLoop};
use crate::events::{SessionEndReason, WarningKind};

impl ReActLoop {
    pub(super) async fn drive_finalize(
        self,
        session: RunState,
        exhausted: bool,
    ) {
        let mut end_reason = session.end_reason.clone();
        if exhausted {
            tracing::warn!(
                "hit max_iterations ({})",
                self.caps.control.max_iterations
            );
            self.sink.warn(
                WarningKind::Loop,
                None,
                format!(
                    "hit max_iterations ({})",
                    self.caps.control.max_iterations
                ),
            );
            end_reason = SessionEndReason::MaxIterations;
        }
        // Close the turn this run opened. A suspended run already
        // wrote its own `turn_end_suspended` at the suspension
        // boundary, and a run that never opened an ordinary turn (a
        // refused resume continues the suspended one) has nothing to
        // close here.
        if self.turn_open && !matches!(end_reason, SessionEndReason::Suspended)
        {
            self.emit_typed(synthia_session::turn_end(
                super::TURN_ORDINAL,
                end_reason.as_str(),
            ));
        }
        self.persist_checkpoint(&session, &end_reason);
        // The last flush: a run that ends (completed, cancelled or
        // failed) must not leave a row in the buffer. The rows of
        // the final step are only durable once they are written.
        if let Err(reason) = self.flush_log().await {
            tracing::warn!(
                end_reason = ?reason,
                "final log flush failed; the run's last rows may be missing"
            );
        }
        tracing::info!(
            end_reason = ?end_reason,
            history_len = session.messages.len(),
            "session end: finalize()"
        );
        self.warn_missing_structured_output(&session, &end_reason);
        self.sink.end(end_reason.clone());
        // `AfterRun` is observational: the run has ended, so the point
        // accepts no decision — a handler that returns one is
        // recorded as a diagnostic.
        super::hooks::dispatch_hook(&self.caps.control, || {
            HookEvent::AfterRun {
                agent_name: self.caps.model.descriptor.name.clone(),
                run_idx: 0,
                reason: end_reason.as_str().to_string(),
            }
        })
        .await;
    }

    fn persist_checkpoint(
        &self,
        session: &RunState,
        end_reason: &SessionEndReason,
    ) {
        let Some(checkpointer) = self.caps.env.checkpointer.as_ref() else {
            return;
        };
        let Some(key) = session.resume_key.as_ref() else {
            return;
        };
        match end_reason {
            SessionEndReason::Completed => {
                if let Err(err) = checkpointer.delete(key) {
                    tracing::warn!(
                        key = %key,
                        error = %err,
                        "checkpoint delete failed"
                    );
                }
            }
            // A log-backed run is resumed from the log, never from a
            // snapshot: saving one would write a transcript no reader
            // consumes. Completed still deletes the key above.
            _ if session.history.is_some() => {
                tracing::debug!(
                    key = %key,
                    "checkpoint save skipped: the session log owns the transcript"
                );
            }
            other => {
                let (thread_id, agent_id) = split_resume_key(key);
                let snapshot = crate::agent::checkpoint::CheckpointSnapshot::at(
                    thread_id,
                    agent_id,
                    session.agent_state.iteration_count,
                    session.messages.clone(),
                    self.caps.env.clock.now(),
                );
                if let Err(err) = checkpointer.save(key, &snapshot) {
                    tracing::warn!(
                        key = %key,
                        error = %err,
                        "checkpoint save failed"
                    );
                } else {
                    tracing::info!(
                        key = %key,
                        reason = ?other,
                        "checkpoint saved for resume"
                    );
                }
            }
        }
    }

    fn warn_missing_structured_output(
        &self,
        session: &RunState,
        end_reason: &SessionEndReason,
    ) {
        if self.caps.model.descriptor.output_schema.is_none() {
            return;
        }
        if !matches!(
            end_reason,
            SessionEndReason::Completed | SessionEndReason::MaxIterations
        ) {
            return;
        }
        if session.structured_output {
            return;
        }
        self.sink.warn(
            WarningKind::StructuredOutput,
            None,
            "the run declared an output schema but ended \
             without a schema-valid `structured_output` \
             submission; the typed result is absent"
                .to_string(),
        );
    }
}

fn split_resume_key(key: &str) -> (String, String) {
    match key.split_once(':') {
        Some((thread, agent)) => (thread.to_string(), agent.to_string()),
        None => (key.to_string(), String::new()),
    }
}
