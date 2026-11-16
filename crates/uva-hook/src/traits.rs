//! The four interception traits a registration implements.
//!
//! The decision vocabulary is [`uva_steering::HookDecision`] — the
//! same five-way vocabulary the steering hooks use; this crate adds
//! no second ruling vocabulary.

use async_trait::async_trait;
use uva_steering::{HookDecision, HookEvent, RequestParams};
use uva_tool::ToolOutput;

use crate::context::HookContext;

/// An observer: sees a hook event and may veto the run.
///
/// An observer is the read-mostly capability: it sees what happens
/// and can block or end the run, but it holds no state the loop
/// depends on.
#[async_trait]
pub trait Hook: Send + Sync {
    /// The registration's name, for diagnostics.
    fn name(&self) -> &str;

    /// Observe one event.
    async fn observe(
        &self,
        event: &HookEvent,
        ctx: &HookContext<'_>,
    ) -> HookDecision;
}

/// A gate: decides whether a call may proceed.
///
/// A gate is the security capability: it may block a call or ask for
/// an approval. It can also fail — `Err` means the gate could not
/// answer, and the dispatcher treats that as a block.
#[async_trait]
pub trait Gate: Send + Sync {
    /// The registration's name, for diagnostics.
    fn name(&self) -> &str;

    /// Check one event.
    async fn check(
        &self,
        event: &HookEvent,
        ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String>;
}

/// Rewrites the request parameters before the request goes on the
/// wire.
///
/// The rewrite surface is [`RequestParams`]: the model, the
/// temperature, the max tokens, the tool choice and the tool
/// schemas. The transcript and the system prompt are not part of it.
#[async_trait]
pub trait RequestTransformer: Send + Sync {
    /// The registration's name, for diagnostics.
    fn name(&self) -> &str;

    /// Rewrite the parameters in place.
    async fn transform(
        &self,
        params: &mut RequestParams,
        ctx: &HookContext<'_>,
    ) -> Result<(), String>;
}

/// Rewrites a tool result after the tool returned.
///
/// Transformers run *before* the loop commits the result, so the
/// `tool_result` row and the history hold the rewritten text — what
/// the log says is what the model read. The loop still records a
/// transformed-result row when the aggregate output changed:
/// provenance ("a registered transformer altered this result before
/// it was committed"), not reconciliation.
#[async_trait]
pub trait ResultTransformer: Send + Sync {
    /// The registration's name, for diagnostics.
    fn name(&self) -> &str;

    /// Rewrite the output in place.
    async fn transform(
        &self,
        output: &mut ToolOutput,
        ctx: &HookContext<'_>,
    ) -> Result<(), String>;
}

// The debug-level lifecycle logger is `uva-steering`'s; the impl of
// this crate's trait lives here because neither the harness nor
// `uva-steering` may write it (the orphan rule: `Hook` is foreign to
// both, and `uva-steering` cannot depend on this crate without a
// cycle). The observer decides nothing — it always allows.
#[async_trait]
impl Hook for uva_steering::LoggingHook {
    fn name(&self) -> &str {
        "logging"
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        match event {
            HookEvent::BeforeRun { agent_name, .. } => {
                tracing::debug!(agent = %agent_name, "hook: agent start");
            }
            HookEvent::AfterRun { reason, .. } => {
                tracing::debug!(reason = %reason, "hook: agent end");
            }
            HookEvent::BeforeProviderCall {
                provider, model, ..
            } => {
                tracing::debug!(provider = %provider, model = %model, "hook: provider start");
            }
            HookEvent::AfterProviderCall {
                provider,
                model,
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                tracing::debug!(
                    provider = %provider,
                    model = %model,
                    total_tokens = prompt_tokens + completion_tokens,
                    "hook: provider end"
                );
            }
            HookEvent::BeforeTool { tool_name, .. } => {
                tracing::debug!(tool = %tool_name, "hook: tool start");
            }
            HookEvent::AfterTool {
                tool_name,
                is_error,
                ..
            } => {
                tracing::debug!(tool = %tool_name, is_error, "hook: tool executed");
            }
            HookEvent::OnError { reason, .. } => {
                tracing::debug!(error = %reason, "hook: error");
            }
            HookEvent::BeforeStep { .. }
            | HookEvent::BeforeRequest { .. }
            | HookEvent::AfterStep { .. }
            | HookEvent::OnCompaction { .. } => {}
        }
        HookDecision::Allow
    }
}
