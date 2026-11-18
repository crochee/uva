//! The host: the typed registration frontend over the seam chains.
//!
//! A host belongs to one agent — `ReActAgent::with_host` installs it
//! — so two agents never share one policy. Every registration
//! returns a [`Registration`](crate::agent::host::Registration):
//! dropping it withdraws the
//! registration, so a plugin can be uninstalled without rebuilding
//! the agent.
//!
//! ## Where each kind dispatches
//!
//! Every extension point is a registration here; where it runs is
//! the kind's home:
//!
//! | Kind | Home |
//! |---|---|
//! | [`Hook`](crate::agent::host::Hook) | middleware on the six pass chains, plus the residual hooks table for the five observation points (`before_run`, `after_run`, `before_provider_call`, `on_error`, `on_compaction`) |
//! | [`Gate`](crate::agent::host::Gate) | middleware on the six pass chains, fail-closed |
//! | [`RequestTransformer`](crate::agent::host::RequestTransformer) | middleware on the `llm/request` chain |
//! | [`Guard`](uva_steering::Guard) | the guard pipeline (`tool_guards`), a sibling of `tool/pre-execute` — data-plane, stays a table |
//! | [`Hint`](uva_steering::Hint) | a scoped read the run queries, not a pass — stays a table |
//! | [`Tracker`](uva_steering::tracker::Tracker) | a table object the loop reads at its boundaries — stays |
//! | [`OutputTransformer`](uva_steering::OutputTransformer) | a rewrite of the run's final text — stays a table |
//! | [`ResultTransformer`](crate::agent::host::ResultTransformer) | a rewrite of a tool result outside the pass payload — stays a table |
//! | [`PromptSection`](crate::agent::host::PromptSection) | a collection the prompt assembly snapshots once per run, not a chain pass — stays a table |
//! | tool | the bound [`ToolRegistry`](uva_tool::ToolRegistry), via token |
//!
//! A chain registration's lifetime is the returned handle's, exactly
//! like a table row's: the middleware withdraws with the row.
//!
//! The remaining tables are published as one immutable
//! `HostSnapshot`; a registration change builds a new snapshot and
//! swaps it. A reading hot path therefore clones one `Arc` and reads
//! it without taking a lock.
//!
//! ## The arbitration rules
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
//!   security decisions;
//! - guards run in registration order and the first denial wins, the
//!   same pipeline `uva_steering::run_guards` defines.

use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Weak},
};

use futures::FutureExt;
use parking_lot::{Mutex, RwLock};
// The prompt-section vocabulary lives one layer down with the run
// input that carries it (`uva-agent`); the deep path
// `uva_harness::agent::host::PromptSection` and the crate-root facade
// both re-export it from there.
pub use uva_agent::PromptSection;
use uva_context::AgentState;
use uva_events::WarningKind;
// The interception contract lives one layer down (ADR 0004): the
// traits and the dispatch context are `uva-hook`'s, re-exported here
// so neither the crate-root facade nor this deep path moves.
pub use uva_hook::{
    Arbitration,
    Gate,
    Hook,
    HookContext,
    RequestTransformer,
    ResultTransformer,
};
use uva_hook::{
    HookPayload,
    arbitration::{Fact, PanicPolicy, Refusal, Ruling, refusal, rule},
};
use uva_steering::{
    Guard,
    GuardOutcome,
    Hint,
    HintPriority,
    HookDecision,
    HookEvent,
    InjectionPoint,
    OutputTransformer,
    guards::{
        LoopDetectionGuard,
        PromptInjectionGuard,
        ShellDenyGuard,
        ToolBudgetGuard,
        WorkspaceBoundaryGuard,
    },
    hint::{ContextBudgetHint, IterationReminderHint, TruncationHint},
    run_guards,
    tracker::{AdaptiveTracker, Tracker},
};
use uva_tool::{RegistrationToken, Tool, ToolEntry, ToolOutput, ToolRegistry};

use crate::{agent::sink::EventSink, seams::VetoRow};

#[cfg(test)]
mod tests;

// How many diagnostics the host keeps.
const DIAGNOSTIC_LIMIT: usize = 64;

// Record a policy veto as a durable `hook_block` row.
//
// The row is the veto's own record: the model-visible tool result
// says that *something* refused the call, and the live warning that
// named the policy was live-only — so a reloaded session had no
// answer to "which policy blocked this, and why".
//
// Only a veto of a tool call is recorded: a denial at another seam
// refuses no call, so it has no call to name. Both denials count —
// `Block` and `Terminate` each refuse the call. The row is log-only —
// the surface fold ignores it.
//
// The sink is an explicit argument, not a `HookContext` field: the
// context a registration sees is decoupled run state only (ADR 0004),
// and the writer half stays with the component that owns the run.
fn record_hook_block(
    sink: &EventSink,
    name: &str,
    decision: &HookDecision,
    event: &HookEvent,
) {
    if let Some(veto) = veto_row(name, decision, event) {
        veto.record(sink);
    }
}

// The veto one adapter layer owes a pass: a denial of a tool call,
// with everything the `hook_block` row names. The chain adapters
// cannot reach the run's sink from inside the chain, so they fold the
// veto into their short-circuit value and the seam layer records it
// where the sink lives (`Decided`).
//
// Both denial kinds count at `BeforeTool`: `Block` renders an error
// result the model can react to, `Terminate` ends the run — either
// way the call was refused, and the row is the reloaded session's only
// answer to "which policy refused this call".
pub(crate) fn veto_row(
    name: &str,
    decision: &HookDecision,
    event: &HookEvent,
) -> Option<VetoRow> {
    let reason = match decision {
        HookDecision::Block { reason } | HookDecision::Terminate { reason } => {
            reason
        }
        _ => return None,
    };
    let HookEvent::BeforeTool {
        tool_name, call_id, ..
    } = event
    else {
        return None;
    };
    Some(VetoRow {
        name: name.to_owned(),
        reason: reason.clone(),
        tool_name: tool_name.clone(),
        call_id: call_id.clone(),
    })
}

// Apply one decision at one point.
//
// The point's own table decides — the owner's rule — and the host's
// answer to "the point does not accept this" is the same everywhere:
// the decision does not apply, and the fact travels with the pass so
// the operator can see it.
fn accept(
    event: &HookEvent,
    decision: HookDecision,
    source: &str,
) -> (HookDecision, Option<Fact>) {
    match rule(event, decision, source) {
        Ruling::Stands(decision) => (decision, None),
        Ruling::Unsupported { fact, .. } => (HookDecision::Allow, Some(fact)),
    }
}

// Report one fact in the run's durable record.
//
// The fact is what the arbitration learned about a registration; the
// warning row is where an operator reads it. The registry's path
// reports the same facts the same way — one channel, two surfaces.
fn report_fact(sink: &EventSink, fact: &Fact) {
    sink.warn(WarningKind::Hook, None, fact.message().to_owned());
}

// The host's panic policy: an in-process registration that panics is
// broken, so it is withdrawn and the fact says so.
//
// The registry keeps an external plugin registered instead — the
// policies are named in one place (`PanicPolicy`), so the two
// surfaces cannot drift into a third.
const PANIC_POLICY: PanicPolicy = PanicPolicy::Withdraw;

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
    // Whether the host's install bookkeeping is the only holder: the
    // plugin dropped its own copy, so the registration is dead.
    fn is_unclaimed(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    // Withdraw the registration now, without waiting for the last
    // handle to drop.
    fn withdraw(&self) {
        if let Some(host) = self.inner.host.upgrade() {
            host.withdraw(self.inner.kind, self.inner.id);
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
    Hook,
    Gate,
    Guard,
    Hint,
    Tracker,
    OutputTransformer,
    RequestTransformer,
    ResultTransformer,
    PromptSection,
    Tool,
}

impl Drop for RegistrationInner {
    fn drop(&mut self) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        host.withdraw(self.kind, self.id);
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
/// # Deprecation direction
///
/// The kernel's effects — through `uva-kernel`'s `Context` and the
/// plugin crates' `Plugin` implementations — are the registration
/// target. `Host` remains while the chat path still builds each run
/// through the consumer's run assembly, which hands every agent a
/// host clone; removing it before that path moves to the `agentLoop`
/// service would break chat. New code should register through kernel
/// effects, not this table.
pub struct Host {
    inner: Arc<HostInner>,
    // One scope per install in progress: the registrations that
    // install made, so the returned handle can own them.
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
    // The kernel chains the seam keys front for. One root context the
    // chains (and their reversible registrations) live on.
    seams: crate::seams::SeamChains,
    // The chain middleware handles of the registrations whose kind
    // dispatches on a seam chain (hooks, gates, request
    // transformers), keyed by registration id so a withdrawal or a
    // panic-disable removes the chain entries with the table row.
    chain_regs:
        Mutex<std::collections::HashMap<u64, Vec<uva_kernel::EffectHandle>>>,
    // Registrations whose kind has no table row (gates, request
    // transformers): they live only as chain middleware, so the
    // count of registrations the host holds includes them.
    chain_only: Mutex<std::collections::HashSet<u64>>,
    state: Mutex<HostState>,
    // The published dispatch tables. Rebuilt on every change.
    snapshot: RwLock<Arc<HostSnapshot>>,
    next_id: std::sync::atomic::AtomicU64,
    // The registrations [`Host::default_policy`] installed. They
    // live as long as the host does — a registration withdraws when
    // its last handle drops, so a default policy must not let its
    // handles fall out of scope.
    _defaults: Mutex<Vec<Registration>>,
}

#[derive(Default)]
struct HostState {
    // Hooks live twice by design: once as chain middleware for the
    // six pass seams, once in this residual table for the five
    // observation points, which have no chain.
    hooks: Vec<Entry<Arc<dyn Hook>>>,
    guards: Vec<Entry<Arc<dyn Guard>>>,
    hints: Vec<Entry<Arc<dyn Hint>>>,
    trackers: Vec<Entry<Arc<dyn Tracker>>>,
    output_transformers: Vec<Entry<Arc<dyn OutputTransformer>>>,
    result_transformers: Vec<Entry<Arc<dyn ResultTransformer>>>,
    prompt_sections: Vec<Entry<PromptSection>>,
    tools: Vec<ToolRow>,
    // The registry the host's tools live in. The host is bound to it
    // when the agent installs the host; before that, the tools wait
    // for the binding.
    registry: Option<Weak<ToolRegistry>>,
    diagnostics: Vec<Diagnostic>,
}

/// The immutable dispatch view one pass reads.
#[derive(Default)]
pub(crate) struct HostSnapshot {
    pub(crate) hooks: Vec<Entry<Arc<dyn Hook>>>,
    pub(crate) guards: Vec<Arc<dyn Guard>>,
    pub(crate) hints: Vec<Entry<Arc<dyn Hint>>>,
    pub(crate) trackers: Vec<Arc<dyn Tracker>>,
    pub(crate) output_transformers: Vec<Arc<dyn OutputTransformer>>,
    pub(crate) result_transformers: Vec<Entry<Arc<dyn ResultTransformer>>>,
    pub(crate) prompt_sections: Vec<PromptSection>,
    pub(crate) has_event_registrations: bool,
    pub(crate) has_result_rewriters: bool,
}

impl HostSnapshot {
    fn build(state: &HostState) -> Self {
        let hooks: Vec<_> = state
            .hooks
            .iter()
            .filter(|entry| !entry.disabled)
            .cloned()
            .collect();
        let result_transformers: Vec<_> = state
            .result_transformers
            .iter()
            .filter(|entry| !entry.disabled)
            .cloned()
            .collect();
        Self {
            has_event_registrations: !hooks.is_empty(),
            has_result_rewriters: !result_transformers.is_empty(),
            hooks,
            guards: state
                .guards
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| Arc::clone(&entry.value))
                .collect(),
            hints: state
                .hints
                .iter()
                .filter(|entry| !entry.disabled)
                .cloned()
                .collect(),
            trackers: state
                .trackers
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| Arc::clone(&entry.value))
                .collect(),
            output_transformers: state
                .output_transformers
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| Arc::clone(&entry.value))
                .collect(),
            prompt_sections: state
                .prompt_sections
                .iter()
                .filter(|entry| !entry.disabled)
                .map(|entry| entry.value.clone())
                .collect(),
            result_transformers,
        }
    }
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
pub(crate) struct Entry<T> {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    // The registration panicked; it was disabled.
    Disabled,
    // The transformer reported a failure; its change was skipped.
    TransformerFailed,
    // A tool's name is already owned by a core tool; the host's
    // tool stays out of the registry.
    ToolNameConflict,
    // The host is already bound to a different registry; the first wins.
    RegistryAlreadyBound,
}

/// One recorded diagnostic.
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
    /// An empty host: no registrations at all.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HostInner {
                seams: crate::seams::SeamChains::new(),
                chain_regs: Mutex::new(std::collections::HashMap::new()),
                chain_only: Mutex::new(std::collections::HashSet::new()),
                state: Mutex::new(HostState::default()),
                snapshot: RwLock::new(Arc::new(HostSnapshot::default())),
                next_id: std::sync::atomic::AtomicU64::new(1),
                _defaults: Mutex::new(Vec::new()),
            }),
            scopes: Vec::new(),
        }
    }

    #[must_use]
    /// The kernel waterfall a seam key fronts for.
    ///
    /// `key` is one of the [`crate::seams`] chain names. A plugin
    /// registers middleware on the returned chain and keeps the
    /// [`uva_kernel::EffectHandle`] for as long as it wants the
    /// middleware to run; the chain dispatches before the host's own
    /// registrations for that seam.
    pub fn waterfall(&self, key: &str) -> Option<&uva_kernel::Waterfall> {
        self.inner.seams.get(key)
    }

    /// The root context the seam chains resolve services through.
    pub(crate) fn seam_context(&self) -> uva_kernel::Context {
        self.inner.seams.context()
    }

    /// The production policy: defensive guards, budget / iteration /
    /// truncation hints, adaptive concurrency and the debug logging
    /// hook.
    ///
    /// A base-library agent starts with [`Host::new`] and no policy;
    /// a deployment installs this explicitly. Tuned to never
    /// interfere with legitimate work: the loop detector only fires on
    /// the 3rd identical consecutive call, the tool budget (250)
    /// exceeds the theoretical maximum of a bounded run, and the shell
    /// list only vetoes machine-level unrecoverable commands.
    #[must_use]
    pub fn default_policy(
        workspace_root: impl Into<std::path::PathBuf>,
    ) -> Self {
        let mut host = Self::new();
        // The handles must outlive this call: a registration withdraws
        // its entry when dropped, so the host itself owns them.
        let defaults = vec![
            host.guard(Arc::new(LoopDetectionGuard::new(3))),
            host.guard(Arc::new(ToolBudgetGuard::new(250))),
            host.guard(Arc::new(ShellDenyGuard::new())),
            host.guard(Arc::new(WorkspaceBoundaryGuard::new(workspace_root))),
            host.guard(Arc::new(PromptInjectionGuard::new())),
            host.hint(Arc::new(ContextBudgetHint::new(0.85))),
            host.hint(Arc::new(IterationReminderHint::new(10))),
            host.hint(Arc::new(TruncationHint)),
            host.tracker(Arc::new(AdaptiveTracker::new(8))),
            host.hook(Arc::new(uva_steering::LoggingHook)),
        ];
        *host.inner._defaults.lock() = defaults;
        host
    }

    /// Install a plugin.
    pub fn install(&mut self, plugin: &dyn Plugin) -> PluginHandle {
        self.scopes.push(Vec::new());
        let plugin_handle = plugin.install(self);
        let scope = self.scopes.pop().unwrap_or_default();
        let mut kept = Vec::with_capacity(scope.len());
        for registration in scope {
            if registration.is_unclaimed() {
                registration.withdraw();
            } else {
                kept.push(registration);
            }
        }
        drop(plugin_handle);
        PluginHandle::new(kept)
    }

    /// Register an observer.
    ///
    /// The registration lands on every seam chain as middleware (the
    /// six pass seams) *and* in the residual hooks table the five
    /// observation points dispatch. Dropping the returned handle
    /// withdraws both.
    pub fn hook(&mut self, hook: Arc<dyn Hook>) -> Registration {
        let name = hook.name().to_string();
        let id = self.next_id();
        let weak = Arc::downgrade(&self.inner);
        let mut handles = Vec::new();
        for key in crate::seams::pass_chain_keys() {
            let Some(chain) = self.inner.seams.get(key) else {
                continue;
            };
            if let Ok(handle) = chain.register(hook_adapter(
                std::sync::Weak::clone(&weak),
                id,
                name.clone(),
                Arc::clone(&hook),
            )) {
                handles.push(handle);
            }
        }
        self.inner.chain_regs.lock().insert(id, handles);
        self.register_value(
            RegistrationKind::Hook,
            name,
            hook,
            |state, entry| state.hooks.push(entry),
            id,
        )
    }

    /// Register a gate.
    ///
    /// The registration is middleware on every seam chain: a gate is
    /// the security capability, fails closed (an `Err` or a panic is
    /// a block), and short-circuits the pass when it decides.
    pub fn gate(&mut self, gate: Arc<dyn Gate>) -> Registration {
        let name = gate.name().to_string();
        let id = self.next_id();
        let mut handles = Vec::new();
        for key in crate::seams::pass_chain_keys() {
            let Some(chain) = self.inner.seams.get(key) else {
                continue;
            };
            if let Ok(handle) =
                chain.register(gate_adapter(name.clone(), Arc::clone(&gate)))
            {
                handles.push(handle);
            }
        }
        self.inner.chain_regs.lock().insert(id, handles);
        self.inner.chain_only.lock().insert(id);
        self.publish();
        self.finish_registration(id, RegistrationKind::Gate)
    }

    /// Register a synchronous guard.
    pub fn guard(&mut self, guard: Arc<dyn Guard>) -> Registration {
        let name = guard.name().to_string();
        self.register(RegistrationKind::Guard, name, guard, |state, entry| {
            state.guards.push(entry)
        })
    }

    /// Register an advisory hint.
    pub fn hint(&mut self, hint: Arc<dyn Hint>) -> Registration {
        let name = hint.name().to_string();
        self.register(RegistrationKind::Hint, name, hint, |state, entry| {
            state.hints.push(entry)
        })
    }

    /// Register a progress tracker.
    pub fn tracker(&mut self, tracker: Arc<dyn Tracker>) -> Registration {
        let name = "tracker".to_string();
        self.register(
            RegistrationKind::Tracker,
            name,
            tracker,
            |state, entry| state.trackers.push(entry),
        )
    }

    /// Register a tool-output transformer.
    pub fn output_transformer(
        &mut self,
        transformer: Arc<dyn OutputTransformer>,
    ) -> Registration {
        let name = "output_transformer".to_string();
        self.register(
            RegistrationKind::OutputTransformer,
            name,
            transformer,
            |state, entry| state.output_transformers.push(entry),
        )
    }

    /// Register a request transformer.
    ///
    /// The registration is middleware on the `llm/request` chain: it
    /// rewrites the pass's parameters in place and lets the chain
    /// continue. A failure is recorded and skipped; a panic disables
    /// the registration.
    pub fn request_transformer(
        &mut self,
        transformer: Arc<dyn RequestTransformer>,
    ) -> Registration {
        let name = transformer.name().to_string();
        let id = self.next_id();
        let weak = Arc::downgrade(&self.inner);
        let mut handles = Vec::new();
        if let Some(chain) = self.inner.seams.get(crate::seams::LLM_REQUEST)
            && let Ok(handle) = chain.register(request_transformer_adapter(
                std::sync::Weak::clone(&weak),
                id,
                name.clone(),
                Arc::clone(&transformer),
            ))
        {
            handles.push(handle);
        }
        self.inner.chain_regs.lock().insert(id, handles);
        self.inner.chain_only.lock().insert(id);
        self.publish();
        self.finish_registration(id, RegistrationKind::RequestTransformer)
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
        let id = self.next_id();
        let (bound, token) = {
            let mut state = self.inner.state.lock();
            let registry =
                state.registry.clone().and_then(|weak| weak.upgrade());
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
        self.publish();
        if bound && token.is_none() {
            self.record(
                DiagnosticKind::ToolNameConflict,
                &name,
                "a core tool already owns the name; the host's tool stays \
                 out of the registry",
            );
        }
        self.finish_registration(id, RegistrationKind::Tool)
    }

    /// Bind the host to an agent's tool registry.
    ///
    /// Returns what *this call* refused, so the caller reports it
    /// instead of reading the host's whole diagnostic history.
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

    fn claim_binding(
        &self,
        registry: &Arc<ToolRegistry>,
    ) -> (bool, Vec<PendingTool>) {
        let mut state = self.inner.state.lock();
        match state.registry.as_ref().and_then(|weak| weak.upgrade()) {
            Some(bound) if Arc::ptr_eq(&bound, registry) => (false, Vec::new()),
            Some(_) => (true, Vec::new()),
            None => {
                state.registry = Some(Arc::downgrade(registry));
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

    /// The published dispatch view. One `Arc` clone on the hot path.
    pub(crate) fn snapshot(&self) -> Arc<HostSnapshot> {
        Arc::clone(&self.inner.snapshot.read())
    }

    /// Run every observer in registration order.
    ///
    /// A policy veto records its `hook_block` row through `sink`:
    /// the context a registration reads carries run state only, so
    /// the writer half arrives as an explicit argument (ADR 0004).
    pub async fn dispatch_hooks(
        &self,
        event: &HookEvent,
        ctx: &HookContext<'_>,
        sink: &EventSink,
    ) -> Arbitration {
        let snapshot = self.snapshot();
        let mut arbitration = Arbitration::default();
        for entry in &snapshot.hooks {
            let decision =
                match AssertUnwindSafe(entry.value.observe(event, ctx))
                    .catch_unwind()
                    .await
                {
                    Ok(decision) => decision,
                    Err(_) => {
                        // The host's panic policy: an in-process
                        // registration that panics is broken, so it is
                        // withdrawn and the fact says so.
                        let fact = PANIC_POLICY
                            .fact(entry.value.name(), "the observer panicked");
                        self.disable(
                            RegistrationKind::Hook,
                            entry.id,
                            entry.value.name(),
                        );
                        report_fact(sink, &fact);
                        continue;
                    }
                };
            let (decision, fact) = accept(event, decision, entry.value.name());
            if let Some(fact) = fact {
                report_fact(sink, &fact);
            }
            match decision {
                HookDecision::Allow => {}
                HookDecision::Ask { reason } => arbitration.asks.push(reason),
                other => {
                    record_hook_block(sink, entry.value.name(), &other, event);
                    arbitration.absorb(other);
                    break;
                }
            }
        }
        arbitration
    }

    /// Run the guard pipeline: registration order, first denial wins,
    /// a panic is a denial.
    pub fn dispatch_guards(
        &self,
        action: &uva_steering::Action,
        ctx: &HookContext<'_>,
    ) -> GuardOutcome {
        let snapshot = self.snapshot();
        run_guards(&snapshot.guards, action, ctx.state)
    }

    /// Collect the hints that fire for this state, in priority order
    /// (`Low` first).
    ///
    /// Each item carries the point the loop must inject it at; the
    /// tool-result point is filtered out here because it is evaluated
    /// at commit time by [`Host::tool_result_hints`].
    pub fn hints(
        &self,
        state: &AgentState,
    ) -> Vec<(InjectionPoint, uva_steering::HintMessage)> {
        let snapshot = self.snapshot();
        let crowded = state.context_utilization() >= 0.8;
        let mut fired: Vec<(
            HintPriority,
            InjectionPoint,
            uva_steering::HintMessage,
        )> = Vec::new();
        for entry in &snapshot.hints {
            let hint = &entry.value;
            let point = hint.injection_point();
            // The tool-result point is evaluated at commit time.
            if matches!(point, InjectionPoint::AppendToToolResult { .. }) {
                continue;
            }
            // Consumer-supplied, but the host's panic policy holds
            // here too: a hint that panics is broken, so it is
            // withdrawn rather than asked again every step.
            let Ok(triggered) =
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    hint.should_trigger(state)
                }))
            else {
                self.disable(RegistrationKind::Hint, entry.id, &entry.name);
                continue;
            };
            if !triggered {
                continue;
            }
            let Ok(message) =
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    hint.generate(state)
                }))
            else {
                self.disable(RegistrationKind::Hint, entry.id, &entry.name);
                continue;
            };
            if message.priority == HintPriority::Low && crowded {
                continue;
            }
            fired.push((message.priority, point, message));
        }
        fired.sort_by_key(|(priority, _, _)| *priority);
        fired
            .into_iter()
            .map(|(_, point, message)| (point, message))
            .collect()
    }

    /// Chain every registered output transformer over one tool result.
    ///
    /// The chain is serial: each transformer sees what the previous
    /// one returned. Errors are impossible by contract (a transformer
    /// returns a String), so this never fails.
    pub async fn transform_output(
        &self,
        text: String,
        tool_name: &str,
        state: &AgentState,
    ) -> String {
        let snapshot = self.snapshot();
        let mut text = text;
        for transformer in &snapshot.output_transformers {
            text = transformer.transform(text, tool_name, state).await;
        }
        text
    }

    /// Append every `AppendToToolResult` hint that names `tool_name`.
    pub fn tool_result_hints(
        &self,
        tool_name: &str,
        state: &AgentState,
    ) -> Vec<String> {
        let snapshot = self.snapshot();
        let mut out = Vec::new();
        for entry in &snapshot.hints {
            let hint = &entry.value;
            match hint.injection_point() {
                InjectionPoint::AppendToToolResult {
                    tool_name: hint_tool,
                } if hint_tool == tool_name => {}
                _ => continue,
            }
            match triggered_hint(hint.as_ref(), state) {
                Ok(Some(message)) => out.push(message.content),
                Ok(None) => {}
                Err(()) => {
                    self.disable(RegistrationKind::Hint, entry.id, &entry.name)
                }
            }
        }
        out
    }

    /// Run the result transformers in registration order.
    ///
    /// Returns the transformers that ran when the aggregate output
    /// changed, so the loop can record provenance.
    #[must_use]
    pub async fn apply_result_transformers(
        &self,
        output: &mut ToolOutput,
        ctx: &HookContext<'_>,
    ) -> Vec<String> {
        let snapshot = self.snapshot();
        if snapshot.result_transformers.is_empty() {
            return Vec::new();
        }
        let before = output.clone();
        let mut ran = Vec::with_capacity(snapshot.result_transformers.len());
        for entry in &snapshot.result_transformers {
            let outcome = AssertUnwindSafe(entry.value.transform(output, ctx))
                .catch_unwind()
                .await;
            match outcome {
                Ok(Ok(())) => ran.push(entry.value.name().to_string()),
                Ok(Err(reason)) => {
                    self.record(
                        DiagnosticKind::TransformerFailed,
                        entry.value.name(),
                        reason,
                    );
                    ran.push(entry.value.name().to_string());
                }
                Err(_) => {
                    self.disable(
                        RegistrationKind::ResultTransformer,
                        entry.id,
                        entry.value.name(),
                    );
                }
            }
        }
        if *output == before {
            return Vec::new();
        }
        ran
    }

    /// The prompt sections, in registration order.
    #[must_use]
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        self.snapshot().prompt_sections.clone()
    }

    /// The registered tools, in registration order.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let state = self.inner.state.lock();
        state
            .tools
            .iter()
            .map(|row| Arc::clone(&row.tool))
            .collect()
    }

    /// How many registrations the host holds.
    #[must_use]
    pub fn registration_count(&self) -> usize {
        let state = self.inner.state.lock();
        let chain_only = self.inner.chain_only.lock().len();
        state.hooks.len()
            + chain_only
            + state.guards.len()
            + state.hints.len()
            + state.trackers.len()
            + state.output_transformers.len()
            + state.result_transformers.len()
            + state.prompt_sections.len()
            + state.tools.len()
    }

    /// Whether the host holds registrations that consume events.
    #[must_use]
    pub fn has_event_registrations(&self) -> bool {
        self.snapshot().has_event_registrations
    }

    /// Whether the host can change a tool result.
    #[must_use]
    pub fn has_result_rewriters(&self) -> bool {
        self.snapshot().has_result_rewriters
    }

    /// The diagnostics recorded so far, oldest first.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let state = self.inner.state.lock();
        state.diagnostics.clone()
    }

    fn next_id(&self) -> u64 {
        self.inner
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn finish_registration(
        &mut self,
        id: u64,
        kind: RegistrationKind,
    ) -> Registration {
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

    fn register<T>(
        &mut self,
        kind: RegistrationKind,
        name: String,
        value: T,
        push: impl FnOnce(&mut HostState, Entry<T>),
    ) -> Registration {
        let id = self.next_id();
        self.register_value(kind, name, value, push, id)
    }

    fn register_value<T>(
        &mut self,
        kind: RegistrationKind,
        name: String,
        value: T,
        push: impl FnOnce(&mut HostState, Entry<T>),
        id: u64,
    ) -> Registration {
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
        self.publish();
        self.finish_registration(id, kind)
    }

    fn publish(&self) {
        let state = self.inner.state.lock();
        let snapshot = HostSnapshot::build(&state);
        drop(state);
        *self.inner.snapshot.write() = Arc::new(snapshot);
    }

    fn track(&mut self, registration: Registration) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(registration);
        }
    }

    // Withdraw a registration that broke, and say so.
    //
    // The host's panic policy: an in-process registration that panics
    // is broken, so it stops being called. Where the run's sink is
    // reachable, the fact also lands in the run's durable record; the
    // paths without a sink (hints, result transformers) keep the
    // host's own diagnostic.
    fn disable(&self, kind: RegistrationKind, id: u64, name: &str) {
        self.inner.withdraw(kind, id);
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
        let overflow = state.diagnostics.len().saturating_sub(DIAGNOSTIC_LIMIT);
        state.diagnostics.drain(..overflow);
    }

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

impl HostInner {
    // Record one diagnostic into the state's ring.
    fn record(
        &self,
        kind: DiagnosticKind,
        source: &str,
        detail: impl Into<String>,
    ) {
        let mut state = self.state.lock();
        state.diagnostics.push(Diagnostic {
            kind,
            source: source.to_string(),
            detail: detail.into(),
        });
        let overflow = state.diagnostics.len().saturating_sub(DIAGNOSTIC_LIMIT);
        state.diagnostics.drain(..overflow);
    }

    // Withdraw one registration and publish the new tables. Chain
    // middleware handles live in `chain_regs`; dropping them removes
    // the middleware from its chain.
    fn withdraw(&self, kind: RegistrationKind, id: u64) {
        {
            let handles = self.chain_regs.lock().remove(&id);
            drop(handles);
            self.chain_only.lock().remove(&id);
            let mut state = self.state.lock();
            match kind {
                RegistrationKind::Hook => retain_id(&mut state.hooks, id),
                RegistrationKind::Gate => {}
                RegistrationKind::Guard => retain_id(&mut state.guards, id),
                RegistrationKind::Hint => retain_id(&mut state.hints, id),
                RegistrationKind::Tracker => retain_id(&mut state.trackers, id),
                RegistrationKind::OutputTransformer => {
                    retain_id(&mut state.output_transformers, id);
                }
                RegistrationKind::RequestTransformer => {}
                RegistrationKind::ResultTransformer => {
                    retain_id(&mut state.result_transformers, id);
                }
                RegistrationKind::PromptSection => {
                    retain_id(&mut state.prompt_sections, id);
                }
                RegistrationKind::Tool => withdraw_tool(&mut state, id),
            }
        }
        self.publish_snapshot();
    }

    fn publish_snapshot(&self) {
        let state = self.state.lock();
        let snapshot = HostSnapshot::build(&state);
        drop(state);
        *self.snapshot.write() = Arc::new(snapshot);
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

// Withdraw the registration with `id`.
fn retain_id<T>(entries: &mut Vec<Entry<T>>, id: u64) {
    entries.retain(|entry| entry.id != id);
}

// Withdraw one tool registration: the host forgets it, and the
// registry forgets it too — a withdrawn tool must not stay callable.
fn withdraw_tool(state: &mut HostState, id: u64) {
    let Some(index) = state.tools.iter().position(|row| row.id == id) else {
        return;
    };
    let row = state.tools.remove(index);
    let Some(registry) =
        state.registry.as_ref().and_then(|weak| weak.upgrade())
    else {
        return;
    };
    withdraw_token(&registry, row.token);
}

// Take the entry filed under `token` out of the registry, if the
// registration ever reached it.
fn withdraw_token(registry: &ToolRegistry, token: Option<RegistrationToken>) {
    if let Some(token) = token {
        registry.unregister_by_token(token);
    }
}

// Run one hint's trigger + generator under a panic guard.
fn triggered_hint(
    hint: &dyn Hint,
    state: &AgentState,
) -> Result<Option<uva_steering::HintMessage>, ()> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        if hint.should_trigger(state) {
            Some(hint.generate(state))
        } else {
            None
        }
    }))
    .map_err(|_| ())
}

// -- seam-chain adapters -------------------------------------------------
//
// The typed registrations the seam chains dispatch. Each layer
// reconstructs the `HookContext` from the pass payload (a state
// snapshot plus the shareable sink and cancel handles), applies the
// same arbitration the table dispatch had — accept-filtering, panic
// isolation, fail-closed gates — and maps the decision onto the
// chain vocabulary: `Ask` is recorded on the pass and the chain
// continues, a deciding ruling short-circuits it.

use std::panic::AssertUnwindSafe as HostAdapterUnwindSafe;

type ChainCtx = Arc<uva_kernel::WaterfallCtx>;
type ChainNext = uva_kernel::Next;
type ChainResult = Result<uva_kernel::Outcome, uva_kernel::WaterfallError>;

fn hook_adapter(
    host: std::sync::Weak<HostInner>,
    id: u64,
    name: String,
    hook: Arc<dyn Hook>,
) -> uva_kernel::Middleware {
    Arc::new(move |ctx, next| {
        Box::pin(hook_layer(
            std::sync::Weak::clone(&host),
            id,
            name.clone(),
            Arc::clone(&hook),
            ctx,
            Arc::clone(&next),
        ))
    })
}

async fn hook_layer(
    host: std::sync::Weak<HostInner>,
    id: u64,
    name: String,
    hook: Arc<dyn Hook>,
    ctx: ChainCtx,
    next: ChainNext,
) -> ChainResult {
    let Some(pass) = HookPayload::payload(&ctx) else {
        return next(ctx).await;
    };
    let event = pass.event().clone();
    let observed =
        HostAdapterUnwindSafe(hook.observe(&event, &pass.hook_context()))
            .catch_unwind()
            .await;
    let decision = match observed {
        Ok(decision) => decision,
        Err(_) => {
            return panicked(
                &host,
                RegistrationKind::Hook,
                id,
                &name,
                ctx,
                &next,
                pass.as_ref(),
            )
            .await;
        }
    };
    let (decision, fact) = accept(&event, decision, &name);
    if let Some(fact) = fact {
        pass.record_fact(fact);
    }
    let veto = veto_row(&name, &decision, &event);
    apply_decision(ctx, next, pass, decision, veto).await
}

fn gate_adapter(name: String, gate: Arc<dyn Gate>) -> uva_kernel::Middleware {
    Arc::new(move |ctx, next| {
        Box::pin(gate_layer(
            name.clone(),
            Arc::clone(&gate),
            ctx,
            Arc::clone(&next),
        ))
    })
}

async fn gate_layer(
    name: String,
    gate: Arc<dyn Gate>,
    ctx: ChainCtx,
    next: ChainNext,
) -> ChainResult {
    let Some(pass) = HookPayload::payload(&ctx) else {
        return next(ctx).await;
    };
    let event = pass.event().clone();
    let checked =
        HostAdapterUnwindSafe(gate.check(&event, &pass.hook_context()))
            .catch_unwind()
            .await;
    let outcome = match checked {
        Ok(Ok(decision)) => Ok(decision),
        Ok(Err(reason)) => Err(format!("the gate failed: {reason}")),
        Err(_) => Err("the gate panicked".to_owned()),
    };
    let (decision, fact) = match outcome {
        Ok(decision) => accept(&event, decision, &name),
        // The gate could not answer: the pass fails closed where the
        // point can refuse, and the failure is reported where it
        // cannot.
        Err(why) => match refusal(&event, &name, &why) {
            Refusal::Decided(decision) => (decision, None),
            Refusal::Reported(fact) => (HookDecision::Allow, Some(fact)),
        },
    };
    if let Some(fact) = fact {
        pass.record_fact(fact);
    }
    let veto = veto_row(&name, &decision, &event);
    apply_decision(ctx, next, pass, decision, veto).await
}

fn request_transformer_adapter(
    host: std::sync::Weak<HostInner>,
    id: u64,
    name: String,
    transformer: Arc<dyn RequestTransformer>,
) -> uva_kernel::Middleware {
    Arc::new(move |ctx, next| {
        Box::pin(request_transformer_layer(
            std::sync::Weak::clone(&host),
            id,
            name.clone(),
            Arc::clone(&transformer),
            ctx,
            Arc::clone(&next),
        ))
    })
}

async fn request_transformer_layer(
    host: std::sync::Weak<HostInner>,
    id: u64,
    name: String,
    transformer: Arc<dyn RequestTransformer>,
    ctx: ChainCtx,
    next: ChainNext,
) -> ChainResult {
    let Some(pass) = HookPayload::payload(&ctx) else {
        return next(ctx).await;
    };
    let HookEvent::BeforeRequest { provider, params } = pass.event() else {
        return next(ctx).await;
    };
    let mut params = params.clone();
    let transformed = HostAdapterUnwindSafe(
        transformer.transform(&mut params, &pass.hook_context()),
    )
    .catch_unwind()
    .await;
    match transformed {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            if let Some(host) = host.upgrade() {
                host.record(DiagnosticKind::TransformerFailed, &name, reason);
            }
            return next(ctx).await;
        }
        Err(_) => {
            return panicked(
                &host,
                RegistrationKind::RequestTransformer,
                id,
                &name,
                ctx,
                &next,
                pass.as_ref(),
            )
            .await;
        }
    }
    let rewritten = HookPayload::rewrite(
        &ctx,
        HookEvent::BeforeRequest {
            provider: provider.clone(),
            params,
        },
    );
    next(rewritten).await
}

// Apply one accepted decision to the chain: `Allow` continues,
// `Ask` is recorded and continues, a deciding ruling short-circuits,
// and `ReplaceParams` rewrites the request parameters in place.
// A deciding ruling travels as a `Decided` value — the ruling plus
// the veto row it owes — so the seam layer can record the veto
// where the run's sink lives.
async fn apply_decision(
    ctx: ChainCtx,
    next: ChainNext,
    pass: Arc<HookPayload>,
    decision: HookDecision,
    veto: Option<VetoRow>,
) -> ChainResult {
    match decision {
        HookDecision::Allow => next(ctx).await,
        HookDecision::Ask { reason } => {
            pass.ask(reason);
            next(ctx).await
        }
        HookDecision::ReplaceParams(params) => {
            if let HookEvent::BeforeRequest { provider, .. } = pass.event() {
                let rewritten = HookPayload::rewrite(
                    &ctx,
                    HookEvent::BeforeRequest {
                        provider: provider.clone(),
                        params,
                    },
                );
                return next(rewritten).await;
            }
            next(ctx).await
        }
        deciding => Ok(uva_kernel::Outcome::ShortCircuit(
            uva_kernel::EventValue::new(crate::seams::Decided {
                ruling: crate::seams::SeamRuling::from(deciding),
                veto,
            }),
        )),
    }
}

// A registration panicked: withdraw it, record the diagnostic, and
// let the chain continue without it.
async fn panicked(
    host: &std::sync::Weak<HostInner>,
    kind: RegistrationKind,
    id: u64,
    name: &str,
    ctx: ChainCtx,
    next: &ChainNext,
    pass: &HookPayload,
) -> ChainResult {
    if let Some(host) = host.upgrade() {
        host.withdraw(kind, id);
    }
    pass.record_fact(PANIC_POLICY.fact(name, "the registration panicked"));
    next(ctx).await
}
