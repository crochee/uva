//! The revocable registry and the arbitration every pass runs under.
//!
//! A third-party plugin registers on one of the twelve mount points
//! ([`MountPoint`]) and gets back a [`HookRegistration`]: dropping
//! the handle withdraws the registration from every pass that shares
//! the registry. The registry is cheap to clone — every clone reads
//! the same table — so a deployment installs one and hands it to the
//! loop. Prompt sections are registered on the same table but are a
//! collection, not an event: [`HookRegistry::prompt_sections`] returns
//! them in table order and no synthetic [`HookEvent`] is manufactured
//! for that point.
//!
//! # Ordering
//!
//! Every registration carries a [`RegistrationOrigin`]. At one mount
//! point the plugin lane runs before the in-process lane, and inside a
//! lane the registration order decides. The lane is a field on the
//! registration, not a second dispatch path.
//!
//! # Arbitration (dsh monotonic rejection)
//!
//! Every dispatch folds its registrations in registration order
//! through [`Arbitration::absorb`]:
//!
//! - the first `Block` short-circuits the pass — later registrations
//!   do not run;
//! - every `Ask` is collected and the pass continues;
//! - a `Terminate` is accepted only where
//!   [`HookEvent::accepts`](uva_steering::HookEvent::accepts) says so
//!   (the same five-point table the harness uses), and otherwise
//!   ignored;
//! - a gate that cannot answer stands apart from all of the above: an
//!   `Err` or a panic ends the pass with the point's own denial —
//!   `Block` before a tool call, `Terminate` at the other points that
//!   can end a run. A gate failure at `llm/request` or `llm/result`,
//!   where the table accepts no denial, is reported as a dispatch
//!   failure and logged at `error`; it is never recorded as "no
//!   opinion";
//! - the fold is deny-only and monotonic: once a decision is a
//!   `Block` or `Terminate`, no later registration can turn it back
//!   into permission, because there is no allow outcome to absorb.
//!
//! A registration that panics follows its origin: the plugin lane is
//! skipped and kept (a plugin may fail transiently and is not told it
//! was disabled), the in-process lane is withdrawn and the fact says
//! so (a broken in-process policy would panic again). A gate that
//! panics is still a gate that could not answer — the pass fails
//! closed where the point can refuse, and the withdrawal applies on
//! top of that.
//!
//! This table is the only interception registration surface (ADR
//! 0028): the harness's `Seams` dispatches it and folds the result
//! through this [`Arbitration`]; the `Host`'s typed tables (guards,
//! hints, trackers, output transformers) sit beside it, not inside
//! it. A `Block` at `tool/pre-execute` also makes the harness write
//! the durable `hook_block` row, naming the registration that vetoed
//! the call.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        Mutex,
        MutexGuard,
        PoisonError,
        Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::FutureExt;
use uva_steering::{HookDecision, HookEvent};
use uva_tool::ToolOutput;

use crate::{
    PromptSection,
    arbitration::{Arbitration, Fact, Refusal, Ruling, refusal, rule},
    context::HookContext,
    mount::{MountPoint, RegistrationOrigin},
    traits::{Gate, Hook, RequestTransformer, ResultTransformer},
};

/// What went wrong with one registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// A registration named a mount point it cannot carry.
    ///
    /// The key is the point's wire spelling: a request transformer
    /// belongs only on `llm/request`, a result transformer only on
    /// `tool/post-execute`, and a prompt section only on
    /// `prompt/sections`.
    WrongMountPoint {
        /// The key the caller passed.
        key: &'static str,
        /// The only key that carries this registration kind.
        expected: &'static str,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongMountPoint { key, expected } => write!(
                f,
                "`{key}` cannot dispatch this registration; it belongs \
                 on `{expected}`"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

/// What one registry dispatch produced.
pub struct RegistryPass {
    /// The arbitration the registry's registrations produced.
    pub arbitration: Arbitration,
    /// The registration that ended the pass, when it was decided.
    pub denied_by: Option<String>,
    /// The effective event the pass hands on — `None` when the pass
    /// was decided.
    pub event: Option<HookEvent>,
    /// What the pass learned about the registrations that ran.
    ///
    /// A gate that could not answer at a point that accepts no denial
    /// could not fail closed, so the failure is reported here — never
    /// folded into `Allow`. A decision the point did not accept and a
    /// registration that panicked are reported the same way: the pass
    /// continued, and the operator should know why.
    pub facts: Vec<Fact>,
}

/// The shared interception table, keyed by mount point.
///
/// Clone is cheap and shares one table: every clone sees every
/// registration, and a [`HookRegistration`] handle dropped anywhere
/// withdraws its row everywhere. One table carries the four
/// interception traits and the prompt-section collection, so an
/// observation and a pass cannot drift into two stores.
#[derive(Clone, Default)]
pub struct HookRegistry {
    inner: Arc<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    entries: Mutex<HashMap<MountPoint, Vec<Entry>>>,
    next_id: AtomicU64,
}

#[derive(Clone)]
struct Entry {
    id: u64,
    name: String,
    origin: RegistrationOrigin,
    kind: Registered,
}

#[derive(Clone)]
enum Registered {
    Hook(Arc<dyn Hook>),
    Gate(Arc<dyn Gate>),
    Request(Arc<dyn RequestTransformer>),
    Result(Arc<dyn ResultTransformer>),
    Section(PromptSection),
}

/// One registration in a [`HookRegistry`].
///
/// Dropping the registration withdraws it: every later pass stops
/// calling it. A clone shares the withdrawal — the registration is
/// gone when the last handle is.
#[derive(Clone)]
pub struct HookRegistration {
    inner: Arc<RegistrationInner>,
}

struct RegistrationInner {
    mount: MountPoint,
    id: u64,
    registry: Weak<RegistryInner>,
}

impl std::fmt::Debug for HookRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRegistration")
            .field("mount", &self.inner.mount)
            .field("id", &self.inner.id)
            .finish()
    }
}

impl Drop for RegistrationInner {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut entries = lock(&registry.entries);
        let Some(list) = entries.get_mut(&self.mount) else {
            return;
        };
        list.retain(|entry| entry.id != self.id);
        if list.is_empty() {
            entries.remove(&self.mount);
        }
    }
}

impl HookRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an observer at `mount`.
    ///
    /// An observer may run at any mount point. A registration on the
    /// prompt-section collection is recorded but never dispatched:
    /// that point is a collection, not an event.
    pub fn register_hook_at(
        &self,
        mount: MountPoint,
        hook: Arc<dyn Hook>,
        origin: RegistrationOrigin,
    ) -> HookRegistration {
        self.add(
            mount,
            hook.name().to_owned(),
            origin,
            Registered::Hook(hook),
        )
    }

    /// Register a gate at `mount`.
    ///
    /// A gate fails closed: an `Err` or a panic from
    /// [`Gate::check`] ends the pass with the decision the mount
    /// point accepts — [`HookDecision::Block`] at `tool/pre-execute`
    /// (the model reads the refusal as the tool result),
    /// [`HookDecision::Terminate`] at the other points that can end a
    /// run. `llm/request` and `llm/result` accept no denial at all, so
    /// a failure there is reported in [`RegistryPass::facts`]
    /// instead — the harness writes a `hook` warning — and is never
    /// folded into `Allow`.
    pub fn register_gate_at(
        &self,
        mount: MountPoint,
        gate: Arc<dyn Gate>,
        origin: RegistrationOrigin,
    ) -> HookRegistration {
        self.add(
            mount,
            gate.name().to_owned(),
            origin,
            Registered::Gate(gate),
        )
    }

    /// Register a request transformer at `mount`.
    ///
    /// `mount` must be [`MountPoint::LlmRequest`]: the rewrite
    /// surface exists only on a `BeforeRequest` pass.
    ///
    /// # Errors
    ///
    /// [`RegistryError::WrongMountPoint`] for any other mount point.
    pub fn register_request_transformer_at(
        &self,
        mount: MountPoint,
        transformer: Arc<dyn RequestTransformer>,
        origin: RegistrationOrigin,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate_for_at(mount, MountPoint::LlmRequest)?;
        let name = transformer.name().to_owned();
        Ok(self.add(mount, name, origin, Registered::Request(transformer)))
    }

    /// Register a result transformer at `mount`.
    ///
    /// `mount` must be [`MountPoint::ToolPost`]: the rewrite surface
    /// exists only on the tool-result seam.
    ///
    /// # Errors
    ///
    /// [`RegistryError::WrongMountPoint`] for any other mount point.
    pub fn register_result_transformer_at(
        &self,
        mount: MountPoint,
        transformer: Arc<dyn ResultTransformer>,
        origin: RegistrationOrigin,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate_for_at(mount, MountPoint::ToolPost)?;
        let name = transformer.name().to_owned();
        Ok(self.add(mount, name, origin, Registered::Result(transformer)))
    }

    /// Register a system-prompt section.
    ///
    /// `mount` must be [`MountPoint::PromptSections`]. A section is a
    /// collection, not an event: [`prompt_sections`](Self::prompt_sections)
    /// returns it in table order, and
    /// [`dispatch_at`](Self::dispatch_at) never manufactures an event
    /// for this point.
    ///
    /// # Errors
    ///
    /// [`RegistryError::WrongMountPoint`] for any other mount point.
    pub fn register_prompt_section(
        &self,
        mount: MountPoint,
        section: PromptSection,
        origin: RegistrationOrigin,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate_for_at(mount, MountPoint::PromptSections)?;
        let name = section.title.clone();
        Ok(self.add(mount, name, origin, Registered::Section(section)))
    }

    /// Whether `mount` has at least one registration.
    #[must_use]
    pub fn has_entries_at(&self, mount: MountPoint) -> bool {
        lock(&self.inner.entries)
            .get(&mount)
            .is_some_and(|list| !list.is_empty())
    }

    /// Whether any result transformer is registered.
    ///
    /// The entry kind decides, not the mount point: a [`Hook`] or
    /// [`Gate`] at [`MountPoint::ToolPost`] observes the `AfterTool`
    /// event and rewrites nothing, so it does not make the loop take
    /// its rewrite path.
    #[must_use]
    pub fn has_result_transformers(&self) -> bool {
        lock(&self.inner.entries)
            .get(&MountPoint::ToolPost)
            .is_some_and(|list| {
                list.iter()
                    .any(|entry| matches!(&entry.kind, Registered::Result(_)))
            })
    }

    /// Whether the registry holds no registration at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        lock(&self.inner.entries)
            .values()
            .all(|list| list.is_empty())
    }

    /// Run the registrations at `mount` over `event`.
    ///
    /// One entry point serves the six pass points and the five
    /// observation points. Hooks and gates fold into the pass
    /// arbitration; request transformers rewrite the effective event
    /// in place. At a point the plugin lane runs before the
    /// in-process lane, registration order inside a lane; a deciding
    /// registration short-circuits the pass — later registrations do
    /// not run.
    ///
    /// [`MountPoint::PromptSections`] is a collection, not an event:
    /// the pass is returned unchanged and no synthetic event is
    /// manufactured for it. Read that point with
    /// [`prompt_sections`](Self::prompt_sections).
    pub async fn dispatch_at(
        &self,
        mount: MountPoint,
        event: &HookEvent,
        ctx: &HookContext<'_>,
    ) -> RegistryPass {
        if !mount.is_event() {
            return RegistryPass {
                arbitration: Arbitration::default(),
                denied_by: None,
                event: Some(event.clone()),
                facts: Vec::new(),
            };
        }
        let mut effective = event.clone();
        let mut arbitration = Arbitration::default();
        let mut denied_by = None;
        let mut facts = Vec::new();
        for entry in self.entries_for(mount) {
            if arbitration.decided() {
                break;
            }
            match entry.kind {
                Registered::Hook(hook) => {
                    let observed = std::panic::AssertUnwindSafe(
                        hook.observe(&effective, ctx),
                    )
                    .catch_unwind()
                    .await;
                    let decision = match observed {
                        Ok(decision) => decision,
                        Err(_) => {
                            let policy = entry.origin.panic_policy();
                            tracing::warn!(
                                hook = %entry.name,
                                consequence = policy.consequence(),
                                "hook registration panicked"
                            );
                            if entry.origin == RegistrationOrigin::InProcess {
                                self.withdraw(mount, entry.id);
                            }
                            facts.push(
                                policy.fact(&entry.name, "the call panicked"),
                            );
                            continue;
                        }
                    };
                    let Some(decision) =
                        ruled(&effective, decision, &entry.name, &mut facts)
                    else {
                        continue;
                    };
                    apply_decision(
                        &mut arbitration,
                        &mut effective,
                        &mut denied_by,
                        entry.name,
                        decision,
                    );
                }
                Registered::Gate(gate) => {
                    let checked = std::panic::AssertUnwindSafe(
                        gate.check(&effective, ctx),
                    )
                    .catch_unwind()
                    .await;
                    let outcome = match checked {
                        Ok(Ok(decision)) => GateOutcome::Decided(decision),
                        Ok(Err(reason)) => GateOutcome::Failed(format!(
                            "the gate failed: {reason}"
                        )),
                        Err(_) => {
                            let policy = entry.origin.panic_policy();
                            tracing::warn!(
                                gate = %entry.name,
                                consequence = policy.consequence(),
                                "gate registration panicked"
                            );
                            if entry.origin == RegistrationOrigin::InProcess {
                                self.withdraw(mount, entry.id);
                            }
                            facts.push(
                                policy.fact(&entry.name, "the gate panicked"),
                            );
                            GateOutcome::Failed("the gate panicked".to_owned())
                        }
                    };
                    match outcome {
                        GateOutcome::Decided(decision) => {
                            let Some(decision) = ruled(
                                &effective,
                                decision,
                                &entry.name,
                                &mut facts,
                            ) else {
                                continue;
                            };
                            apply_decision(
                                &mut arbitration,
                                &mut effective,
                                &mut denied_by,
                                entry.name,
                                decision,
                            );
                        }
                        GateOutcome::Failed(why) => {
                            tracing::error!(
                                gate = %entry.name,
                                event = %uva_steering::event_name(&effective),
                                %why,
                                "gate could not answer"
                            );
                            match refusal(&effective, &entry.name, &why) {
                                Refusal::Decided(decision) => apply_decision(
                                    &mut arbitration,
                                    &mut effective,
                                    &mut denied_by,
                                    entry.name,
                                    decision,
                                ),
                                Refusal::Reported(fact) => {
                                    facts.push(fact);
                                    continue;
                                }
                            }
                        }
                    }
                }
                Registered::Request(transformer) => {
                    let HookEvent::BeforeRequest { provider, params } =
                        &effective
                    else {
                        continue;
                    };
                    let mut params = params.clone();
                    let transformed = std::panic::AssertUnwindSafe(
                        transformer.transform(&mut params, ctx),
                    )
                    .catch_unwind()
                    .await;
                    match transformed {
                        Ok(Ok(())) => {
                            effective = HookEvent::BeforeRequest {
                                provider: provider.clone(),
                                params,
                            };
                        }
                        Ok(Err(reason)) => tracing::warn!(
                            transformer = %entry.name,
                            %reason,
                            "request transformer failed; skipped"
                        ),
                        Err(_) => {
                            let policy = entry.origin.panic_policy();
                            tracing::warn!(
                                transformer = %entry.name,
                                consequence = policy.consequence(),
                                "request transformer panicked"
                            );
                            if entry.origin == RegistrationOrigin::InProcess {
                                self.withdraw(mount, entry.id);
                            }
                            facts.push(
                                policy.fact(&entry.name, "the call panicked"),
                            );
                        }
                    }
                }
                Registered::Result(_) | Registered::Section(_) => {}
            }
        }
        if arbitration.decided() {
            RegistryPass {
                arbitration,
                denied_by,
                event: None,
                facts,
            }
        } else {
            RegistryPass {
                arbitration,
                denied_by: None,
                event: Some(effective),
                facts,
            }
        }
    }

    /// Run the result transformers over `output`.
    ///
    /// Returns the names that ran, in table order, when the aggregate
    /// output changed; an unchanged output reports no names, so the
    /// loop writes no provenance row for a no-op. The early return
    /// reads [`has_result_transformers`](Self::has_result_transformers),
    /// so a `Hook` or `Gate` at the point does not make this pass copy
    /// the output. A panicking transformer follows its origin: a
    /// plugin is kept, an in-process registration is withdrawn.
    pub async fn transform_result(
        &self,
        output: &mut ToolOutput,
        ctx: &HookContext<'_>,
    ) -> Vec<String> {
        if !self.has_result_transformers() {
            return Vec::new();
        }
        let before = output.clone();
        let mut ran = Vec::new();
        for entry in self.entries_for(MountPoint::ToolPost) {
            let Entry {
                id,
                name,
                origin,
                kind: Registered::Result(transformer),
            } = entry
            else {
                continue;
            };
            match std::panic::AssertUnwindSafe(
                transformer.transform(output, ctx),
            )
            .catch_unwind()
            .await
            {
                Ok(Ok(())) => ran.push(name),
                Ok(Err(reason)) => {
                    tracing::warn!(
                        transformer = %name,
                        %reason,
                        "result transformer failed"
                    );
                    ran.push(name);
                }
                Err(_) => {
                    let policy = origin.panic_policy();
                    tracing::warn!(
                        transformer = %name,
                        consequence = policy.consequence(),
                        "result transformer panicked"
                    );
                    if origin == RegistrationOrigin::InProcess {
                        self.withdraw(MountPoint::ToolPost, id);
                    }
                }
            }
        }
        if *output == before {
            return Vec::new();
        }
        ran
    }

    /// The prompt sections, in table order.
    ///
    /// The collection follows the table's order — the plugin lane
    /// before the in-process lane, registration order inside a lane —
    /// and a dropped [`HookRegistration`] withdraws its section like
    /// any other row. This is the collection the prompt assembly
    /// reads; no [`HookEvent`] is manufactured for it.
    #[must_use]
    pub fn prompt_sections(&self) -> Vec<PromptSection> {
        self.entries_for(MountPoint::PromptSections)
            .into_iter()
            .filter_map(|entry| match entry.kind {
                Registered::Section(section) => Some(section),
                _ => None,
            })
            .collect()
    }

    fn validate_for_at(
        &self,
        mount: MountPoint,
        expected: MountPoint,
    ) -> Result<(), RegistryError> {
        if mount == expected {
            Ok(())
        } else {
            Err(RegistryError::WrongMountPoint {
                key: mount.as_str(),
                expected: expected.as_str(),
            })
        }
    }

    fn add(
        &self,
        mount: MountPoint,
        name: String,
        origin: RegistrationOrigin,
        kind: Registered,
    ) -> HookRegistration {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.entries)
            .entry(mount)
            .or_default()
            .push(Entry {
                id,
                name,
                origin,
                kind,
            });
        HookRegistration {
            inner: Arc::new(RegistrationInner {
                mount,
                id,
                registry: Arc::downgrade(&self.inner),
            }),
        }
    }

    fn withdraw(&self, mount: MountPoint, id: u64) {
        let mut entries = lock(&self.inner.entries);
        let Some(list) = entries.get_mut(&mount) else {
            return;
        };
        list.retain(|entry| entry.id != id);
        if list.is_empty() {
            entries.remove(&mount);
        }
    }

    /// The registrations at `mount`, in table order.
    ///
    /// The plugin lane precedes the in-process lane; ids increase in
    /// registration order, so `(lane, id)` is the whole ordering
    /// rule.
    fn entries_for(&self, mount: MountPoint) -> Vec<Entry> {
        let mut entries = lock(&self.inner.entries)
            .get(&mount)
            .cloned()
            .unwrap_or_default();
        entries.sort_by_key(|entry| (entry.origin.lane(), entry.id));
        entries
    }
}

// Fold one accepted decision into the pass.
//
// `ReplaceParams` rewrites the effective event so later registrations
// — and the harness's own typed registrations — see what the earlier
// one left. A denial records its source for the `hook_block` row.
/// What one registration did at one mount point.
enum GateOutcome {
    /// The gate answered.
    Decided(HookDecision),
    /// The gate could not answer: this is why.
    Failed(String),
}

/// Rule one registration's decision at the point.
///
/// `None` when the point does not accept the decision: the fact is
/// recorded and the pass continues — the decision does not apply.
fn ruled(
    event: &HookEvent,
    decision: HookDecision,
    name: &str,
    facts: &mut Vec<Fact>,
) -> Option<HookDecision> {
    match rule(event, decision, name) {
        Ruling::Stands(decision) => Some(decision),
        Ruling::Unsupported { fact, .. } => {
            tracing::debug!(
                registration = %name,
                event = %uva_steering::event_name(event),
                "decision not accepted here; ignored"
            );
            facts.push(fact);
            None
        }
    }
}

fn apply_decision(
    arbitration: &mut Arbitration,
    event: &mut HookEvent,
    denied_by: &mut Option<String>,
    name: String,
    decision: HookDecision,
) {
    if let (
        HookEvent::BeforeRequest { provider, .. },
        HookDecision::ReplaceParams(params),
    ) = (&*event, &decision)
    {
        *event = HookEvent::BeforeRequest {
            provider: provider.clone(),
            params: params.clone(),
        };
    }
    if matches!(
        decision,
        HookDecision::Block { .. } | HookDecision::Terminate { .. }
    ) {
        *denied_by = Some(name);
    }
    arbitration.absorb(decision);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
