//! The compaction pass at a step boundary.
//!
//! The planning and the durable rows live in `elysper-session`
//! ([`elysper_session::plan_compaction`] / `begin_compaction` /
//! `finish_compaction`) so a host's manual compaction writes exactly
//! the same replace the loop does. The harness keeps what is its own:
//! when a pass may run at all, the live `CompactionStarted` /
//! `CompactionFinished` events, and re-deriving the transcript after
//! the replace is durable.
//!
//! The plan is made over the transcript the loop already holds
//! ([`RunState::messages`], index-parallel with its provenance), so a
//! pass costs no log read; only a committed replace is written.
//!
//! The harness also cannot enforce pruning for a custom [`Compactor`]:
//! `Compactor::summarize` owns the request it sends. The default
//! `SummaryCompactor` prunes through `Compactor::prune` before its
//! provider call; a custom implementation that ignores `prune` sends
//! the raw span.
//!
//! [`Compactor`]: elysper_session::Compactor

use std::sync::Arc;

use elysper_provider::Message;
use elysper_session::{
    CompactionError,
    CompactionLifecycleOutcome,
    CompactionOutcome,
    CompactionPass,
    CompactionPlan,
    Compactor,
    PlannedCompaction,
    SessionError,
    TokenBudget,
    plan_compaction,
};
use tracing::{debug, warn};

use super::ReActLoop;
use crate::{
    agent::re_act::state::RunState,
    events::{SessionEndReason, SystemEvent, WarningKind},
};

// The reason a compaction pass names in its log-only rows.
const COMPACT_REASON: &str = "context_pressure";

// One compaction pass at a step boundary: plan over the in-memory
// transcript, summarise, write the durable replace, splice the
// summary into the transcript. Only a writer-backed run compacts — a
// summary is a `Replace` row, and a run with no log has no surface to
// rewrite. The compactor itself is optional; without one this is a
// no-op.
pub(super) async fn compact(
    this: &ReActLoop,
    session: &mut RunState,
    iteration: usize,
) -> Result<(), SessionEndReason> {
    let Some(compactor) = this.caps.control.compactor.as_ref() else {
        return Ok(());
    };
    // A suspended run waits for a decision; its resume token still
    // points at the span a replace would shadow.
    if !session.pending_approvals.is_empty() {
        return Ok(());
    }
    let Some(writer) = this.caps.env.log_writer.as_ref() else {
        return Ok(());
    };
    let Some(plan) = plan_pass(
        &session.messages,
        &session.surface_seqs,
        session.last_compaction,
        compactor.as_ref(),
        this.cfg.context_window,
        iteration,
    ) else {
        return Ok(());
    };
    let token = elysper_session::begin_compaction(
        writer.as_ref(),
        &plan,
        COMPACT_REASON,
    )
    .await
    .map_err(log_failure)?;
    this.sink.system(SystemEvent::CompactionStarted {
        reason: COMPACT_REASON.to_string(),
        start: plan.range.start,
        end: plan.range.end,
        first_seq: plan.shadowed_seqs.first().copied(),
        last_seq: plan.shadowed_seqs.last().copied(),
    });
    let cancel = Arc::clone(&session.cancel);
    let summary = compactor.summarize(&plan, cancel).await;
    finish(FinishArgs {
        this,
        session,
        writer: writer.as_ref(),
        token: &token,
        plan: &plan,
        summary,
        iteration,
    })
    .await
}

// The pieces one pass's terminal step needs, bundled so `compact`
// stays inside the shape budget.
struct FinishArgs<'a> {
    this: &'a ReActLoop,
    session: &'a mut RunState,
    writer: &'a dyn elysper_session::SessionLog,
    token: &'a elysper_session::SurfaceToken,
    plan: &'a CompactionPlan,
    summary: Result<CompactionOutcome, CompactionError>,
    iteration: usize,
}

// Write the pass's terminal rows, publish `CompactionFinished`, and
// re-derive the transcript on success.
async fn finish(
    FinishArgs {
        this,
        session,
        writer,
        token,
        plan,
        summary,
        iteration,
    }: FinishArgs<'_>,
) -> Result<(), SessionEndReason> {
    match summary {
        Ok(outcome) => {
            let seq = elysper_session::finish_compaction(
                writer,
                token,
                COMPACT_REASON,
                plan,
                CompactionPass::Committed(&outcome),
            )
            .await
            .map_err(log_failure)?;
            // The replace is durable now, so the transcript can
            // carry it: the summary takes the plan's span and the
            // compaction row's ordinal takes the matching provenance
            // slot. The next plan therefore sees the same positions
            // the fold would reproduce, with no log read.
            if let Some(seq) = seq {
                let start = plan.range.start;
                let end = plan.range.end;
                let mut next = (*session.messages).clone();
                next.splice(
                    start..end,
                    [Message::assistant(outcome.summary.clone())],
                );
                session.messages = Arc::new(next);
                session.surface_seqs.splice(start..end, [seq]);
                session.last_compaction = Some(seq);
            }
            this.sink.system(SystemEvent::CompactionFinished {
                reason: COMPACT_REASON.to_string(),
                shadowed_messages: plan.range.len(),
                shadowed_tokens: plan.shadowed_tokens,
                outcome: CompactionLifecycleOutcome::Committed,
                first_seq: plan.shadowed_seqs.first().copied(),
                last_seq: plan.shadowed_seqs.last().copied(),
            });
            debug!(
                iteration,
                shadowed_messages = plan.range.len(),
                shadowed_tokens = plan.shadowed_tokens,
                "compaction: the summary replaced its span"
            );
            Ok(())
        }
        Err(error) => fail(this, writer, token, plan, error, iteration).await,
    }
}

// A failed summariser is not fatal: record the audit end row, warn, and
// let the turn continue uncompacted.
async fn fail(
    this: &ReActLoop,
    writer: &dyn elysper_session::SessionLog,
    token: &elysper_session::SurfaceToken,
    plan: &CompactionPlan,
    error: CompactionError,
    iteration: usize,
) -> Result<(), SessionEndReason> {
    warn!(
        iteration,
        %error,
        "compaction failed; continuing without it"
    );
    // A cancelled summariser is an interrupted attempt, not a
    // provider failure.
    let lifecycle = if error == CompactionError::Cancelled {
        CompactionLifecycleOutcome::Interrupted
    } else {
        CompactionLifecycleOutcome::Failed
    };
    elysper_session::finish_compaction(
        writer,
        token,
        COMPACT_REASON,
        plan,
        CompactionPass::Failed {
            error: &error.to_string(),
            outcome: lifecycle,
        },
    )
    .await
    .map_err(log_failure)?;
    if lifecycle == CompactionLifecycleOutcome::Failed {
        this.sink.warn(
            WarningKind::ContextCompaction,
            Some(iteration),
            format!("compaction failed: {error}"),
        );
    }
    this.sink.system(SystemEvent::CompactionFinished {
        reason: COMPACT_REASON.to_string(),
        shadowed_messages: 0,
        shadowed_tokens: 0,
        outcome: lifecycle,
        first_seq: None,
        last_seq: None,
    });
    Ok(())
}

// Plan against the fold. `None` means this step does not compact —
// the log does not fold, the compactor declined, the span is empty, or
// the only shadowable row is the previous summary.
fn plan_pass(
    history: &[Message],
    surface_seqs: &[u64],
    last_compaction: Option<u64>,
    compactor: &dyn Compactor,
    context_window: usize,
    iteration: usize,
) -> Option<CompactionPlan> {
    let budget = TokenBudget::for_window(context_window);
    let plan = match plan_compaction(history, surface_seqs, compactor, budget) {
        Ok(PlannedCompaction::Plan(plan)) => plan,
        Ok(PlannedCompaction::Declined | PlannedCompaction::EmptySpan) => {
            return None;
        }
        Err(error) => {
            warn!(
                iteration,
                %error,
                "compaction skipped: the log cannot be planned over"
            );
            return None;
        }
    };
    // No progress: the only row the plan could shadow is the summary
    // the last pass wrote. Summarising it again costs a provider call
    // and loses detail for nothing, so the pass is skipped.
    if repeats_previous_summary(&plan, last_compaction) {
        debug!(
            iteration,
            "compaction skipped: the only shadowable row is the last summary"
        );
        return None;
    }
    Some(plan)
}

fn log_failure(error: SessionError) -> SessionEndReason {
    SessionEndReason::Error(format!("session log write failed: {error}"))
}

// `true` when every seq the plan shadows is the row of the previous
// compaction's summary — the only case where a pass would rewrite a
// summary without shadowing anything new.
fn repeats_previous_summary(
    plan: &CompactionPlan,
    last_compaction: Option<u64>,
) -> bool {
    let Some(last) = last_compaction else {
        return false;
    };
    !plan.shadowed_seqs.is_empty()
        && plan.shadowed_seqs.iter().all(|seq| *seq == last)
}

#[cfg(test)]
mod tests;
