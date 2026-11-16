//! The four loop seams, by the names middleware registers under.
//!
//! The loop exposes four interception points as kernel waterfalls. A
//! middleware registered on one of these keys observes, rewrites, or
//! short-circuits the pass — the same three powers the `Host`'s
//! registrations have always had:
//!
//! | Seam constant | Kernel key | `Host` event that dispatches there |
//! |---|---|---|
//! | [`STEP_PRE`](crate::seams::STEP_PRE) | `agent/step-pre` | `HookEvent::BeforeStep` |
//! | [`STEP_POST`](crate::seams::STEP_POST) | `agent/step-post` | `HookEvent::AfterStep` |
//! | [`TOOL_PRE`](crate::seams::TOOL_PRE) | `tool/pre-execute` | `HookEvent::BeforeTool` |
//! | [`TOOL_POST`](crate::seams::TOOL_POST) | `tool/post-execute` | `HookEvent::AfterTool` |
//!
//! Two more keys cover the pieces around the four: [`LLM_REQUEST`](crate::seams::LLM_REQUEST)
//! carries a request's parameters before they go on the wire,
//! [`LLM_RESULT`](crate::seams::LLM_RESULT) carries the provider's answer, and
//! [`PROMPT_SECTIONS`](crate::seams::PROMPT_SECTIONS) is the collection event a
//! plugin contributes a system-prompt section through.
//!
//! # [`Seams`](crate::seams::Seams): the loop's only dispatch surface
//!
//! The loop holds one [`Seams`](crate::seams::Seams) and names only its methods: `step_pre`,
//! `step_post`, `tool_pre`, `tool_post`, the `llm_*` points, and the
//! scoped read helpers (`prompt_sections`, `hints`, `tracker_table`,
//! `tools`). It never names [`Host`] or builds a `HookEvent`.
//!
//! Every seam method dispatches in two stages. First the external
//! [`HookRegistry`](uva_hook::HookRegistry) runs the registrations a
//! third-party plugin installed through the `hooks` service — observe,
//! rewrite, or decide; a denial short-circuits here, and a `Block` at
//! `tool/pre-execute` writes the durable `hook_block` row. Then the
//! point's kernel [`Waterfall`](uva_kernel::Waterfall) chain runs:
//! middleware registered through
//! [`Host::waterfall`](crate::agent::host::Host::waterfall) together
//! with the [`Host`]'s typed hooks, gates and transformers, with the
//! arbitration, fail-closed gate rules and panic isolation they have
//! always had, over the event the registry produced. Both stages fold
//! through one [`Arbitration`]:
//!
//! - the first `Block` short-circuits the pass — later registrations
//!   do not run;
//! - `Ask` reasons collect from both surfaces into one report;
//! - a `Terminate` is accepted only where `HookEvent::accepts` says so;
//! - once a denial is recorded nothing later can turn it back into
//!   permission (the deny-only terminal, ADR 0004).
//!
//! The pass payload is the contract crate's
//! [`HookPayload`](uva_hook::HookPayload); the keys and the
//! interception traits are `uva-hook`'s too, re-exported by this
//! module (ADR 0004). An empty registry plus no chain and no host
//! registration exits before the event is even built, so a bare run
//! pays nothing for the spine.
//!
//! # Registration and lifecycle
//!
//! External registrations go through [`HookRegistry`](uva_hook::HookRegistry)
//! and arrive as the `hooks` service the `agentLoop` factory injects;
//! typed registrations still go through [`Host`] (`hook`, `gate`,
//! `guard`, transformer, `prompt_section`, `tool`), which
//! the loop no longer reads. A deployment wraps an installed host with
//! [`Seams::new`](crate::seams::Seams::new), attaches the registry with
//! [`Seams::with_hook_registry`](crate::seams::Seams::with_hook_registry),
//! and installs the result with
//! [`ReActAgent::with_seams`](crate::ReActAgent::with_seams); the
//! legacy `ReActAgent::with_host` wraps a host for it.
//!
//! A registration's lifetime is its handle: dropping the last
//! [`Registration`] withdraws the typed row, and dropping the last
//! [`HookRegistration`](uva_hook::HookRegistration) withdraws the
//! external one — the same lifetime a kernel plugin's effect handle
//! owns.

use std::{
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use futures::FutureExt;
use uva_context::AgentState;
use uva_events::WarningKind;
// The mount-point keys are the contract crate's (ADR 0004); the deep
// paths `uva_harness::seams::<KEY>` keep working through this
// module-internal re-export, which is not a crate-root curated name.
pub use uva_hook::seam::{
    LLM_REQUEST,
    LLM_RESULT,
    PROMPT_SECTIONS,
    STEP_POST,
    STEP_PRE,
    TOOL_POST,
    TOOL_PRE,
};
use uva_hook::{HookPayload, HookRegistry};
use uva_kernel::{Context, EventValue, Next, Outcome, Waterfall, WaterfallCtx};
use uva_steering::{
    Action,
    GuardOutcome,
    HintMessage,
    HookDecision,
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

#[derive(Debug, Clone)]
/// A foreign middleware's ruling on one seam pass.
///
/// The vocabulary is the [`HookDecision`] subset a middleware may
/// impose from outside the host: [`SeamRuling::Ask`] records a reason
/// without ending the pass, [`SeamRuling::Block`] vetoes it, and
/// [`SeamRuling::Terminate`] ends the run. Continuing
/// without a ruling is expressed by invoking the continuation.
///
/// A middleware short-circuits with `EventValue::new(ruling)`;
/// anything else a chain short-circuits with reads as a block — the
/// same fail-closed posture a broken gate has.
pub enum SeamRuling {
    Ask { reason: String },
    Block { reason: String },
    Terminate { reason: String },
}

impl From<HookDecision> for SeamRuling {
    fn from(decision: HookDecision) -> Self {
        match decision {
            HookDecision::Ask { reason } => Self::Ask { reason },
            HookDecision::Block { reason } => Self::Block { reason },
            HookDecision::Terminate { reason } => Self::Terminate { reason },
            // `Allow` continues the chain and `ReplaceParams`
            // rewrites the pass payload; neither short-circuits, so
            // the adapter never builds a ruling from them.
            decision @ (HookDecision::Allow
            | HookDecision::ReplaceParams(_)) => {
                unreachable!("non-ruling decision {decision:?}")
            }
        }
    }
}

impl SeamRuling {
    fn decision(&self) -> HookDecision {
        match self {
            Self::Ask { reason } => HookDecision::Ask {
                reason: reason.clone(),
            },
            Self::Block { reason } => HookDecision::Block {
                reason: reason.clone(),
            },
            Self::Terminate { reason } => HookDecision::Terminate {
                reason: reason.clone(),
            },
        }
    }
}

// The short-circuit value the host's own adapters produce: the
// ruling plus the veto row the decision owes. The adapters run
// inside the kernel chain, where the run's sink is not reachable, so
// the veto travels with the ruling and the dispatch pass records it
// where the sink lives. A foreign middleware short-circuits with a
// bare [`SeamRuling`] instead — its veto row is the known gap the
// hook registry closes (design §6.2.1).
#[derive(Clone)]
pub(crate) struct Decided {
    pub(crate) ruling: SeamRuling,
    pub(crate) veto: Option<VetoRow>,
}

// The `hook_block` row one veto owes: who refused, why, and the call
// it refused. See `veto_row` in the host adapters for the filter.
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

/// The kernel chains a [`Host`] fronts for, one per seam key.
pub(crate) struct SeamChains {
    ctx: Context,
    step_pre: Waterfall,
    step_post: Waterfall,
    tool_pre: Waterfall,
    tool_post: Waterfall,
    llm_request: Waterfall,
    llm_result: Waterfall,
    prompt_sections: Waterfall,
}

impl Default for SeamChains {
    fn default() -> Self {
        Self::new()
    }
}

impl SeamChains {
    pub(crate) fn new() -> Self {
        let ctx = Context::new();
        Self {
            step_pre: Waterfall::new(&ctx),
            step_post: Waterfall::new(&ctx),
            tool_pre: Waterfall::new(&ctx),
            tool_post: Waterfall::new(&ctx),
            llm_request: Waterfall::new(&ctx),
            llm_result: Waterfall::new(&ctx),
            prompt_sections: Waterfall::new(&ctx),
            ctx,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Waterfall> {
        match key {
            STEP_PRE => Some(&self.step_pre),
            STEP_POST => Some(&self.step_post),
            TOOL_PRE => Some(&self.tool_pre),
            TOOL_POST => Some(&self.tool_post),
            LLM_REQUEST => Some(&self.llm_request),
            LLM_RESULT => Some(&self.llm_result),
            PROMPT_SECTIONS => Some(&self.prompt_sections),
            _ => None,
        }
    }

    pub(crate) fn context(&self) -> Context {
        self.ctx.clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The seam keys a pass dispatches on, in pass order. A hook or gate
/// registration lands on every one of them.
pub(crate) const fn pass_chain_keys() -> [&'static str; 6] {
    [
        STEP_PRE,
        STEP_POST,
        TOOL_PRE,
        TOOL_POST,
        LLM_REQUEST,
        LLM_RESULT,
    ]
}

#[derive(Clone)]
/// The loop's seam layer: one named method per interception point.
///
/// Clone is cheap and shares one policy table, so every run of an agent
/// sees the same registrations. The loop reads this type and no other
/// interception surface.
pub struct Seams {
    host: Host,
    registry: HookRegistry,
}

impl Seams {
    #[must_use]
    /// Wrap an installed host policy table.
    ///
    /// The external registry starts empty; a deployment that publishes
    /// a `hooks` service attaches it with
    /// [`Seams::with_hook_registry`].
    pub fn new(host: Host) -> Self {
        Self {
            host,
            registry: HookRegistry::new(),
        }
    }

    #[must_use]
    /// Attach the deployment's external hook registry.
    ///
    /// The registry's registrations run *before* the host's typed
    /// ones on every pass, through the same [`Arbitration`]: the
    /// first `Block` short-circuits, `Ask`s collect from both
    /// surfaces, and a deciding registration stops the later one. The
    /// table is shared, so a [`HookRegistration`] dropped anywhere
    /// withdraws its row from every agent built over this registry.
    ///
    /// [`HookRegistration`]: uva_hook::HookRegistration
    pub fn with_hook_registry(mut self, registry: HookRegistry) -> Self {
        self.registry = registry;
        self
    }

    #[must_use]
    /// The external registry this layer dispatches.
    pub fn hook_registry(&self) -> &HookRegistry {
        &self.registry
    }

    #[must_use]
    /// The production policy: defensive guards, budget / iteration /
    /// truncation hints, adaptive concurrency and the debug logging
    /// hook, exactly as [`Host::default_policy`] installs them.
    pub fn default_policy(workspace: impl Into<PathBuf>) -> Self {
        Self::new(Host::default_policy(workspace))
    }

    #[must_use]
    /// The host this layer wraps: the typed registration surface for
    /// the kinds that do not dispatch on a chain.
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
        self.dispatch_on(Some(STEP_PRE), ctx, sink, || HookEvent::BeforeStep {
            agent_name: agent_name.to_string(),
            run_idx: 0,
            iteration,
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
        self.dispatch_on(Some(STEP_POST), ctx, sink, || HookEvent::AfterStep {
            agent_name: agent_name.to_string(),
            run_idx: 0,
            iteration,
        })
        .await
        .arbitration
    }

    /// `tool/pre-execute`: the hooks, then the gates, before a tool runs.
    ///
    /// A gate is the security capability, so it gets the last word — but
    /// only when nothing before it vetoed. The guard pipeline is the
    /// sibling [`tool_guards`](Self::tool_guards) call; the loop runs it
    /// after this seam accepted the call so an approval request names the
    /// effective arguments.
    pub async fn tool_pre(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        tool_name: &str,
        call_id: &str,
        arguments: &serde_json::Value,
    ) -> Arbitration {
        self.dispatch_on(Some(TOOL_PRE), ctx, sink, || HookEvent::BeforeTool {
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
            arguments: arguments.clone(),
        })
        .await
        .arbitration
    }

    /// `tool/post-execute`: the hooks after a tool returned.
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
        self.dispatch_on(Some(TOOL_POST), ctx, sink, || HookEvent::AfterTool {
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
            is_error,
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
        if !self.seam_active(LLM_REQUEST) {
            return Arbitration::default();
        }
        let pass = self
            .dispatch_on(Some(LLM_REQUEST), ctx, sink, || {
                HookEvent::BeforeRequest {
                    provider: provider.to_string(),
                    params: params.clone(),
                }
            })
            .await;
        // The chain's effective event carries every rewrite — a
        // middleware's, a hook's `ReplaceParams`, a transformer's —
        // so the pass result is what goes on the wire.
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
        self.dispatch_on(Some(LLM_RESULT), ctx, sink, || {
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
        self.dispatch(ctx, sink, || HookEvent::BeforeRun {
            agent_name: agent_name.to_string(),
            run_idx,
        })
        .await
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
        self.dispatch(ctx, sink, || HookEvent::AfterRun {
            agent_name: agent_name.to_string(),
            run_idx,
            reason: reason.to_string(),
        })
        .await
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
        self.dispatch(ctx, sink, || HookEvent::BeforeProviderCall {
            provider: provider.to_string(),
            model: model.to_string(),
            estimated_tokens,
        })
        .await
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
        self.dispatch(ctx, sink, || HookEvent::OnError {
            agent_name: agent_name.to_string(),
            reason: reason.to_string(),
        })
        .await
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
        self.dispatch(ctx, sink, || HookEvent::OnCompaction {
            start,
            end,
            summary: summary.to_string(),
        })
        .await
    }

    #[must_use]
    /// The prompt sections the run's manifest carries, in registration
    /// order.
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        self.host.prompt_sections()
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
        self.host.has_result_rewriters()
            || self.registry.has_result_transformers()
    }

    /// `tool/post-execute`'s result transformers: the external
    /// registry's first, then the host's, in registration order. Each
    /// sees what the previous one left; the names that ran are
    /// returned when the aggregate output changed.
    pub async fn tool_result_transformers(
        &self,
        ctx: &HookContext<'_>,
        output: &mut ToolOutput,
    ) -> Vec<String> {
        let mut ran = self.registry.transform_result(output, ctx).await;
        ran.extend(self.host.apply_result_transformers(output, ctx).await);
        ran
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

    fn chain_active(&self, key: &'static str) -> bool {
        self.host
            .waterfall(key)
            .is_some_and(|chain| !chain.is_empty())
    }

    fn seam_active(&self, key: &'static str) -> bool {
        self.chain_active(key) || self.registry.has_entries(key)
    }

    // One dispatch pass. The external registry runs first; the host's
    // typed registrations — the seam's kernel chain, or the residual
    // hooks table at the observation points — then run over the
    // effective event the registry produced. Both fold through the
    // same `Arbitration`: a registry denial short-circuits the host
    // pass, and a continuing pass carries its asks and rewrite on.
    // The event is built lazily, so a run with no registration at all
    // pays nothing for the clone.
    async fn dispatch(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        event: impl FnOnce() -> HookEvent,
    ) -> Arbitration {
        self.dispatch_on(None, ctx, sink, event).await.arbitration
    }

    async fn dispatch_on(
        &self,
        key: Option<&'static str>,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        build: impl FnOnce() -> HookEvent,
    ) -> PassOutcome {
        let chain = key
            .and_then(|key| self.host.waterfall(key))
            .filter(|chain| !chain.is_empty());
        let registry_active =
            key.is_some_and(|key| self.registry.has_entries(key));
        if !registry_active && chain.is_none() {
            return self.host_pass(ctx, sink, build).await;
        }
        let mut event = build();
        let mut registry_arbitration = Arbitration::default();
        if let Some(key) = key.filter(|_| registry_active) {
            match self.registry_pass(key, &event, ctx, sink).await {
                RegistryStep::Continue {
                    event: rewritten,
                    arbitration,
                } => {
                    event = rewritten;
                    registry_arbitration = arbitration;
                }
                RegistryStep::Decided(arbitration) => {
                    return PassOutcome {
                        arbitration,
                        effective: None,
                    };
                }
            }
        }
        let outcome = match chain {
            Some(chain) => {
                self.chained_pass(
                    key.unwrap_or_default(),
                    chain,
                    ctx,
                    sink,
                    event,
                )
                .await
            }
            None => {
                let arbitration = if self.host.has_event_registrations() {
                    self.host.dispatch_hooks(&event, ctx, sink).await
                } else {
                    Arbitration::default()
                };
                PassOutcome {
                    arbitration,
                    effective: Some(event),
                }
            }
        };
        PassOutcome {
            arbitration: merge(registry_arbitration, outcome.arbitration),
            effective: outcome.effective,
        }
    }

    // The registry's half of one dispatch. A continuing pass hands the
    // (possibly rewritten) event and its arbitration back; a decided
    // pass writes the veto row it owes and stops here. The row is the
    // harness's to write (ADR 0004): the contract carries no sink, and
    // a reloaded session must still know which registration blocked
    // which call.
    async fn registry_pass(
        &self,
        key: &'static str,
        event: &HookEvent,
        ctx: &HookContext<'_>,
        sink: &EventSink,
    ) -> RegistryStep {
        let pass = self.registry.dispatch(key, event, ctx).await;
        // A gate that could not answer at a point with no denial to
        // carry (`llm/request`, `llm/result`) cannot refuse the pass.
        // The registry reports it instead of folding it into `Allow` —
        // it becomes a `hook` warning on the run's event stream, which
        // the session controller persists like every other warning, so
        // an operator reading the session sees the gate that never
        // answered.
        for failure in &pass.failures {
            sink.warn(WarningKind::Hook, None, failure.clone());
        }
        let Some(rewritten) = pass.event else {
            let name = pass.denied_by.as_deref().unwrap_or("hook registry");
            if let Some(veto) =
                veto_row(name, &pass.arbitration.decision, event)
            {
                veto.record(sink);
            }
            return RegistryStep::Decided(pass.arbitration);
        };
        RegistryStep::Continue {
            event: rewritten,
            arbitration: pass.arbitration,
        }
    }

    // The host-only pass: the observation points, whose hooks stay
    // in the host's residual table. Same lazy build and fast exit.
    async fn host_pass(
        &self,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        build: impl FnOnce() -> HookEvent,
    ) -> PassOutcome {
        if !self.host.has_event_registrations() {
            return PassOutcome {
                arbitration: Arbitration::default(),
                effective: None,
            };
        }
        let event = build();
        let arbitration = self.host.dispatch_hooks(&event, ctx, sink).await;
        PassOutcome {
            arbitration,
            effective: Some(event),
        }
    }

    // The chain pass: foreign middleware first, then — when nothing
    // foreign decided — the host's own hooks and gates over the
    // effective event the chain produced. The payload is the
    // contract crate's `HookPayload`; a deciding adapter ruling
    // travels back as a `Decided` value, and the veto row it owes is
    // recorded here, where the run's sink lives.
    async fn chained_pass(
        &self,
        key: &'static str,
        chain: &Waterfall,
        ctx: &HookContext<'_>,
        sink: &EventSink,
        event: HookEvent,
    ) -> PassOutcome {
        let pass = Arc::new(HookPayload::new(event, ctx));
        let seen: Arc<Mutex<Option<HookEvent>>> = Arc::new(Mutex::new(None));
        let request = Arc::new(WaterfallCtx::new(
            key,
            EventValue::from_arc(Arc::clone(&pass)),
            self.host.seam_context(),
        ));
        let ran =
            AssertUnwindSafe(chain.run(request, recorder(Arc::clone(&seen))))
                .catch_unwind()
                .await;
        match ran {
            Ok(Ok(Outcome::Continue)) => {
                let event = lock(&seen).take();
                PassOutcome {
                    arbitration: fold_asks(pass.take_asks()),
                    effective: event,
                }
            }
            Ok(Ok(Outcome::ShortCircuit(value))) => PassOutcome {
                arbitration: short_circuit_arbitration(value, &pass, sink),
                effective: None,
            },
            Ok(Err(error)) => PassOutcome {
                arbitration: block(format!("seam middleware failed: {error}")),
                effective: None,
            },
            Err(_) => PassOutcome {
                arbitration: block("seam middleware panicked".to_owned()),
                effective: None,
            },
        }
    }
}

// Fold the ask reasons a chain collected. Only asks are recorded on
// the payload — a deciding ruling short-circuits the chain instead —
// so the fold is a plain collection, the same absorb rules the
// host's own hook loop applies.
fn fold_asks(asks: Vec<String>) -> Arbitration {
    Arbitration {
        asks,
        ..Arbitration::default()
    }
}

// Fold the host's pass into the registry's: the registry ran first, so
// its asks stay first, and its decision was necessarily undecided (a
// decided registry pass returns before the host runs). The host's
// decision — including a veto that its chain adapters recorded — is
// what the merged pass reports.
fn merge(mut earlier: Arbitration, later: Arbitration) -> Arbitration {
    earlier.asks.extend(later.asks);
    earlier.absorb(later.decision);
    earlier
}

fn short_circuit_arbitration(
    value: EventValue,
    pass: &HookPayload,
    sink: &EventSink,
) -> Arbitration {
    // The host's own adapters short-circuit with a `Decided` value
    // (the ruling plus the veto row it owes); foreign middleware
    // short-circuits with a bare `SeamRuling`. Anything else reads
    // as a block — the same fail-closed posture as before.
    let (decision, veto) = if let Ok(decided) = value.downcast::<Decided>() {
        (decided.ruling.decision(), decided.veto.clone())
    } else if let Ok(ruling) = value.downcast::<SeamRuling>() {
        (ruling.decision(), None)
    } else {
        (unrecognized_short_circuit(), None)
    };
    // The ask reasons recorded before the deciding ruling belong to
    // the same report — today's hook loop keeps them too.
    let mut arbitration = fold_asks(pass.take_asks());
    arbitration.decision = decision;
    if let Some(veto) = veto {
        veto.record(sink);
    }
    arbitration
}

fn block(reason: String) -> Arbitration {
    Arbitration {
        asks: Vec::new(),
        decision: HookDecision::Block { reason },
    }
}

// What one dispatch pass produced: the arbitration, and — when a
// foreign chain rewrote it — the effective event the host dispatched.
struct PassOutcome {
    arbitration: Arbitration,
    effective: Option<HookEvent>,
}

// The external registry's half of one dispatch.
enum RegistryStep {
    Continue {
        event: HookEvent,
        arbitration: Arbitration,
    },
    Decided(Arbitration),
}

fn unrecognized_short_circuit() -> HookDecision {
    HookDecision::Block {
        reason: "seam middleware short-circuited with an unrecognized                  payload"
            .to_owned(),
    }
}

// The innermost callback of a seam pass: record the effective event
// and let the chain report `Continue`.
fn recorder(seen: Arc<Mutex<Option<HookEvent>>>) -> Next {
    Arc::new(move |ctx| {
        let seen = Arc::clone(&seen);
        Box::pin(async move {
            if let Some(pass) = HookPayload::payload(&ctx) {
                *lock(&seen) = Some(pass.event().clone());
            }
            Ok(Outcome::Continue)
        })
    })
}

#[cfg(test)]
mod tests;
