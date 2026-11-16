//! The per-dispatch context a registration sees.

use std::sync::Arc;

use uva_context::AgentState;
use uva_core::CancelToken;

/// The per-dispatch context a registration sees.
///
/// Built by the agent loop at each boundary from the run's own
/// state, so a registration table holds no run state of its own: the
/// same installed registrations can serve two runs without sharing a
/// counter.
///
/// The context deliberately carries no sink handle: a component that
/// writes durable rows (the loop's `hook_block` veto row) receives
/// its writer as an explicit argument, so the contract a registration
/// sees is only observable run state — no host-private type rides
/// along (ADR 0004).
pub struct HookContext<'a> {
    /// The run's agent state (counters, utilisation, decided calls).
    pub state: &'a AgentState,
    /// The run's cancellation token.
    pub cancel: &'a Arc<dyn CancelToken>,
}
