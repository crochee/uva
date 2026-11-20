//! The host: the typed policy tables the loop reads, and the one
//! interception registry it carries.
//!
//! A host belongs to one agent — `ReActAgent::with_host` installs it
//! — so two agents never share one policy. Every registration
//! returns a [`Registration`](crate::agent::host::Registration):
//! dropping it withdraws the
//! registration, so a plugin can be uninstalled without rebuilding
//! the agent.
//!
//! ## What the host holds
//!
//! The host is the composition point for the typed policies the
//! [`HookRegistry`](uva_hook::HookRegistry) cannot express, plus the
//! registry itself:
//!
//! | Kind | Home |
//! |---|---|
//! | [`Hook`](crate::agent::host::Hook), [`Gate`](crate::agent::host::Gate), [`RequestTransformer`](crate::agent::host::RequestTransformer), [`ResultTransformer`](crate::agent::host::ResultTransformer) | the [`HookRegistry`](uva_hook::HookRegistry) this host carries — one table for the six passes and the five observation points |
//! | [`Guard`](uva_steering::Guard) | the guard pipeline (`tool_guards`), a sibling of `tool/pre-execute` — data-plane, stays a table |
//! | [`Hint`](uva_steering::Hint) | a scoped read the run queries, not a pass — stays a table |
//! | [`Tracker`](uva_steering::tracker::Tracker) | a table object the loop reads at its boundaries — stays |
//! | [`OutputTransformer`](uva_steering::OutputTransformer) | a rewrite of the run's final text — stays a table |
//! | tool | the bound [`ToolRegistry`](uva_tool::ToolRegistry), via token |
//!
//! A registration's lifetime is the returned handle's, exactly like a
//! registry row's: the entry withdraws with the handle.
//!
//! The registry is part of the host, not a second surface beside it:
//! boot builds one instance,
//! [`Host::default_policy`](crate::agent::host::Host::default_policy) writes the
//! deployment policy into it, and the loop's `Seams` reads it from
//! here. Every host clone shares it, so a registration made after an
//! agent was built is visible to that agent's next pass.
//!
//! The remaining tables are published as one immutable
//! `HostSnapshot`; a registration change builds a new snapshot and
//! swaps it. A reading hot path therefore clones one `Arc` and reads
//! it without taking a lock.
//!
//! ## The arbitration rules
//!
//! The interception rules live with the table
//! ([`HookRegistry`](uva_hook::HookRegistry)): source lane then
//! registration order, the first `Block` short-circuits, `Ask`
//! reasons collect, and a panicking registration follows its origin.
//! The host's own rules stay what they were:
//!
//! - guards run in registration order and the first denial wins, the
//!   same pipeline `uva_steering::run_guards` defines;
//! - a hint that panics is disabled with a diagnostic.

use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Weak, atomic::AtomicU64},
};

use parking_lot::{Mutex, RwLock};
// The prompt-section vocabulary lives one layer down with the run
// input that carries it (`uva-agent`); the deep path
// `uva_harness::agent::host::PromptSection` and the crate-root facade
// both re-export it from there.
pub use uva_agent::PromptSection;
use uva_context::AgentState;
// The interception contract lives one layer down (ADR 0004): the
// traits, the dispatch context and the registry are `uva-hook`'s,
// re-exported here so neither the crate-root facade nor this deep
// path moves.
pub use uva_hook::{
    Arbitration,
    Gate,
    Hook,
    HookContext,
    RequestTransformer,
    ResultTransformer,
};
use uva_hook::{HookRegistration, HookRegistry};
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
use uva_tool::{RegistrationToken, Tool, ToolEntry, ToolRegistry};

use crate::seams::VetoRow;

#[cfg(test)]
mod tests;

// How many diagnostics the host keeps.
const DIAGNOSTIC_LIMIT: usize = 64;

// The veto one pass owes the `hook_block` row: a denial of a tool
// call, with everything the row names.
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

/// One registration in a host.
///
/// Dropping the registration withdraws it: the host stops calling
/// it. A clone shares the withdrawal — the registration is gone when
/// the last handle is.
#[derive(Clone)]
pub struct Registration {
    inner: Arc<RegistrationInner>,
}

struct RegistrationInner {
    id: u64,
    kind: RegistrationKind,
    host: Weak<HostInner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrationKind {
    Guard,
    Hint,
    Tracker,
    OutputTransformer,
    Tool,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("kind", &self.inner.kind)
            .field("id", &self.inner.id)
            .finish()
    }
}

impl Drop for RegistrationInner {
    fn drop(&mut self) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        host.withdraw(self.kind, self.id);
    }
}

/// The per-agent registry of typed policy registrations, over the one
/// interception table it carries.
///
/// # Deprecation direction
///
/// The kernel's effects — through `uva-kernel`'s `Context` and the
/// plugin crates' `Plugin` implementations — are the registration
/// target. `Host` remains while the chat path still builds each run
/// through the consumer's run assembly, which hands every agent a
/// host clone; removing it before that path moves to the `agentLoop`
/// service would break chat.
pub struct Host {
    inner: Arc<HostInner>,
}

impl Clone for Host {
    fn clone(&self) -> Self {
        // The clone shares the registrations and the interception
        // table — a host is one policy, not a copy of it.
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct HostInner {
    // The one interception table this deployment dispatches: the six
    // pass points, the five observation points and the prompt-section
    // collection.
    registry: HookRegistry,
    state: Mutex<HostState>,
    // The published dispatch tables. Rebuilt on every change.
    snapshot: RwLock<Arc<HostSnapshot>>,
    next_id: AtomicU64,
    // The handles [`Host::default_policy`] installed. They live as
    // long as the host does — a registration withdraws when its last
    // handle drops, so a default policy must not let its handles fall
    // out of scope.
    _defaults: Mutex<DefaultHandles>,
}

// The handles the default policy owns, one list per registration
// surface. Holding the handles is the whole job — a registration
// withdraws when its last handle drops — so nothing reads these lists.
#[derive(Default)]
struct DefaultHandles {
    // The typed policy rows (guards, hints, tracker).
    _tables: Vec<Registration>,
    // The `LoggingHook` at each event mount point.
    _registry: Vec<HookRegistration>,
}

#[derive(Default)]
struct HostState {
    guards: Vec<Entry<Arc<dyn Guard>>>,
    hints: Vec<Entry<Arc<dyn Hint>>>,
    trackers: Vec<Entry<Arc<dyn Tracker>>>,
    output_transformers: Vec<Entry<Arc<dyn OutputTransformer>>>,
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
    pub(crate) guards: Vec<Arc<dyn Guard>>,
    pub(crate) hints: Vec<Entry<Arc<dyn Hint>>>,
    pub(crate) trackers: Vec<Arc<dyn Tracker>>,
    pub(crate) output_transformers: Vec<Arc<dyn OutputTransformer>>,
}

impl HostSnapshot {
    fn build(state: &HostState) -> Self {
        Self {
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
    /// An empty host: no policy rows and an empty interception table.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_registry(HookRegistry::new())
    }

    /// An empty host over `registry`.
    ///
    /// The registry is the deployment's one table: a host built this
    /// way dispatches exactly the registrations it already holds, and
    /// every later registration on the same table is visible to this
    /// host's runs. An embedder with no deployment table calls
    /// [`Host::new`], which builds an empty one.
    #[must_use]
    pub fn new_with_registry(registry: HookRegistry) -> Self {
        Self {
            inner: Arc::new(HostInner {
                registry,
                state: Mutex::new(HostState::default()),
                snapshot: RwLock::new(Arc::new(HostSnapshot::default())),
                next_id: AtomicU64::new(1),
                _defaults: Mutex::new(DefaultHandles::default()),
            }),
        }
    }

    /// The one interception table this host dispatches.
    ///
    /// The loop's seam layer reads it from here (ADR 0028): the
    /// deployment builds one registry, this host carries it, and
    /// every pass of every run over this host reads the same rows.
    #[must_use]
    pub fn hook_registry(&self) -> &HookRegistry {
        &self.inner.registry
    }

    /// The production policy: defensive guards, budget / iteration /
    /// truncation hints, adaptive concurrency and the debug logging
    /// hook.
    ///
    /// A base-library agent starts with [`Host::new`] and no policy;
    /// a deployment installs this explicitly. The logging hook is an
    /// in-process registration on the same table `registry` names —
    /// eleven event mount points, all but the prompt-section
    /// collection — so a deployment's out-of-tree registrations and
    /// its policy dispatch through one surface. Tuned to never
    /// interfere with legitimate work: the loop detector only fires on
    /// the 3rd identical consecutive call, the tool budget (250)
    /// exceeds the theoretical maximum of a bounded run, and the shell
    /// list only vetoes machine-level unrecoverable commands.
    #[must_use]
    pub fn default_policy(
        workspace_root: impl Into<std::path::PathBuf>,
        registry: HookRegistry,
    ) -> Self {
        let mut host = Self::new_with_registry(registry);
        // The handles must outlive this call: a registration withdraws
        // its entry when dropped, so the host itself owns them.
        let tables = vec![
            host.guard(Arc::new(LoopDetectionGuard::new(3))),
            host.guard(Arc::new(ToolBudgetGuard::new(250))),
            host.guard(Arc::new(ShellDenyGuard::new())),
            host.guard(Arc::new(WorkspaceBoundaryGuard::new(workspace_root))),
            host.guard(Arc::new(PromptInjectionGuard::new())),
            host.hint(Arc::new(ContextBudgetHint::new(0.85))),
            host.hint(Arc::new(IterationReminderHint::new(10))),
            host.hint(Arc::new(TruncationHint)),
            host.tracker(Arc::new(AdaptiveTracker::new(8))),
        ];
        // The policy's observer runs in the in-process lane at every
        // event point: the same eleven points the loop dispatches.
        let logging: Arc<dyn Hook> = Arc::new(uva_steering::LoggingHook);
        let registry_handles = uva_hook::MountPoint::ALL
            .into_iter()
            .filter(|mount| mount.is_event())
            .map(|mount| {
                host.inner.registry.register_hook_at(
                    mount,
                    Arc::clone(&logging),
                    uva_hook::RegistrationOrigin::InProcess,
                )
            })
            .collect();
        *host.inner._defaults.lock() = DefaultHandles {
            _tables: tables,
            _registry: registry_handles,
        };
        host
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

    /// The registered tools, in registration order.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let state = self.inner.state.lock();
        state
            .tools
            .iter()
            .map(|row| Arc::clone(&row.tool))
            .collect()
    }

    /// How many typed policy rows the host holds.
    ///
    /// The interception table has its own query
    /// ([`HookRegistry::has_entries_at`](uva_hook::HookRegistry::has_entries_at));
    /// this count is the host's own tables, so a deployment can assert
    /// what [`Host::default_policy`] installed.
    #[must_use]
    pub fn registration_count(&self) -> usize {
        let state = self.inner.state.lock();
        state.guards.len()
            + state.hints.len()
            + state.trackers.len()
            + state.output_transformers.len()
            + state.tools.len()
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
        Registration {
            inner: Arc::new(RegistrationInner {
                id,
                kind,
                host: Arc::downgrade(&self.inner),
            }),
        }
    }

    fn register<T>(
        &mut self,
        kind: RegistrationKind,
        name: String,
        value: T,
        push: impl FnOnce(&mut HostState, Entry<T>),
    ) -> Registration {
        let id = self.next_id();
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

    // Withdraw a registration that broke, and say so.
    //
    // The host's panic policy for its own tables: an in-process
    // registration that panics is broken, so it stops being called.
    // The interception table's policy lives on the registration
    // origin (ADR 0020); this path covers the tables the registry does
    // not carry.
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
    // Withdraw one registration and publish the new tables.
    fn withdraw(&self, kind: RegistrationKind, id: u64) {
        {
            let mut state = self.state.lock();
            match kind {
                RegistrationKind::Guard => retain_id(&mut state.guards, id),
                RegistrationKind::Hint => retain_id(&mut state.hints, id),
                RegistrationKind::Tracker => retain_id(&mut state.trackers, id),
                RegistrationKind::OutputTransformer => {
                    retain_id(&mut state.output_transformers, id);
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
