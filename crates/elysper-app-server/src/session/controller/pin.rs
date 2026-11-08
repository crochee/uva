//! Per-turn provider selection: the pin that travels with a
//! submitted operation.
//!
//! ## Why the pin rides on the operation
//!
//! The provider a run samples with lives in
//! [`RunDependencies`](super::RunDependencies), which is owned by the
//! controller's inner state and is not reachable from a route
//! handler. The one thing a route *can* hand the controller is an
//! operation, so the selection is submitted alongside it:
//!
//! ```text
//! route: resolve `model` ─▶ Arc<dyn ModelProvider>
//!                          │
//!                          ▼
//! SessionController::submit_with_provider(op, PinnedProvider::new(provider))
//!                          │
//!                          ▼
//! dispatch loop: park the pin on the inner state
//!                          │
//!                          ▼
//! maybe_start_run: consume the pin, build AgentRunConfig { provider: … }
//!                          │
//!                          ▼
//!                          except on a resume: the pin stays parked for
//!                          the next turn, because the suspended envelope
//!                          was recorded under the default provider
//! ```
//!
//! The pin is *consumed* by the run it starts, which buys two
//! properties a sticky swap on the controller could not:
//!
//! - it is per turn, not per session — the next turn without a
//!   selection runs the deployment default again;
//! - two operations queued behind a running turn each keep their own
//!   provider, so neither can swap the other's provider out.
//!
//! `SessionController::submit` is unchanged and means "no pin": the
//! session's configured default provider runs the turn.

use std::sync::Arc;

use elysper::provider::traits::ModelProvider;
use tokio::sync::oneshot;

use super::ops::SessionOp;

/// A provider selected for one turn.
pub struct PinnedProvider {
    provider: Arc<dyn ModelProvider>,
    /// The catalogue key the selection named, when the submitter
    /// resolved it. The handle's `name()` is the model for an
    /// OpenAI-compatible provider, so the id has to travel beside
    /// it.
    id: Option<String>,
}

impl PinnedProvider {
    /// Pin `provider` for the operation this is submitted with.
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self { provider, id: None }
    }

    /// Name the catalogue entry this pin resolved.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The catalogue key, when the submitter knew it.
    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(super) fn take(self) -> Arc<dyn ModelProvider> {
        self.provider
    }

    /// The model the pin runs.
    pub(crate) fn model_name(&self) -> String {
        self.provider.model_config().name
    }
}

/// What the controller loop reports back about a submitted operation.
///
/// The submitter waits for it when the operation's durable effect has
/// to be on disk before the caller answers — a park's
/// `parked_prompt` row, a feedback row, a cancel's
/// `park_consumed` rows.
#[derive(Debug)]
pub(super) enum OpAck {
    /// The operation is handled, and the row it writes — if it writes
    /// one — is on disk.
    Durable,
    /// The operation could not make its row durable; the message
    /// names the failure.
    Failed(String),
}

/// One entry on the controller's operation channel: the operation
/// plus the turn's provider selection, when the submitter made one.
///
/// A single envelope rather than a second channel, so an operation and
/// its pin can never be reordered against each other.
pub(super) struct SubmittedOp {
    pub(super) op: SessionOp,
    pub(super) provider: Option<PinnedProvider>,
    /// The submitter's ack channel: `Some` when the caller waits for
    /// the operation's durable effect. The loop answers it once the
    /// operation's row is on disk (or with the failure that stopped
    /// it).
    pub(super) ack: Option<oneshot::Sender<OpAck>>,
}

impl SubmittedOp {
    /// The turn's selection, if any. `None` keeps the session's
    /// configured default provider.
    pub(super) fn into_parts(
        self,
    ) -> (
        SessionOp,
        Option<PinnedProvider>,
        Option<oneshot::Sender<OpAck>>,
    ) {
        (self.op, self.provider, self.ack)
    }
}
