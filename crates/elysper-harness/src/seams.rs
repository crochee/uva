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
//! `tools`, `delegators`). It never names [`Host`] or builds a
//! `HookEvent`.
//!
//! Every seam method is a pass over the point's kernel
//! [`Waterfall`](elysper_kernel::Waterfall) chain: foreign middleware
//! registered through [`Host::waterfall`](crate::agent::host::Host::waterfall)
//! runs first and may observe, rewrite (via
//! [`SeamPass::rewrite`](crate::seams::SeamPass::rewrite)), or
//! short-circuit the pass with a
//! [`SeamRuling`](crate::seams::SeamRuling); when nothing foreign
//! decided, the [`Host`]'s typed hooks, gates and transformers run
//! over the effective event the chain produced, with the arbitration,
//! fail-closed gate rules and panic isolation they have always had.
//! An empty chain plus no host registration exits before the event is
//! even built, so a bare run pays nothing for the spine.
//!
//! # Registration and lifecycle
//!
//! Registrations still go through [`Host`] (`hook`, `gate`, `guard`,
//! transformer, `prompt_section`, `tool`, `delegator`), which the loop no
//! longer reads. A deployment either wraps an installed host with
//! [`Seams::new`](crate::seams::Seams::new) and installs it with
//! [`ReActAgent::with_seams`](crate::ReActAgent::with_seams), or keeps
//! using `ReActAgent::with_host`, which wraps the host for it.
//!
//! A registration's lifetime is its [`Registration`] handle: dropping
//! the last handle withdraws the row from every seam wrapped around
//! the host, which is the same lifetime the kernel's `process-plugins`
//! entry (in `elysper-plugin-host`) hands its effect.

use std::{
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use elysper_context::AgentState;
use elysper_kernel::{
    Context,
    EventValue,
    Next,
    Outcome,
    Waterfall,
    WaterfallCtx,
};
use elysper_steering::{
    Action,
    GuardOutcome,
    HintMessage,
    HookDecision,
    HookEvent,
    InjectionPoint,
    RequestParams,
    tracker::Tracker,
};
use elysper_tool::{Tool, ToolOutput, ToolRegistry};
use futures::FutureExt;

use crate::agent::{
    DelegatingTool,
    host::{Arbitration, Diagnostic, HookContext, Host, PromptSection},
};

/// Before a loop step samples the model.
///
/// A middleware may observe the step, terminate the run, or
/// short-circuit it — a short-circuit skips the model call and its
/// value is the step's result.
pub const STEP_PRE: &str = "agent/step-pre";

/// After a loop step committed its tool results.
///
/// The pass observes a step that already ran; a middleware returns
/// `Continue` to leave the result unchanged.
pub const STEP_POST: &str = "agent/step-post";

/// Before a tool call executes.
///
/// A middleware may observe the call, rewrite its arguments, or
/// short-circuit it with a rejection. A rejection skips the tool and the
/// loop commits a matching `tool_result`.
pub const TOOL_PRE: &str = "tool/pre-execute";

/// After a tool call returned, before the loop commits its result.
///
/// A middleware may observe the result or rewrite it; the rewritten
/// value is what the model reads in the next request.
pub const TOOL_POST: &str = "tool/post-execute";

/// Before a model request goes on the wire.
///
/// Carries the request parameters (`RequestParams`): the model, the
/// temperature, the max tokens, the tool choice and the tool schemas.
/// The transcript and the system prompt are not part of the payload.
pub const LLM_REQUEST: &str = "llm/request";

/// After a model request returned.
///
/// Carries the provider's answer. A middleware may observe it or
/// rewrite it before the loop commits the step.
pub const LLM_RESULT: &str = "llm/result";

/// A collection event: one system-prompt section per middleware.
///
/// Sections render in registration order after the base manifest; a run
/// takes its section snapshot once, so a registration made mid-run lands
/// on the next run.
pub const PROMPT_SECTIONS: &str = "prompt/sections";

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

    fn ask_reason(&self) -> Option<&str> {
        match self {
            Self::Ask { reason } => Some(reason),
            _ => None,
        }
    }
}

/// The payload every seam chain carries for one pass.
///
/// A foreign middleware downcasts the chain context's payload to
/// this type to read the event, record ask reasons, or rebuild the
/// context with a rewritten event (see [`SeamPass::rewrite`]). The
/// host's own registrations run after the chain, over the effective
/// event the chain produced.
pub struct SeamPass {
    event: HookEvent,
    rulings: Mutex<Vec<SeamRuling>>,
}

impl SeamPass {
    pub(crate) fn new(event: HookEvent) -> Self {
        Self {
            event,
            rulings: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    /// The typed event this pass carries.
    pub fn event(&self) -> &HookEvent {
        &self.event
    }

    /// Record an ask reason; the pass continues.
    pub fn ask(&self, reason: impl Into<String>) {
        lock(&self.rulings).push(SeamRuling::Ask {
            reason: reason.into(),
        });
    }

    #[must_use]
    /// The pass payload a chain context carries, if it carries one.
    pub fn payload(ctx: &WaterfallCtx) -> Option<Arc<Self>> {
        ctx.payload().downcast::<Self>().ok()
    }

    #[must_use]
    /// Continue the chain with a rewritten event.
    ///
    /// The rewritten context keeps the chain's event name and
    /// services; only the pass payload is replaced.
    pub fn rewrite(
        ctx: &Arc<WaterfallCtx>,
        event: HookEvent,
    ) -> Arc<WaterfallCtx> {
        Arc::new(WaterfallCtx::new(
            ctx.event().to_owned(),
            EventValue::from_arc(Arc::new(Self::new(event))),
            ctx.services().clone(),
        ))
    }

    fn take_rulings(&self) -> Vec<SeamRuling> {
        std::mem::take(&mut *lock(&self.rulings))
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
    pub fn new(host: Host) -> Self {
        Self { host }
    }

    #[must_use]
    /// The production policy: defensive guards, budget / iteration /
    /// truncation hints, adaptive concurrency and the debug logging
    /// hook, exactly as [`Host::default_policy`] installs them.
    pub fn default_policy(workspace: impl Into<PathBuf>) -> Self {
        Self::new(Host::default_policy(workspace))
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
        agent_name: &str,
        iteration: u32,
    ) -> Arbitration {
        self.dispatch_on(Some(STEP_PRE), ctx, || HookEvent::BeforeStep {
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
        agent_name: &str,
        iteration: u32,
    ) -> Arbitration {
        self.dispatch_on(Some(STEP_POST), ctx, || HookEvent::AfterStep {
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
        tool_name: &str,
        call_id: &str,
        arguments: &serde_json::Value,
    ) -> Arbitration {
        self.dispatch_on(Some(TOOL_PRE), ctx, || HookEvent::BeforeTool {
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
        tool_name: &str,
        call_id: &str,
        is_error: bool,
    ) -> Arbitration {
        self.dispatch_on(Some(TOOL_POST), ctx, || HookEvent::AfterTool {
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
        provider: &str,
        params: &mut RequestParams,
    ) -> Arbitration {
        if !self.chain_active(LLM_REQUEST) && !self.host.has_request_rewriters()
        {
            return Arbitration::default();
        }
        let pass = self
            .dispatch_on(Some(LLM_REQUEST), ctx, || HookEvent::BeforeRequest {
                provider: provider.to_string(),
                params: params.clone(),
            })
            .await;
        if let Some(HookEvent::BeforeRequest {
            params: rewritten, ..
        }) = pass.effective
        {
            *params = rewritten;
        }
        if let HookDecision::ReplaceParams(replacement) =
            &pass.arbitration.decision
        {
            *params = replacement.clone();
        }
        self.host.apply_request_transformers(params, ctx).await;
        pass.arbitration
    }

    /// `llm/result`: observe the provider's answer after it was
    /// assembled.
    ///
    /// Observational in the current steering vocabulary: a decision the
    /// point does not accept is recorded and ignored.
    pub async fn llm_result(
        &self,
        ctx: &HookContext<'_>,
        provider: &str,
        model: &str,
        prompt_tokens: usize,
        completion_tokens: usize,
        stop_reason: Option<String>,
    ) -> Arbitration {
        self.dispatch_on(Some(LLM_RESULT), ctx, || {
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
        agent_name: &str,
        run_idx: u32,
    ) -> Arbitration {
        self.dispatch(ctx, || HookEvent::BeforeRun {
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
        agent_name: &str,
        run_idx: u32,
        reason: &str,
    ) -> Arbitration {
        self.dispatch(ctx, || HookEvent::AfterRun {
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
        provider: &str,
        model: &str,
        estimated_tokens: usize,
    ) -> Arbitration {
        self.dispatch(ctx, || HookEvent::BeforeProviderCall {
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
        agent_name: &str,
        reason: &str,
    ) -> Arbitration {
        self.dispatch(ctx, || HookEvent::OnError {
            agent_name: agent_name.to_string(),
            reason: reason.to_string(),
        })
        .await
    }

    /// The `on_compaction` observation point.
    pub async fn compacted(
        &self,
        ctx: &HookContext<'_>,
        start: usize,
        end: usize,
        summary: &str,
    ) -> Arbitration {
        self.dispatch(ctx, || HookEvent::OnCompaction {
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
    /// The registered delegating tools, in registration order.
    pub fn delegators(&self) -> Vec<Arc<dyn DelegatingTool>> {
        self.host.delegators()
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
    }

    /// `tool/post-execute`'s result transformers: run in registration
    /// order and return the names that changed the output.
    pub async fn tool_result_transformers(
        &self,
        ctx: &HookContext<'_>,
        output: &mut ToolOutput,
    ) -> Vec<String> {
        self.host.apply_result_transformers(output, ctx).await
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

    // One dispatch pass. The seam's kernel chain runs first when it
    // carries foreign middleware; the host's hooks and gates then run
    // over the effective event the chain produced. The event is built
    // lazily, so a run with no chain middleware and no registration
    // pays nothing for the clone.
    async fn dispatch(
        &self,
        ctx: &HookContext<'_>,
        event: impl FnOnce() -> HookEvent,
    ) -> Arbitration {
        self.dispatch_on(None, ctx, event).await.arbitration
    }

    async fn dispatch_on(
        &self,
        key: Option<&'static str>,
        ctx: &HookContext<'_>,
        build: impl FnOnce() -> HookEvent,
    ) -> PassOutcome {
        let chain = key.and_then(|k| self.host.waterfall(k));
        let active = chain.filter(|chain| !chain.is_empty());
        match (key, active) {
            (Some(key), Some(chain)) => {
                self.chained_pass(key, chain, ctx, build).await
            }
            _ => self.host_pass(ctx, build).await,
        }
    }

    // The host-only pass: today's dispatch, unchanged, with the same
    // lazy build and fast exit.
    async fn host_pass(
        &self,
        ctx: &HookContext<'_>,
        build: impl FnOnce() -> HookEvent,
    ) -> PassOutcome {
        if !self.host.has_event_registrations() {
            return PassOutcome {
                arbitration: Arbitration::default(),
                effective: None,
            };
        }
        let event = build();
        let mut arbitration = self.host.dispatch_hooks(&event, ctx).await;
        if !arbitration.decided() {
            arbitration.merge(self.host.dispatch_gates(&event, ctx).await);
        }
        PassOutcome {
            arbitration,
            effective: Some(event),
        }
    }

    // The chain pass: foreign middleware first, then — when nothing
    // foreign decided — the host's own hooks and gates over the
    // effective event the chain produced.
    async fn chained_pass(
        &self,
        key: &'static str,
        chain: &Waterfall,
        ctx: &HookContext<'_>,
        build: impl FnOnce() -> HookEvent,
    ) -> PassOutcome {
        let pass = Arc::new(SeamPass::new(build()));
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
                let foreign = fold_rulings(pass.take_rulings());
                if foreign.decided() {
                    return PassOutcome {
                        arbitration: foreign,
                        effective: None,
                    };
                }
                let event =
                    lock(&seen).take().unwrap_or_else(|| pass.event().clone());
                let mut arbitration =
                    self.host.dispatch_hooks(&event, ctx).await;
                if !arbitration.decided() {
                    arbitration
                        .merge(self.host.dispatch_gates(&event, ctx).await);
                }
                arbitration.asks.splice(0..0, foreign.asks);
                PassOutcome {
                    arbitration,
                    effective: Some(event),
                }
            }
            Ok(Ok(Outcome::ShortCircuit(value))) => PassOutcome {
                arbitration: short_circuit_arbitration(value),
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

// Fold the ask reasons a foreign chain collected, stopping at the
// first deciding ruling — the same absorb rules the host's own hook
// loop applies.
fn fold_rulings(rulings: Vec<SeamRuling>) -> Arbitration {
    let mut arbitration = Arbitration::default();
    for ruling in rulings {
        if let Some(reason) = ruling.ask_reason() {
            arbitration.asks.push(reason.to_owned());
        } else {
            arbitration.decision = ruling.decision();
            break;
        }
    }
    arbitration
}

fn short_circuit_arbitration(value: EventValue) -> Arbitration {
    let decision = match value.downcast::<SeamRuling>() {
        Ok(ruling) => ruling.decision(),
        Err(_) => unrecognized_short_circuit(),
    };
    Arbitration {
        asks: Vec::new(),
        decision,
    }
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
            if let Some(pass) = SeamPass::payload(&ctx) {
                *lock(&seen) = Some(pass.event().clone());
            }
            Ok(Outcome::Continue)
        })
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use elysper_kernel::{EventValue, Outcome, WaterfallCtx};
    use elysper_steering::{HookDecision, HookEvent, RequestParams};
    use futures::channel::mpsc;

    use super::{LLM_REQUEST, STEP_PRE, SeamPass, SeamRuling, Seams, TOOL_PRE};
    use crate::agent::{
        EventSink,
        host::{Hook, HookContext, Host},
    };

    struct Fixture {
        state: elysper_context::AgentState,
        sink: EventSink,
        cancel: Arc<dyn elysper_core::CancelToken>,
    }

    impl Fixture {
        fn new() -> Self {
            let (tx, _rx) = mpsc::unbounded();
            Self {
                state: elysper_context::AgentState::with_window(1_000),
                sink: EventSink::new(Arc::new(tx), None),
                cancel: elysper_core::AtomicCancelToken::shared(),
            }
        }

        fn ctx(&self) -> HookContext<'_> {
            HookContext {
                state: &self.state,
                events: &self.sink,
                cancel: &self.cancel,
            }
        }
    }

    fn short_circuits_with(ruling: SeamRuling) -> elysper_kernel::Middleware {
        Arc::new(move |_ctx, _next| {
            let ruling = ruling.clone();
            Box::pin(async move {
                Ok(Outcome::ShortCircuit(EventValue::new(ruling)))
            })
        })
    }

    struct CountingHook {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Hook for CountingHook {
        fn name(&self) -> &str {
            "counting"
        }

        async fn observe(
            &self,
            _event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> HookDecision {
            self.calls.fetch_add(1, Ordering::SeqCst);
            HookDecision::Allow
        }
    }

    #[tokio::test]
    async fn a_foreign_block_on_step_pre_decides_the_pass() {
        let host = Host::new();
        let chain = host.waterfall(STEP_PRE).expect("step-pre chain");
        let _guard = chain
            .register(short_circuits_with(SeamRuling::Block {
                reason: "no model today".to_owned(),
            }))
            .expect("register blocks");
        let seams = Seams::new(host);

        let fixture = Fixture::new();
        let arbitration = seams.step_pre(&fixture.ctx(), "a", 0).await;
        assert!(
            matches!(arbitration.decision, HookDecision::Block { .. }),
            "a foreign block decides the pass: {arbitration:?}"
        );
    }

    #[tokio::test]
    async fn a_foreign_ask_collects_and_the_host_hooks_still_run() {
        let mut host = Host::new();
        let chain = host.waterfall(TOOL_PRE).expect("tool-pre chain");
        let _guard = chain
            .register(Arc::new(|ctx, next| {
                Box::pin(async move {
                    if let Some(pass) = SeamPass::payload(&ctx) {
                        pass.ask("high risk");
                    }
                    next(ctx).await
                })
            }))
            .expect("register ask");
        let calls = Arc::new(AtomicUsize::new(0));
        let _registration = host.hook(Arc::new(CountingHook {
            calls: Arc::clone(&calls),
        }));
        let seams = Seams::new(host);

        let fixture = Fixture::new();
        let arbitration = seams
            .tool_pre(&fixture.ctx(), "shell", "c1", &serde_json::json!({}))
            .await;
        assert_eq!(arbitration.asks, vec!["high risk".to_owned()]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the host hooks run after the foreign chain: {arbitration:?}"
        );
    }

    #[tokio::test]
    async fn a_foreign_rewrite_on_llm_request_rewrites_the_params() {
        let host = Host::new();
        let chain = host.waterfall(LLM_REQUEST).expect("llm-request chain");
        let _guard = chain
            .register(Arc::new(|ctx, next| {
                Box::pin(async move {
                    let rewritten = SeamPass::payload(&ctx).map(|pass| {
                        let event = pass.event().clone();
                        match event {
                            HookEvent::BeforeRequest {
                                provider,
                                mut params,
                            } => {
                                params.model = "rewritten-model".to_owned();
                                HookEvent::BeforeRequest { provider, params }
                            }
                            other => other,
                        }
                    });
                    match rewritten {
                        Some(event) => {
                            next(SeamPass::rewrite(&ctx, event)).await
                        }
                        None => next(ctx).await,
                    }
                })
            }))
            .expect("register rewriter");
        // `_guard` above keeps the middleware registered for the pass.
        let seams = Seams::new(host);

        let fixture = Fixture::new();
        let mut params =
            RequestParams::from_request(&elysper_provider::CompletionRequest {
                model: "original-model".to_owned(),
                ..elysper_provider::CompletionRequest::default()
            });
        let arbitration =
            seams.llm_request(&fixture.ctx(), "p", &mut params).await;
        assert_eq!(params.model, "rewritten-model");
        assert!(
            !arbitration.decided(),
            "a rewrite is not a decision: {arbitration:?}"
        );
    }

    #[tokio::test]
    async fn a_panicking_foreign_middleware_on_tool_pre_is_a_block() {
        let host = Host::new();
        let chain = host.waterfall(TOOL_PRE).expect("tool-pre chain");
        let _guard = chain
            .register(Arc::new(|_ctx, _next| {
                Box::pin(async move { panic!("broken middleware") })
            }))
            .expect("register panic");
        let seams = Seams::new(host);

        let fixture = Fixture::new();
        let arbitration = seams
            .tool_pre(&fixture.ctx(), "shell", "c1", &serde_json::json!({}))
            .await;
        assert!(
            matches!(arbitration.decision, HookDecision::Block { .. }),
            "a broken foreign middleware fails closed: {arbitration:?}"
        );
    }

    #[test]
    fn a_pass_rewrite_builds_a_new_chain_context() {
        let pass = Arc::new(SeamPass::new(HookEvent::BeforeStep {
            agent_name: "a".to_owned(),
            run_idx: 0,
            iteration: 0,
        }));
        let ctx = Arc::new(WaterfallCtx::new(
            STEP_PRE,
            EventValue::from_arc(pass),
            elysper_kernel::Context::new(),
        ));
        let rewritten = SeamPass::rewrite(
            &ctx,
            HookEvent::BeforeStep {
                agent_name: "b".to_owned(),
                run_idx: 0,
                iteration: 0,
            },
        );
        let payload = SeamPass::payload(&rewritten).expect("carries a pass");
        assert!(matches!(
            payload.event(),
            HookEvent::BeforeStep { agent_name, .. } if agent_name == "b"
        ));
        assert_eq!(rewritten.event(), STEP_PRE);
    }
}
