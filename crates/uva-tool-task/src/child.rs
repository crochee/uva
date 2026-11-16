//! The child run: what every round of a delegated child shares.
//!
//! A round is one peer run inside a child session: the input the peer
//! is handed, the token that ends it, and the text the parent is told
//! about. The `task` tool drives the first round; the control surface
//! drives the follow-up and cold-wake rounds. The plumbing lives here
//! so both callers drive a child the same way.

use std::{pin::Pin, sync::Arc};

use futures::{StreamExt, future::select};
use uva_agent::{Agent, AgentInput, SubagentUsage};
use uva_core::{AtomicCancelToken, CancelToken};
use uva_events::{AgentEvent, SessionEndReason};
use uva_provider::ContentPart;

/// A child's cancellation token: the run's own token and the child's
/// own trigger — either of them ends the child's round.
///
/// The parent's token keeps its reach: cancelling the parent still
/// ends its children, exactly as before the control surface landed.
/// The trigger is the child's own: the control surface fires it to
/// stop one round without touching the rest of the session.
pub(crate) struct ChildCancel {
    parent: Arc<dyn CancelToken>,
    trigger: Arc<AtomicCancelToken>,
}

impl ChildCancel {
    /// Link a child's own trigger to the run's token.
    pub(crate) fn new(parent: Arc<dyn CancelToken>) -> Arc<Self> {
        Arc::new(Self {
            parent,
            trigger: Arc::new(AtomicCancelToken::new()),
        })
    }
}

impl CancelToken for ChildCancel {
    fn is_cancelled(&self) -> bool {
        self.parent.is_cancelled() || self.trigger.is_cancelled()
    }

    fn cancel(&self) {
        self.trigger.cancel();
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let parent = Arc::clone(&self.parent);
        let trigger = Arc::clone(&self.trigger);
        Box::pin(async move {
            select(parent.cancelled(), trigger.cancelled()).await;
        })
    }
}

impl std::fmt::Debug for ChildCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildCancel")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Drive one child run to its end and retain the last assistant text.
pub(crate) async fn drain_child(
    peer: Arc<dyn Agent>,
    input: AgentInput,
    cancel: Arc<dyn CancelToken>,
    usage: Option<&dyn SubagentUsage>,
    child_id: &str,
) -> (SessionEndReason, Option<String>) {
    drain_child_observed(peer, input, cancel, None, usage, child_id).await
}

/// Drive one child run to its end, reporting its activity to
/// `observer` when one is installed and its consumption to `usage`.
///
/// The dispatch that streams progress installs an
/// [`L1Observer`](crate::progress::L1Observer); the control surface's
/// follow-up rounds pass `None` — their progress belongs to the
/// `agents_message` call, not to the original `task` block.
///
/// `usage` is the deployment's account: the child runs on its own
/// harness, so its `Usage` events and its terminal event never reach
/// the parent's controller. Reporting them here is what keeps a
/// delegated round in `/api/v1/chat/usage`.
pub(crate) async fn drain_child_observed(
    peer: Arc<dyn Agent>,
    input: AgentInput,
    cancel: Arc<dyn CancelToken>,
    mut observer: Option<&mut crate::progress::L1Observer>,
    usage: Option<&dyn SubagentUsage>,
    child_id: &str,
) -> (SessionEndReason, Option<String>) {
    let mut stream = peer.run(input, cancel).await;
    let mut text = String::new();
    let mut end_reason = SessionEndReason::Completed;
    while let Some(event) = stream.next().await {
        if let Some(observer) = observer.as_deref_mut() {
            observer.observe(&event);
        }
        match &event {
            AgentEvent::Model(ContentPart::Text(part)) => {
                if !part.text.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&part.text);
                }
            }
            AgentEvent::System(uva_events::SystemEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            }) => {
                if let Some(usage) = usage {
                    usage.tokens(child_id, *input_tokens, *output_tokens);
                }
            }
            AgentEvent::System(uva_events::SystemEvent::SessionEnded {
                reason,
            }) => {
                if let Some(usage) = usage {
                    usage.round_ended(child_id);
                }
                end_reason = reason.clone();
            }
            _ => {}
        }
    }
    let text = if matches!(end_reason, SessionEndReason::Completed)
        && !text.trim().is_empty()
    {
        Some(text)
    } else {
        None
    };
    (end_reason, text)
}

/// How a child's round ended, as the roster reports it.
pub(crate) fn outcome_of(
    reason: &SessionEndReason,
) -> crate::control::RoundOutcome {
    use crate::control::RoundOutcome;
    match reason {
        SessionEndReason::Completed => RoundOutcome::Completed,
        SessionEndReason::Cancelled => RoundOutcome::Cancelled,
        _ => RoundOutcome::Failed,
    }
}
