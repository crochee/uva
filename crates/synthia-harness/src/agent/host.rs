//! The plugin host: the per-agent registry a plugin installs into,
//! and the arbitration its registrations run through.
//!
//! A host belongs to one agent — `ReActAgent::with_host` installs it
//! — so two agents never share one policy. Every registration
//! returns a [`Registration`](crate::agent::host::Registration): dropping
//! it withdraws the registration, so a plugin can be uninstalled
//! without rebuilding the agent.
//!
//! The arbitration rules are the same everywhere:
//!
//! - a `Block` / `Terminate` decision ends the pass and wins over
//!   everything collected before it;
//! - an `Ask` never ends the pass — the reasons are collected and
//!   reported together, so one approval request can carry all of
//!   them;
//! - the transformers chain: each one sees what the previous one
//!   left;
//! - a gate that fails or panics is a block (fail closed), because
//!   a broken gate must not become an approval;
//! - an observer or transformer that panics is disabled with a
//!   diagnostic — fail open is only for decisions that are not
//!   security decisions.

use std::{
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use synthia_steering::{HookDecision, HookEvent, RequestParams, event_name};
use synthia_tool::{
    RegistrationToken,
    Tool,
    ToolEntry,
    ToolOutput,
    ToolRegistry,
};

use super::re_act::delegation::DelegatingTool;

#[cfg(test)]
mod tests;

// How many diagnostics the host keeps.
const DIAGNOSTIC_LIMIT: usize = 64;

/// What one dispatch pass decided at one point.
///
/// The `decision` is the one that won the pass: `Allow` when
/// nothing objected, `Block` / `Terminate` when a registration
/// ended the pass, `ReplaceParams` when a request was rewritten.
/// The `asks` are the approval reasons the registrations collected
/// on the way — they never end a pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arbitration {
    /// The decision that won the pass.
    pub decision: HookDecision,
    /// The reasons the registrations asked for an approval.
    pub asks: Vec<String>,
}

impl Arbitration {
    /// Fold one decision into the pass.
    ///
    /// An `Ask` joins the collected reasons; a `Block` / `Terminate`
    /// wins over a `ReplaceParams` that came before it, because the
    /// run does not reach the provider when it was vetoed.
    pub fn absorb(&mut self, decision: HookDecision) {
        match decision {
            HookDecision::Allow => {}
            HookDecision::Ask { reason } => self.asks.push(reason),
            HookDecision::ReplaceParams(_) => {
                if matches!(self.decision, HookDecision::Allow) {
                    self.decision = decision;
                }
            }
            HookDecision::Block { .. } | HookDecision::Terminate { .. } => {
                if matches!(
                    self.decision,
                    HookDecision::Allow | HookDecision::ReplaceParams(_)
                ) {
                    self.decision = decision;
                }
            }
        }
    }

    /// Fold another pass into this one.
    pub fn merge(&mut self, other: Arbitration) {
        self.asks.extend(other.asks);
        self.absorb(other.decision);
    }

    /// Whether a decision ended the pass: a block or a termination.
    ///
    /// The registrations behind such a decision are not consulted —
    /// the call is already vetoed.
    pub fn decided(&self) -> bool {
        matches!(
            self.decision,
            HookDecision::Block { .. } | HookDecision::Terminate { .. }
        )
    }
}

/// An observer: sees a hook event and may veto the run.
///
/// An observer is the read-mostly capability: it sees what happens
/// and can block or end the run, but it holds no state the loop
/// depends on.
#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;

    /// Observe one event.
    async fn observe(&self, event: &HookEvent) -> HookDecision;
}

/// A gate: decides whether a call may proceed.
///
/// A gate is the security capability: it may block a call or ask for
/// an approval. It can also fail — `Err` means the gate could not
/// answer, and the host treats that as a block.
#[async_trait]
pub trait Gate: Send + Sync {
    fn name(&self) -> &str;

    /// Check one event.
    async fn check(&self, event: &HookEvent) -> Result<HookDecision, String>;
}

/// Rewrites the request parameters before the request goes on the
/// wire.
///
/// The rewrite surface is [`RequestParams`]: the model, the
/// temperature, the max tokens, the tool choice and the tool
/// schemas. The transcript and the system prompt are not part of it.
#[async_trait]
pub trait RequestTransformer: Send + Sync {
    fn name(&self) -> &str;

    /// Rewrite the parameters in place.
    async fn transform(&self, params: &mut RequestParams)
    -> Result<(), String>;
}

/// Rewrites a tool result after the tool returned.
///
/// Transformers run *before* the loop commits the result, so the
/// `tool_result` row and the history hold the rewritten text — what the
/// log says is what the model read. The harness still records a
/// `SessionEvent::ToolResultTransformed` row when the aggregate output
/// changed: provenance ("a registered transformer altered this result
/// before it was committed"), not reconciliation.
#[async_trait]
pub trait ResultTransformer: Send + Sync {
    fn name(&self) -> &str;

    /// Rewrite the output in place.
    async fn transform(&self, output: &mut ToolOutput) -> Result<(), String>;
}

/// A system-prompt section a plugin contributes.
///
/// The type has no `remove` and no `replace`: a section is added to
/// the prompt, and that is all it can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSection {
    /// The section's title.
    pub title: String,
    /// The section's body.
    pub body: String,
}

/// A plugin: a bundle of registrations installed into one host.
pub trait Plugin: Send + Sync {
    /// The plugin's name.
    fn name(&self) -> &str;

    /// Install the plugin's registrations; the handle owns them.
    fn install(&self, host: &mut Host) -> PluginHandle;
}

/// One registration in a host.
///
/// Dropping the registration withdraws it: the host stops calling
/// it. A clone shares the withdrawal — the registration is gone when
/// the last handle is.
#[derive(Clone)]
pub struct Registration {
    inner: Arc<RegistrationInner>,
}

impl Registration {
    /// The registration's id in the host's table.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.inner.id
    }

    // Whether the host's install bookkeeping is the only holder: the
    // plugin dropped its own copy, so the registration is dead.
    fn is_unclaimed(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    // Withdraw the registration now, without waiting for the last
    // handle to drop.
    fn withdraw(&self) {
        if let Some(host) = self.inner.host.upgrade() {
            let mut state = host.state.lock();
            remove_registration(&mut state, self.inner.kind, self.inner.id);
        }
    }
}

struct RegistrationInner {
    id: u64,
    kind: RegistrationKind,
    host: Weak<HostInner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrationKind {
    Observer,
    Gate,
    RequestTransformer,
    ResultTransformer,
    PromptSection,
    Tool,
    Delegator,
}

impl Drop for RegistrationInner {
    fn drop(&mut self) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        let mut state = host.state.lock();
        remove_registration(&mut state, self.kind, self.id);
    }
}

// Withdraw one registration from the host's tables.
fn remove_registration(state: &mut HostState, kind: RegistrationKind, id: u64) {
    match kind {
        RegistrationKind::Observer => retain_id(&mut state.observers, id),
        RegistrationKind::Gate => retain_id(&mut state.gates, id),
        RegistrationKind::RequestTransformer => {
            retain_id(&mut state.request_transformers, id);
        }
        RegistrationKind::ResultTransformer => {
            retain_id(&mut state.result_transformers, id);
        }
        RegistrationKind::PromptSection => {
            retain_id(&mut state.prompt_sections, id);
        }
        RegistrationKind::Tool => withdraw_tool(state, id),
        RegistrationKind::Delegator => retain_id(&mut state.delegators, id),
    }
}

// Withdraw one tool registration: the host forgets it, and the
// registry forgets it too — a withdrawn tool must not stay callable.
//
// The withdrawal names the token the host's own entry was filed
// under: another party's entry under the same name is a different
// registration and stays where it is.
fn withdraw_tool(state: &mut HostState, id: u64) {
    let Some(index) = state.tools.iter().position(|row| row.id == id) else {
        return;
    };
    let row = state.tools.remove(index);
    let Some(registry) = state.registry.as_ref() else {
        return;
    };
    withdraw_token(registry, row.token);
}

// Take the entry filed under `token` out of the registry, if the
// registration ever reached it.
fn withdraw_token(registry: &ToolRegistry, token: Option<RegistrationToken>) {
    if let Some(token) = token {
        registry.unregister_by_token(token);
    }
}

/// The handle a plugin install returns: it owns every registration
/// the install made, so dropping it uninstalls the plugin.
#[derive(Clone, Default)]
pub struct PluginHandle {
    registrations: Vec<Registration>,
}

impl PluginHandle {
    /// A handle over the registrations one install made.
    #[must_use]
    pub fn new(registrations: Vec<Registration>) -> Self {
        Self { registrations }
    }

    /// The registrations the handle owns.
    #[must_use]
    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    /// Withdraw every registration now.
    pub fn uninstall(self) {
        drop(self.registrations);
    }
}

/// The per-agent registry of plugin registrations.
///
/// One host belongs to one agent: the agent's loop consults it at
/// every decision point, and the registrations run in the order
/// they were installed.
pub struct Host {
    inner: Arc<HostInner>,
    // One scope per install in progress: the registrations that
    // install made, so the returned handle can own them. Outside an
    // install the list is empty, and a registration lives only in
    // the caller's hands.
    scopes: Vec<Vec<Registration>>,
}

impl Clone for Host {
    fn clone(&self) -> Self {
        // The clone shares the registrations — a host is one policy,
        // not a copy of it — and starts with no install in progress.
        Self {
            inner: Arc::clone(&self.inner),
            scopes: Vec::new(),
        }
    }
}

struct HostInner {
    state: Mutex<HostState>,
    next_id: AtomicU64,
}

#[derive(Default)]
struct HostState {
    observers: Vec<Entry<Arc<dyn Hook>>>,
    gates: Vec<Entry<Arc<dyn Gate>>>,
    request_transformers: Vec<Entry<Arc<dyn RequestTransformer>>>,
    result_transformers: Vec<Entry<Arc<dyn ResultTransformer>>>,
    prompt_sections: Vec<Entry<PromptSection>>,
    tools: Vec<ToolRow>,
    delegators: Vec<Entry<Arc<dyn DelegatingTool>>>,
    // The registry the host's tools live in. The host is bound to it
    // when the agent installs the host; before that, the tools wait
    // for the binding.
    registry: Option<Arc<ToolRegistry>>,
    diagnostics: Vec<Diagnostic>,
}

// One tool row waiting for the registry binding: its id, name and
// the tool the row holds.
type PendingTool = (u64, String, Arc<dyn Tool>);

// One tool registration's row in the host's table.
struct ToolRow {
    id: u64,
    name: String,
    tool: Arc<dyn Tool>,
    // The token the registry filed this tool's entry under. It is
    // what the withdrawal names, so another party's entry under the
    // same name is untouched.
    token: Option<RegistrationToken>,
}

// One registration's row in one of the host's tables.
struct Entry<T> {
    id: u64,
    name: String,
    value: T,
    disabled: bool,
}

impl<T> Clone for Entry<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            name: self.name.clone(),
            value: self.value.clone(),
            disabled: self.disabled,
        }
    }
}

/// What went wrong with one registration.
///
/// The kinds are: a decision the point does not accept (ignored), a
/// registration that panicked (disabled), a gate that could not
/// answer (its call blocked), a transformer that reported a failure
/// (its change skipped), a tool whose name a core tool already owns
/// (the host's tool stays out of the registry), and a second,
/// different registry offered to an already-bound host (the
/// original binding stays).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The registration returned a decision the point does not
    /// accept; the decision was ignored.
    UnsupportedDecision,
    /// The registration panicked; it was disabled.
    Disabled,
    /// The gate could not answer; its call was blocked.
    GateFailed,
    /// The transformer reported a failure; its change was skipped.
    TransformerFailed,
    // A tool's name is already owned by a core tool; the host's
    // tool stays out of the registry.
    ToolNameConflict,
    /// The host is already bound to a different registry; the first wins.
    RegistryAlreadyBound,
}

/// One recorded diagnostic.
///
/// The host keeps the last few so a consumer can explain why a
/// decision was ignored or why a call was blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// What happened.
    pub kind: DiagnosticKind,
    /// The registration's name.
    pub source: String,
    /// The human-readable detail.
    pub detail: String,
}

impl Host {
    /// An empty host: no observers, no gates, no transformers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HostInner {
                state: Mutex::new(HostState::default()),
                next_id: AtomicU64::new(1),
            }),
            scopes: Vec::new(),
        }
    }

    /// Install a plugin.
    ///
    /// The returned handle owns every registration the install made,
    /// so dropping the handle uninstalls the plugin. The install
    /// order is the execution order.
    pub fn install(&mut self, plugin: &dyn Plugin) -> PluginHandle {
        self.scopes.push(Vec::new());
        let plugin_handle = plugin.install(self);
        let scope = self.scopes.pop().unwrap_or_default();
        // A registration the plugin dropped inside the install is
        // dead: the host must not keep it alive through the scope's
        // clone. The scope owns the registrations the plugin kept.
        let mut kept = Vec::with_capacity(scope.len());
        for registration in scope {
            if registration.is_unclaimed() {
                registration.withdraw();
            } else {
                kept.push(registration);
            }
        }
        // The kept registrations are a subset of the plugin's own
        // handle: dropping that one does not withdraw anything.
        drop(plugin_handle);
        PluginHandle::new(kept)
    }

    /// Register an observer.
    pub fn observe(&mut self, hook: Arc<dyn Hook>) -> Registration {
        let name = hook.name().to_string();
        self.register(RegistrationKind::Observer, name, hook, |state, entry| {
            state.observers.push(entry)
        })
    }

    /// Register a gate.
    pub fn gate(&mut self, gate: Arc<dyn Gate>) -> Registration {
        let name = gate.name().to_string();
        self.register(RegistrationKind::Gate, name, gate, |state, entry| {
            state.gates.push(entry)
        })
    }

    /// Register a request transformer.
    pub fn request_transformer(
        &mut self,
        transformer: Arc<dyn RequestTransformer>,
    ) -> Registration {
        let name = transformer.name().to_string();
        self.register(
            RegistrationKind::RequestTransformer,
            name,
            transformer,
            |state, entry| state.request_transformers.push(entry),
        )
    }

    /// Register a result transformer.
    pub fn result_transformer(
        &mut self,
        transformer: Arc<dyn ResultTransformer>,
    ) -> Registration {
        let name = transformer.name().to_string();
        self.register(
            RegistrationKind::ResultTransformer,
            name,
            transformer,
            |state, entry| state.result_transformers.push(entry),
        )
    }

    /// Register a system-prompt section.
    pub fn prompt_section(&mut self, section: PromptSection) -> Registration {
        let name = section.title.clone();
        self.register(
            RegistrationKind::PromptSection,
            name,
            section,
            |state, entry| state.prompt_sections.push(entry),
        )
    }

    /// Register a tool.
    ///
    /// The tool joins the registry the host is bound to. A host that
    /// is not bound yet keeps the tool until it is — the order of a
    /// plugin's install and the agent's binding does not decide
    /// whether the tool is callable.
    ///
    /// A name a core tool already owns is refused: the registry keeps
    /// the core tool, the host's tool stays out of the registry, and
    /// the collision is recorded as a diagnostic. A name a *dynamic*
    /// tool already owns is not a collision: the registry files both
    /// entries, and the withdrawal takes only the host's own.
    pub fn tool(&mut self, tool: Arc<dyn Tool>) -> Registration {
        let name = tool.name().to_string();
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        // The binding, the registration and the row push happen in
        // one critical section: a concurrent bind cannot slip
        // between them and leave the tool out of the registry.
        let (bound, token) = {
            let mut state = self.inner.state.lock();
            let registry = state.registry.clone();
            let token = registry.as_ref().and_then(|registry| {
                registry.register_entry_with_token(ToolEntry::new(Arc::clone(
                    &tool,
                )))
            });
            state.tools.push(ToolRow {
                id,
                name: name.clone(),
                tool: Arc::clone(&tool),
                token: token.clone(),
            });
            (registry.is_some(), token)
        };
        if bound && token.is_none() {
            self.record(
                DiagnosticKind::ToolNameConflict,
                &name,
                "a core tool already owns the name; the host's tool stays \
                 out of the registry",
            );
        }
        let registration = Registration {
            inner: Arc::new(RegistrationInner {
                id,
                kind: RegistrationKind::Tool,
                host: Arc::downgrade(&self.inner),
            }),
        };
        self.track(registration.clone());
        registration
    }

    /// Register a delegating tool.
    ///
    /// The loop merges the host's delegators into the same name-keyed
    /// table the agent's own installs go into, so a plugin may install a
    /// delegator either way. The registration is reversible: dropping it
    /// withdraws the delegator from the next run.
    ///
    /// Precedence for a name is by source, not by install time: an
    /// `ReActAgent::with_delegator` install wins, then the host's rows in
    /// registration order. A row that loses a name stays registered —
    /// `delegators` still lists it — but is not dispatched for that name;
    /// the loop logs a warning naming it.
    pub fn delegator(&mut self, tool: Arc<dyn DelegatingTool>) -> Registration {
        let name = tool
            .definitions()
            .first()
            .map(|definition| definition.name.clone())
            .unwrap_or_default();
        self.register(
            RegistrationKind::Delegator,
            name,
            tool,
            |state, entry| state.delegators.push(entry),
        )
    }

    /// Bind the host to an agent's tool registry.
    ///
    /// The host's tools join the registry now, and every tool
    /// registered later joins it as it is registered. Withdrawing a
    /// tool registration withdraws it from the registry too — and only
    /// it: another party's entry under the same name stays.
    ///
    /// Binding is idempotent by identity: every agent built over the
    /// same deployment shares one registry, so each of them binds the
    /// same `Arc` and the second bind is a no-op. A *different*
    /// registry is refused — the tokens already filed name entries in
    /// the original registry, and moving the host would leave the two
    /// sides disagreeing about where its tools live. The original
    /// binding stays in force.
    ///
    /// Returns what *this call* refused, so the caller reports it
    /// instead of reading the host's whole diagnostic history: one
    /// [`DiagnosticKind::RegistryAlreadyBound`] when the registry is
    /// refused — and then nothing else, because no tool of the host's
    /// was touched — or one [`DiagnosticKind::ToolNameConflict`] per
    /// pending tool the registry would not take (a name a core tool
    /// owns). An empty list means everything this bind offered
    /// landed. The host's diagnostic history keeps its own copy.
    #[must_use = "a refused bind must be reported, not swallowed"]
    pub fn bind(&self, registry: Arc<ToolRegistry>) -> Vec<Diagnostic> {
        let (refused, pending) = self.claim_binding(&registry);
        if refused {
            return vec![self.refuse(
                DiagnosticKind::RegistryAlreadyBound,
                "tool registry",
                "the host is already bound to a different registry; \
                 the original binding is kept",
            )];
        }
        let mut refusals = Vec::new();
        for (id, name, tool) in pending {
            let token =
                registry.register_entry_with_token(ToolEntry::new(tool));
            let refused = token.is_none();
            // The registration was withdrawn while the bind ran: the
            // tool must not stay in the registry.
            if !self.record_bound_token(id, token.clone()) {
                withdraw_token(&registry, token);
                continue;
            }
            if refused {
                refusals.push(self.refuse(
                    DiagnosticKind::ToolNameConflict,
                    &name,
                    "a core tool already owns the name; the host's tool \
                     stays out of the registry",
                ));
            }
        }
        refusals
    }

    // Claim the binding under one lock pass: `(false, pending)` when
    // this bind owns the registry, `(true, empty)` when a different
    // registry already does.
    fn claim_binding(
        &self,
        registry: &Arc<ToolRegistry>,
    ) -> (bool, Vec<PendingTool>) {
        let mut state = self.inner.state.lock();
        match state.registry.as_ref() {
            // The same registry: every agent over one deployment
            // binds it, and only the first bind does any work.
            Some(bound) if Arc::ptr_eq(bound, registry) => (false, Vec::new()),
            // A different registry: keep the original binding.
            Some(_) => (true, Vec::new()),
            None => {
                state.registry = Some(Arc::clone(registry));
                (
                    false,
                    state
                        .tools
                        .iter()
                        .filter(|row| row.token.is_none())
                        .map(|row| {
                            (row.id, row.name.clone(), Arc::clone(&row.tool))
                        })
                        .collect(),
                )
            }
        }
    }

    // Record the token one tool's row was filed under. `false` means
    // the row is gone: the registration was withdrawn while the bind
    // ran.
    fn record_bound_token(
        &self,
        id: u64,
        token: Option<RegistrationToken>,
    ) -> bool {
        let mut state = self.inner.state.lock();
        match state.tools.iter_mut().find(|row| row.id == id) {
            Some(row) => {
                row.token = token;
                true
            }
            None => false,
        }
    }

    /// Run every observer in registration order.
    ///
    /// A decision that ends the pass wins over the asks collected
    /// before it; an ask is collected and the pass continues. An
    /// observer that panics is disabled: the panic is a bug in the
    /// observer, not a reason to end the run.
    pub async fn dispatch_observers(&self, event: &HookEvent) -> Arbitration {
        // The snapshot carries the registration itself, not its name:
        // the name is only needed when something goes wrong, and a
        // clone per dispatch would cost on the hot path.
        let entries: Vec<(u64, Arc<dyn Hook>)> = {
            let state = self.inner.state.lock();
            state
                .observers
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| (entry.id, Arc::clone(&entry.value)))
                .collect()
        };
        let mut arbitration = Arbitration::default();
        for (id, hook) in entries {
            let decision = match AssertUnwindSafe(hook.observe(event))
                .catch_unwind()
                .await
            {
                Ok(decision) => decision,
                Err(_) => {
                    self.disable(RegistrationKind::Observer, id, hook.name());
                    continue;
                }
            };
            let decision = self.accept(event, decision, hook.name());
            match decision {
                HookDecision::Allow => {}
                HookDecision::Ask { reason } => arbitration.asks.push(reason),
                other => {
                    arbitration.decision = other;
                    break;
                }
            }
        }
        arbitration
    }

    /// Run every gate in registration order.
    ///
    /// A gate that fails or panics blocks the call: a gate that
    /// could not answer does not get to approve it.
    pub async fn dispatch_gates(&self, event: &HookEvent) -> Arbitration {
        let entries: Vec<(u64, Arc<dyn Gate>)> = {
            let state = self.inner.state.lock();
            state
                .gates
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| (entry.id, Arc::clone(&entry.value)))
                .collect()
        };
        let mut arbitration = Arbitration::default();
        for (_id, gate) in entries {
            let outcome =
                AssertUnwindSafe(gate.check(event)).catch_unwind().await;
            let decision = match outcome {
                Ok(Ok(decision)) => self.accept(event, decision, gate.name()),
                Ok(Err(reason)) => {
                    self.record(
                        DiagnosticKind::GateFailed,
                        gate.name(),
                        format!("the gate failed: {reason}"),
                    );
                    blocked(gate.name(), "the gate failed")
                }
                Err(_) => {
                    self.record(
                        DiagnosticKind::GateFailed,
                        gate.name(),
                        "the gate panicked",
                    );
                    blocked(gate.name(), "the gate panicked")
                }
            };
            match decision {
                HookDecision::Allow => {}
                HookDecision::Ask { reason } => arbitration.asks.push(reason),
                other => {
                    arbitration.decision = other;
                    break;
                }
            }
        }
        arbitration
    }

    /// Run the request transformers in registration order.
    ///
    /// Each transformer sees the parameters the previous one left, so
    /// the chain composes. A transformer that panics is disabled and
    /// the chain continues.
    pub async fn apply_request_transformers(&self, params: &mut RequestParams) {
        let entries: Vec<(u64, Arc<dyn RequestTransformer>)> = {
            let state = self.inner.state.lock();
            state
                .request_transformers
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| (entry.id, Arc::clone(&entry.value)))
                .collect()
        };
        for (id, transformer) in entries {
            let outcome = AssertUnwindSafe(transformer.transform(params))
                .catch_unwind()
                .await;
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => {
                    self.record(
                        DiagnosticKind::TransformerFailed,
                        transformer.name(),
                        reason,
                    );
                }
                Err(_) => {
                    self.disable(
                        RegistrationKind::RequestTransformer,
                        id,
                        transformer.name(),
                    );
                }
            }
        }
    }

    /// Run the result transformers in registration order.
    ///
    /// Returns the registered transformers that ran when the aggregate
    /// output changed, so the loop can record provenance for a result a
    /// transformer altered before it was committed. The change is
    /// measured here — the output is snapshotted and compared — not
    /// read from the transformer, which has no way to report it. An
    /// empty result means the output is exactly what the tool returned.
    #[must_use]
    pub async fn apply_result_transformers(
        &self,
        output: &mut ToolOutput,
    ) -> Vec<String> {
        let entries: Vec<(u64, Arc<dyn ResultTransformer>)> = {
            let state = self.inner.state.lock();
            state
                .result_transformers
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| (entry.id, Arc::clone(&entry.value)))
                .collect()
        };
        // Nothing registered, nothing to measure: the common path takes
        // no snapshot.
        if entries.is_empty() {
            return Vec::new();
        }
        let before = output.clone();
        let mut ran = Vec::with_capacity(entries.len());
        for (id, transformer) in entries {
            let outcome = AssertUnwindSafe(transformer.transform(output))
                .catch_unwind()
                .await;
            match outcome {
                Ok(Ok(())) => ran.push(transformer.name().to_string()),
                Ok(Err(reason)) => {
                    // A transformer that failed may still have written
                    // part of its rewrite; whether it changed anything
                    // is decided by the comparison below, not here.
                    self.record(
                        DiagnosticKind::TransformerFailed,
                        transformer.name(),
                        reason,
                    );
                    ran.push(transformer.name().to_string());
                }
                Err(_) => {
                    self.disable(
                        RegistrationKind::ResultTransformer,
                        id,
                        transformer.name(),
                    );
                }
            }
        }
        if *output == before {
            return Vec::new();
        }
        ran
    }

    /// Apply one decision at one point.
    ///
    /// A decision the point does not accept is ignored with a
    /// diagnostic: the run continues, and the diagnostic says why. A
    /// decision that could not be honoured must never be mistaken for
    /// one that was.
    pub fn accept(
        &self,
        event: &HookEvent,
        decision: HookDecision,
        source: &str,
    ) -> HookDecision {
        if event.accepts(&decision) {
            return decision;
        }
        self.record(
            DiagnosticKind::UnsupportedDecision,
            source,
            format!(
                "`{}` does not accept {}",
                event_name(event),
                describe(&decision)
            ),
        );
        HookDecision::Allow
    }

    /// The prompt sections, in registration order.
    #[must_use]
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        let state = self.inner.state.lock();
        state
            .prompt_sections
            .iter()
            .map(|entry| entry.value.clone())
            .collect()
    }

    /// The registered tools, in registration order.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let state = self.inner.state.lock();
        state
            .tools
            .iter()
            .map(|entry| Arc::clone(&entry.tool))
            .collect()
    }

    /// The registered delegators, in registration order.
    #[must_use]
    pub fn delegators(&self) -> Vec<Arc<dyn DelegatingTool>> {
        let state = self.inner.state.lock();
        state
            .delegators
            .iter()
            .map(|entry| Arc::clone(&entry.value))
            .collect()
    }

    /// How many registrations the host holds.
    #[must_use]
    pub fn registration_count(&self) -> usize {
        let state = self.inner.state.lock();
        state.observers.len()
            + state.gates.len()
            + state.request_transformers.len()
            + state.result_transformers.len()
            + state.prompt_sections.len()
            + state.tools.len()
            + state.delegators.len()
    }

    /// Whether the host holds registrations that consume events.
    ///
    /// The loop uses it to skip building an event nobody will see.
    #[must_use]
    pub fn has_event_registrations(&self) -> bool {
        let state = self.inner.state.lock();
        !state.observers.is_empty() || !state.gates.is_empty()
    }

    /// Whether the host can change the request's parameters.
    ///
    /// The loop uses it to skip the whole rewrite pass — the params
    /// snapshot, the dispatch and the write-back — on a run where
    /// nothing can rewrite the request.
    #[must_use]
    pub fn has_request_rewriters(&self) -> bool {
        let state = self.inner.state.lock();
        !state.request_transformers.is_empty() || !state.observers.is_empty()
    }

    /// Whether the host can change a tool result.
    ///
    /// The loop uses it to skip the whole rewrite pass — the snapshot,
    /// the transformers and the comparison — on a run where nothing can
    /// rewrite a result.
    #[must_use]
    pub fn has_result_rewriters(&self) -> bool {
        let state = self.inner.state.lock();
        state
            .result_transformers
            .iter()
            .any(|entry| !entry.disabled)
    }

    /// The diagnostics recorded so far, oldest first.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let state = self.inner.state.lock();
        state.diagnostics.clone()
    }

    fn register<T>(
        &mut self,
        kind: RegistrationKind,
        name: String,
        value: T,
        push: impl FnOnce(&mut HostState, Entry<T>),
    ) -> Registration {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = self.inner.state.lock();
            push(
                &mut state,
                Entry {
                    id,
                    name,
                    value,
                    disabled: false,
                },
            );
        }
        let registration = Registration {
            inner: Arc::new(RegistrationInner {
                id,
                kind,
                host: Arc::downgrade(&self.inner),
            }),
        };
        self.track(registration.clone());
        registration
    }

    // Record a registration in the install in progress, so the
    // handle can own it. Outside an install there is nothing to
    // record: the caller holds the only claim.
    fn track(&mut self, registration: Registration) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(registration);
        }
    }

    fn disable(&self, kind: RegistrationKind, id: u64, name: &str) {
        {
            let mut state = self.inner.state.lock();
            match kind {
                RegistrationKind::Observer => {
                    mark_disabled(&mut state.observers, id);
                }
                RegistrationKind::Gate => {
                    mark_disabled(&mut state.gates, id);
                }
                RegistrationKind::RequestTransformer => {
                    mark_disabled(&mut state.request_transformers, id);
                }
                RegistrationKind::ResultTransformer => {
                    mark_disabled(&mut state.result_transformers, id);
                }
                RegistrationKind::PromptSection => {}
                RegistrationKind::Tool | RegistrationKind::Delegator => {}
            }
        }
        self.record(
            DiagnosticKind::Disabled,
            name,
            "the registration panicked; it is disabled",
        );
    }

    fn record(
        &self,
        kind: DiagnosticKind,
        source: &str,
        detail: impl Into<String>,
    ) {
        let mut state = self.inner.state.lock();
        state.diagnostics.push(Diagnostic {
            kind,
            source: source.to_string(),
            detail: detail.into(),
        });
        // A misbehaving plugin must not grow the host without bound.
        let overflow = state.diagnostics.len().saturating_sub(DIAGNOSTIC_LIMIT);
        state.diagnostics.drain(..overflow);
    }

    // Record a refusal and hand the same value back to the caller.
    fn refuse(
        &self,
        kind: DiagnosticKind,
        source: &str,
        detail: &str,
    ) -> Diagnostic {
        self.record(kind, source, detail);
        Diagnostic {
            kind,
            source: source.to_string(),
            detail: detail.to_string(),
        }
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

// Withdraw every registration but the one with `id`.
fn retain_id<T>(entries: &mut Vec<Entry<T>>, id: u64) {
    entries.retain(|entry| entry.id != id);
}

// Disable the registration with `id`.
fn mark_disabled<T>(entries: &mut [Entry<T>], id: u64) {
    for entry in entries.iter_mut() {
        if entry.id == id {
            entry.disabled = true;
        }
    }
}

// The block a failing gate produces.
fn blocked(name: &str, cause: &str) -> HookDecision {
    HookDecision::Block {
        reason: format!("gate `{name}` could not answer: {cause}"),
    }
}

// A readable name for a decision, for the diagnostics.
fn describe(decision: &HookDecision) -> String {
    match decision {
        HookDecision::Allow => "allow".to_string(),
        HookDecision::Block { reason } => format!("block ({reason})"),
        HookDecision::Ask { reason } => format!("ask ({reason})"),
        HookDecision::ReplaceParams(_) => "replace params".to_string(),
        HookDecision::Terminate { reason } => format!("terminate ({reason})"),
    }
}
