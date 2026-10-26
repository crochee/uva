//! The suspension boundary: the rows a suspended run leaves behind
//! and the `Suspended` event that carries the resume token.
//!
//! A suspension is not a crash. The approval rows and the boundary
//! rows are written and flushed before the event goes out, so the
//! operator's decision is made against a log that already says which
//! calls are waiting.

use super::ReActLoop;
use crate::{
    agent::{
        approval::{ResumeToken, Suspension},
        re_act::state::RunState,
    },
    events::{SessionEndReason, SystemEvent},
};

pub(super) async fn boundary(
    this: &mut ReActLoop,
    session: &mut RunState,
) -> Result<(), SessionEndReason> {
    let requests = std::mem::take(&mut session.pending_approvals);
    let pending: Vec<String> = requests
        .iter()
        .map(|request| request.call_id.clone())
        .collect();
    for request in &requests {
        this.sink.append_typed(synthia_session::approval_asked(
            &request.id,
            &request.call_id,
            &request.tool,
            &request.arguments_hash,
            request.risk.as_str(),
            &request.agent,
        ));
    }
    this.sink
        .append_typed(synthia_session::turn_suspended(pending.clone()));
    this.sink
        .append_typed(synthia_session::step_end_suspended());
    let last_seq = this
        .sink
        .append_typed(synthia_session::turn_end_suspended());
    // The sync barrier: the approval rows must survive the crash the
    // suspension is meant to survive.
    this.flush_log().await?;
    // The suspended end above is this turn's one closer. Clear the
    // open-turn flag so a later flush failure (which ends the run as
    // `Error`) cannot append a second, ordinary `turn_end` after it.
    this.turn_open = false;
    let hash = session.request_snapshot_hash.clone().unwrap_or_default();
    let suspension = Suspension::AwaitingApproval {
        interrupts: requests,
        resume: ResumeToken::new(last_seq, pending, hash),
    };
    this.sink.system(SystemEvent::Suspended(suspension));
    Ok(())
}
