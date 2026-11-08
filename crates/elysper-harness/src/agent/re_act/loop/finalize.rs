//! Post-loop finalize: the max-iterations warning, the terminal `SessionEnded` emission, and output assembly.
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
            let mut event = elysper_session::turn_end(
                super::TURN_ORDINAL,
                end_reason.as_str(),
            );
            // A failed turn's message is part of the outcome. Without
            // it the log says only "error", and every reader — the
            // page's banner, an operator reading the log — has to
            // guess why the run stopped.
            if let (
                SessionEndReason::Error(message),
                elysper_session::SessionEvent::Turn { data, .. },
            ) = (&end_reason, &mut event)
            {
                data["message"] = serde_json::Value::String(message.clone());
            }
            self.emit_typed(event);
        }
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
        // `run_finished` is observational: the run has ended, so the
        // point accepts no decision — a handler that returns one is
        // recorded as a diagnostic.
        let ctx = self.hook_context(&session.agent_state);
        self.caps
            .control
            .seams
            .run_finished(
                &ctx,
                &self.caps.model.descriptor.name,
                0,
                end_reason.as_str(),
            )
            .await;
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
