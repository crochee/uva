//! The loop's seam layer: one named method per interception point,
//! dispatched through the host's one registry.
//!
//! # [`Seams`](crate::seams::Seams): the loop's only dispatch surface
//!
//! The loop holds one [`Seams`](crate::seams::Seams) and names only its methods:
//! `step_pre`, `step_post`, `tool_pre`, `tool_post`, the `llm_*`
//! points, the five observation methods (`run_started`,
//! `run_finished`, `provider_started`, `provider_failed`,
//! `compacted`) and the scoped read helpers (`prompt_sections`,
//! `hints`, `tracker_table`, `tools`). It never names [`Host`] or
//! builds a [`HookEvent`](uva_hook::HookEvent) except through these methods.
//!
//! Every method dispatches through
//! [`HookRegistry::dispatch_at`](uva_hook::HookRegistry::dispatch_at)
//! on the registry the host carries — one entry point for the six
//! pass points and the five observation points. At a point the plugin
//! lane runs before the in-process lane, registration order inside a
//! lane; a denial short-circuits, `Ask` reasons collect, and a
//! `Block` at `tool/pre-execute` makes this layer write the durable
//! `hook_block` row. The registry's facts become `hook` warnings on
//! the run's event stream here, where the run's sink lives.
//!
//! An empty registry exits before the event is even built, so a bare
//! run pays nothing for the spine.
//!
//! # Registration and lifecycle
//!
//! Registrations go through the
//! [`HookRegistry`](uva_hook::HookRegistry) — the deployment's
//! one table, published as the `hooks` service — and arrive here on
//! the host the run receives:
//!
//! ```ignore
//! let registry = HookRegistry::new();
//! let _registration = registry.register_gate_at(
//!     MountPoint::ToolPre,
//!     gate,
//!     RegistrationOrigin::Plugin,
//! );
//! let agent = ReActAgent::new(provider, tools).with_host(Host::new_with_registry(registry))?;
//! ```
//!
//! A registration's lifetime is its handle: dropping the last
//! [`HookRegistration`](uva_hook::HookRegistration) withdraws the row
//! from every run that shares the table.

use std::{path::PathBuf, sync::Arc};

use uva_context::AgentState;
use uva_events::WarningKind;
use uva_hook::{HookRegistry, MountPoint};
use uva_steering::{
    Action,
    GuardOutcome,
    HintMessage,
    HookEvent,
    InjectionPoint,
    RequestParams,
    tracker::Tracker,
};
use uva_tool::{Tool, ToolOutput, ToolRegistry};

use crate::agent::{
    EventSink,
    host::{
        Arbitration,
        Diagnostic,
        HookContext,
        Host,
        PromptSection,
        veto_row,
    },
};

// The `hook_block` row one veto owes: who refused, why, and the call
// it refused. See `veto_row` for the filter.
#[derive(Clone)]
pub(crate) struct VetoRow {
    pub(crate) name: String,
    pub(crate) reason: String,
    pub(crate) tool_name: String,
    pub(crate) call_id: String,
}

impl VetoRow {
    // Record the row through the run's sink.
    pub(crate) fn record(&self, sink: &EventSink) {
        sink.append_hook_block(
            &self.name,
            &self.reason,
            &self.tool_name,
            &self.call_id,
        );
    }
}

#[derive(Clone)]
/// The loop's seam layer: one named method per interception point.
///
/// Clone is cheap and shares one policy table, so every run of an agent
/// sees the same registrations. The loop reads this type and no other
/// interception surface.
pub struct Seams {
    host: Host,
}

impl Seams {
    #[must_use]
    /// Wrap an installed host policy table.
    ///
    /// The interception table is the host's: `Seams` reads it from
    /// there on every pass, so a registration added to the same
    /// registry after this call is visible to the next pass.
    pub fn new(host: Host) -> Self {
        Self { host }
    }

    #[must_use]
    /// The interception registry this layer dispatches.
    pub fn hook_registry(&self) -> &HookRegistry {
        self.host.hook_registry()
    }

    #[must_use]
    /// The production policy over an empty registry: defensive guards,
    /// budget / iteration / truncation hints, adaptive concurrency and
    /// the debug logging hook, exactly as [`Host::default_policy`]
    /// installs them.
    pub fn default_policy(workspace: impl Into<PathBuf>) -> Self {
        Self::new(Host::default_policy(workspace, HookRegistry::new()))
    }

    #[must_use]
    /// The host this layer wraps: the typed tables and the registry.
    pub fn host(&self) -> &Host {
        &self.host
    }

    #[must_use = "a refused bind must be reported, not swallowed"]
    /// Bind the wrapped table to an agent's tool registry.
    ///
    /// Same contract as [`Host::bind`]: returns what *this call*
    /// refused, so the caller reports it.
    pub fn bind(&self, registry: Arc<ToolRegistry>) -> Vec<Diagnostic> {
        self.host.bind(registry)
    }

    /// `agent/step-pre`: before the step samples the model.
    ///
    /// A `Terminate` or a fail-closed `Block` short-circuits the step —
    /// the model is not called, and the loop ends the run with the
    /// decision's reason as its result.
    pub async fn step_pre(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        agent_name: &str,
        iteration: u32,
    ) -> Arbitration {
        self.dispatch(MountPoint::StepPre, ctx, sink, || {
            HookEvent::BeforeStep {
                agent_name: agent_name.to_string(),
                run_idx: 0,
                iteration,
            }
        })
        .await
        .arbitration
    }

    /// `agent/step-post`: after the step committed its tool results.
    ///
    /// The pass observes a step that already ran; a `Terminate` ends the
    /// run, and everything else leaves the result unchanged.
    pub async fn step_post(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        agent_name: &str,
        iteration: u32,
    ) -> Arbitration {
        self.dispatch(MountPoint::StepPost, ctx, sink, || {
            HookEvent::AfterStep {
                agent_name: agent_name.to_string(),
                run_idx: 0,
                iteration,
            }
        })
        .await
        .arbitration
    }

    /// `tool/pre-execute`: before a tool runs.
    ///
    /// The guard pipeline is the sibling
    /// [`tool_guards`](Self::tool_guards) call; the loop runs it after
    /// this seam accepted the call so an approval request names the
    /// effective arguments.
    pub async fn tool_pre(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        tool_name: &str,
        call_id: &str,
        arguments: &serde_json::Value,
    ) -> Arbitration {
        self.dispatch(MountPoint::ToolPre, ctx, sink, || {
            HookEvent::BeforeTool {
                tool_name: tool_name.to_string(),
                call_id: call_id.to_string(),
                arguments: arguments.clone(),
            }
        })
        .await
        .arbitration
    }

    /// `tool/post-execute`: after a tool returned.
    ///
    /// The result-rewrite half of the same seam is
    /// [`tool_result_transformers`](Self::tool_result_transformers) and
    /// [`output_transformer`](Self::output_transformer); they run before
    /// the loop commits, so the model reads the rewritten value.
    pub async fn tool_post(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        tool_name: &str,
        call_id: &str,
        is_error: bool,
    ) -> Arbitration {
        self.dispatch(MountPoint::ToolPost, ctx, sink, || {
            HookEvent::AfterTool {
                tool_name: tool_name.to_string(),
                call_id: call_id.to_string(),
                is_error,
            }
        })
        .await
        .arbitration
    }

    /// `llm/request`: rewrite the request parameters before they go on
    /// the wire.
    ///
    /// A hook's `ReplaceParams` replaces the snapshot, then the
    /// registered request transformers chain over it. The message list is
    /// not part of the rewrite surface. Returns the arbitration for the
    /// caller's diagnostics; the loop writes `params` back itself.
    pub async fn llm_request(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        provider: &str,
        params: &mut RequestParams,
    ) -> Arbitration {
        let pass = self
            .dispatch(MountPoint::LlmRequest, ctx, sink, || {
                HookEvent::BeforeRequest {
                    provider: provider.to_string(),
                    params: params.clone(),
                }
            })
            .await;
        // The effective event carries every rewrite — a hook's
        // `ReplaceParams`, a transformer's — so the pass result is what
        // goes on the wire.
        if let Some(HookEvent::BeforeRequest {
            params: rewritten, ..
        }) = pass.effective
        {
            *params = rewritten;
        }
        pass.arbitration
    }

    /// `llm/result`: observe the provider's answer after it was
    /// assembled.
    ///
    /// Observational in the current steering vocabulary: a decision the
    /// point does not accept is recorded and ignored.
    #[allow(
        clippy::too_many_arguments,
        reason = "the sink is explicit (ADR 0004); the other parameters are the AfterProviderCall event's own fields"
    )]
    pub async fn llm_result(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        provider: &str,
        model: &str,
        prompt_tokens: usize,
        completion_tokens: usize,
        stop_reason: Option<String>,
    ) -> Arbitration {
        self.dispatch(MountPoint::LlmResult, ctx, sink, || {
            HookEvent::AfterProviderCall {
                provider: provider.to_string(),
                model: model.to_string(),
                prompt_tokens,
                completion_tokens,
                stop_reason,
            }
        })
        .await
        .arbitration
    }

    /// The `before_run` observation point: fired once per run, before the
    /// first step.
    pub async fn run_started(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        agent_name: &str,
        run_idx: u32,
    ) -> Arbitration {
        self.dispatch(MountPoint::BeforeRun, ctx, sink, || {
            HookEvent::BeforeRun {
                agent_name: agent_name.to_string(),
                run_idx,
            }
        })
        .await
        .arbitration
    }

    /// The `after_run` observation point: the run has ended, so a
    /// decision is recorded and ignored.
    pub async fn run_finished(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        agent_name: &str,
        run_idx: u32,
        reason: &str,
    ) -> Arbitration {
        self.dispatch(MountPoint::AfterRun, ctx, sink, || HookEvent::AfterRun {
            agent_name: agent_name.to_string(),
            run_idx,
            reason: reason.to_string(),
        })
        .await
        .arbitration
    }

    /// The `before_provider_call` observation point: right before the
    /// dispatch.
    pub async fn provider_started(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        provider: &str,
        model: &str,
        estimated_tokens: usize,
    ) -> Arbitration {
        self.dispatch(MountPoint::BeforeProviderCall, ctx, sink, || {
            HookEvent::BeforeProviderCall {
                provider: provider.to_string(),
                model: model.to_string(),
                estimated_tokens,
            }
        })
        .await
        .arbitration
    }

    /// The `on_error` observation point, fired before a run ends with an
    /// error reason.
    pub async fn provider_failed(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        agent_name: &str,
        reason: &str,
    ) -> Arbitration {
        self.dispatch(MountPoint::OnError, ctx, sink, || HookEvent::OnError {
            agent_name: agent_name.to_string(),
            reason: reason.to_string(),
        })
        .await
        .arbitration
    }

    /// The `on_compaction` observation point.
    pub async fn compacted(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        start: usize,
        end: usize,
        summary: &str,
    ) -> Arbitration {
        self.dispatch(MountPoint::OnCompaction, ctx, sink, || {
            HookEvent::OnCompaction {
                start,
                end,
                summary: summary.to_string(),
            }
        })
        .await
        .arbitration
    }

    #[must_use]
    /// The prompt sections the run's manifest carries, in registration
    /// order.
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        self.host.hook_registry().prompt_sections()
    }

    #[must_use]
    /// The hints that fire for this state, in priority order.
    pub fn hints(
        &self,
        state: &AgentState,
    ) -> Vec<(InjectionPoint, HintMessage)> {
        self.host.hints(state)
    }

    #[must_use]
    /// The tracker table, for the loop's observation boundaries.
    pub fn tracker_table(&self) -> Vec<Arc<dyn Tracker>> {
        self.host.snapshot().trackers.clone()
    }

    #[must_use]
    /// The registered tools, in registration order.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.host.tools()
    }

    #[must_use]
    /// Whether any guard is registered, for the loop's zero-guard fast
    /// path.
    pub fn has_guards(&self) -> bool {
        !self.host.snapshot().guards.is_empty()
    }

    /// `tool/pre-execute`'s guard pipeline: registration order, first
    /// denial wins.
    pub fn tool_guards(
        &self,
        action: &Action,
        ctx: &HookContext<'_>,
    ) -> GuardOutcome {
        self.host.dispatch_guards(action, ctx)
    }

    #[must_use]
    /// Whether any result transformer is registered, for the loop's
    /// no-rewrite fast path.
    pub fn has_tool_result_transformers(&self) -> bool {
        self.host.hook_registry().has_result_transformers()
    }

    /// `tool/post-execute`'s result transformers, in table order. Each
    /// sees what the previous one left; the names that ran are
    /// returned when the aggregate output changed.
    pub async fn tool_result_transformers(
        &self,
        ctx: &HookContext<'_>,
        output: &mut ToolOutput,
    ) -> Vec<String> {
        self.host
            .hook_registry()
            .transform_result(output, ctx)
            .await
    }

    /// `tool/post-execute`'s output transformers over the result text.
    pub async fn output_transformer(
        &self,
        text: String,
        tool_name: &str,
        state: &AgentState,
    ) -> String {
        self.host.transform_output(text, tool_name, state).await
    }

    #[must_use]
    /// The `AppendToToolResult` hints that name this tool.
    pub fn tool_result_hints(
        &self,
        tool_name: &str,
        state: &AgentState,
    ) -> Vec<String> {
        self.host.tool_result_hints(tool_name, state)
    }

    // One dispatch pass, for every mount point.
    //
    // The registry is read from the host, and the event is built
    // lazily: a point with no registration at all pays nothing for the
    // clone. The facts the pass learned become `hook` warnings on the
    // run's stream, and a decided pass at `tool/pre-execute` writes the
    // `hook_block` row it owes — both here, where the run's sink lives
    // (ADR 0004).
    async fn dispatch(
        &self,
        mount: MountPoint,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        build: impl FnOnce() -> HookEvent,
    ) -> PassOutcome {
        let registry = self.host.hook_registry();
        if !registry.has_entries_at(mount) {
            return PassOutcome {
                arbitration: Arbitration::default(),
                effective: None,
            };
        }
        let event = build();
        let pass = registry.dispatch_at(mount, &event, ctx).await;
        // What the pass learned about the registrations that ran: a
        // gate that could not answer where no denial exists, a
        // decision the point did not accept, a registration that
        // panicked. They become `hook` warnings on the run's event
        // stream, which the session controller persists like every
        // other warning, so an operator reading the session sees the
        // registration that misbehaved.
        for fact in &pass.facts {
            sink.warn(WarningKind::Hook, None, fact.message().to_owned());
        }
        let Some(rewritten) = pass.event else {
            let name = pass.denied_by.as_deref().unwrap_or("hook registry");
            if let Some(veto) =
                veto_row(name, &pass.arbitration.decision, &event)
            {
                veto.record(sink);
            }
            return PassOutcome {
                arbitration: pass.arbitration,
                effective: None,
            };
        };
        PassOutcome {
            arbitration: pass.arbitration,
            effective: Some(rewritten),
        }
    }
}

// What one dispatch pass produced: the arbitration, and the effective
// event — `None` when the pass was decided or nothing was registered.
struct PassOutcome {
    arbitration: Arbitration,
    effective: Option<HookEvent>,
}

#[cfg(test)]
mod tests;
