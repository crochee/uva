//! The revocable registry and the arbitration every pass runs under.
//!
//! A third-party plugin registers on one of the seven mount points
//! ([`crate::seam`]) and gets back a [`HookRegistration`]: dropping
//! the handle withdraws the registration from every pass that shares
//! the registry. The registry is cheap to clone — every clone reads
//! the same table — so a deployment installs one and hands it to the
//! loop.
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
//! The harness runs the registry's registrations *before* its own
//! typed ones and folds both results through the same [`Arbitration`],
//! so the two surfaces cannot disagree about a pass. A `Block` at
//! `tool/pre-execute` also makes the harness write the durable
//! `hook_block` row, naming the registration that vetoed the call.

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
    context::HookContext,
    seam::{
        LLM_REQUEST,
        LLM_RESULT,
        PROMPT_SECTIONS,
        STEP_POST,
        STEP_PRE,
        TOOL_POST,
        TOOL_PRE,
    },
    traits::{Gate, Hook, RequestTransformer, ResultTransformer},
};

/// The seven mount points a registration may name.
///
/// The six event-driven keys dispatch on their seam pass. The seventh,
/// `prompt/sections`, is the collection key: the loop's prompt
/// assembly reads the host's section table, so a registration there is
/// recorded but never runs a pass. Registrations validate against all
/// seven so the key vocabulary is one list (ADR 0004).
const MOUNT_POINTS: [&str; 7] = [
    STEP_PRE,
    STEP_POST,
    TOOL_PRE,
    TOOL_POST,
    LLM_REQUEST,
    LLM_RESULT,
    PROMPT_SECTIONS,
];

/// Whether `key` is one of the seven mount points.
#[must_use]
pub fn is_mount_point(key: &str) -> bool {
    MOUNT_POINTS.contains(&key)
}

/// What one dispatch pass decided at one mount point.
///
/// The `decision` is the one that won the pass: `Allow` when nothing
/// objected, `Block` / `Terminate` when a registration ended the
/// pass, `ReplaceParams` when a request was rewritten. The `asks` are
/// the approval reasons the registrations collected on the way — they
/// never end a pass.
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
    /// The fold is deny-only and monotonic — the terminal rule from
    /// dsh (`E004`, `E048`): an `Ask` joins the collected reasons, a
    /// `ReplaceParams` schedules a rewrite while the pass is still
    /// allowed, and a `Block` / `Terminate` is final. Once a denial is
    /// recorded, no later decision can clear it: there is no allow
    /// result to absorb, so registration order cannot turn a denial
    /// back into permission.
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

    /// Whether a decision ended the pass: a block or a termination.
    #[must_use]
    pub fn decided(&self) -> bool {
        matches!(
            self.decision,
            HookDecision::Block { .. } | HookDecision::Terminate { .. }
        )
    }
}

/// What went wrong with one registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The key is not one of the seven mount points.
    UnknownMountPoint(String),
    /// A transformer named a mount point it cannot dispatch on.
    WrongMountPoint {
        /// The key the caller passed.
        key: &'static str,
        /// The only key that dispatches this registration kind.
        expected: &'static str,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMountPoint(key) => {
                write!(f, "`{key}` is not a mount-point key")
            }
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
    /// Gates that could not answer at a point that accepts no denial.
    ///
    /// The four points that can refuse a pass get a synthesized
    /// decision instead ([`HookDecision::Block`] before a tool call,
    /// [`HookDecision::Terminate`] elsewhere). `llm/request` and
    /// `llm/result` accept no denial at all, so a failing gate there
    /// cannot fail closed; the failure is reported here — never folded
    /// into `Allow` — and the harness turns it into a warning.
    pub failures: Vec<String>,
}

/// The shared table of external registrations, keyed by mount point.
///
/// Clone is cheap and shares one table: every clone sees every
/// registration, and a [`HookRegistration`] handle dropped anywhere
/// withdraws its row everywhere.
#[derive(Clone, Default)]
pub struct HookRegistry {
    inner: Arc<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    entries: Mutex<HashMap<&'static str, Vec<Entry>>>,
    next_id: AtomicU64,
}

#[derive(Clone)]
struct Entry {
    id: u64,
    name: String,
    kind: Registered,
}

#[derive(Clone)]
enum Registered {
    Hook(Arc<dyn Hook>),
    Gate(Arc<dyn Gate>),
    Request(Arc<dyn RequestTransformer>),
    Result(Arc<dyn ResultTransformer>),
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
    key: &'static str,
    id: u64,
    registry: Weak<RegistryInner>,
}

impl std::fmt::Debug for HookRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRegistration")
            .field("key", &self.inner.key)
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
        let Some(list) = entries.get_mut(self.key) else {
            return;
        };
        list.retain(|entry| entry.id != self.id);
        if list.is_empty() {
            entries.remove(self.key);
        }
    }
}

impl HookRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an observer on `key`.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownMountPoint`] when `key` is not one of
    /// the seven mount points.
    pub fn register_hook(
        &self,
        key: &'static str,
        hook: Arc<dyn Hook>,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate(key)?;
        let name = hook.name().to_owned();
        Ok(self.add(key, name, Registered::Hook(hook)))
    }

    /// Register a gate on `key`.
    ///
    /// A gate fails closed: an `Err` or a panic from
    /// [`Gate::check`] ends the pass with the decision the mount
    /// point accepts — [`HookDecision::Block`] at `tool/pre-execute`
    /// (the model reads the refusal as the tool result),
    /// [`HookDecision::Terminate`] at the other points that can end a
    /// run. `llm/request` and `llm/result` accept no denial at all, so
    /// a failure there is reported in [`RegistryPass::failures`]
    /// instead — the harness writes a `hook` warning — and is never
    /// folded into `Allow`.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownMountPoint`] when `key` is not one of
    /// the seven mount points.
    pub fn register_gate(
        &self,
        key: &'static str,
        gate: Arc<dyn Gate>,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate(key)?;
        let name = gate.name().to_owned();
        Ok(self.add(key, name, Registered::Gate(gate)))
    }

    /// Register a request transformer.
    ///
    /// `key` must be [`LLM_REQUEST`]: the
    /// rewrite surface exists only on a `BeforeRequest` pass.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownMountPoint`] for an unknown key,
    /// [`RegistryError::WrongMountPoint`] for any other mount point.
    pub fn register_request_transformer(
        &self,
        key: &'static str,
        transformer: Arc<dyn RequestTransformer>,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate_for(key, LLM_REQUEST)?;
        let name = transformer.name().to_owned();
        Ok(self.add(key, name, Registered::Request(transformer)))
    }

    /// Register a result transformer.
    ///
    /// `key` must be [`TOOL_POST`]: the
    /// rewrite surface exists only on the tool-result seam.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownMountPoint`] for an unknown key,
    /// [`RegistryError::WrongMountPoint`] for any other mount point.
    pub fn register_result_transformer(
        &self,
        key: &'static str,
        transformer: Arc<dyn ResultTransformer>,
    ) -> Result<HookRegistration, RegistryError> {
        self.validate_for(key, TOOL_POST)?;
        let name = transformer.name().to_owned();
        Ok(self.add(key, name, Registered::Result(transformer)))
    }

    /// Whether `key` has at least one registration.
    #[must_use]
    pub fn has_entries(&self, key: &str) -> bool {
        lock(&self.inner.entries)
            .get(key)
            .is_some_and(|list| !list.is_empty())
    }

    /// Whether any result transformer is registered.
    #[must_use]
    pub fn has_result_transformers(&self) -> bool {
        self.has_entries(TOOL_POST)
    }

    /// Whether the registry holds no registration at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        lock(&self.inner.entries)
            .values()
            .all(|list| list.is_empty())
    }

    /// Run the registrations on `key` over `event`.
    ///
    /// Hooks and gates fold into the pass arbitration in registration
    /// order; request transformers rewrite the effective event in
    /// place. A deciding registration short-circuits the pass — later
    /// registrations do not run.
    pub async fn dispatch(
        &self,
        key: &'static str,
        event: &HookEvent,
        ctx: &HookContext<'_>,
    ) -> RegistryPass {
        let mut effective = event.clone();
        let mut arbitration = Arbitration::default();
        let mut denied_by = None;
        let mut failures = Vec::new();
        for entry in self.entries_for(key) {
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
                            tracing::warn!(
                                hook = %entry.name,
                                "hook registration panicked; skipped"
                            );
                            continue;
                        }
                    };
                    if !effective.accepts(&decision) {
                        tracing::debug!(
                            hook = %entry.name,
                            event = %uva_steering::event_name(&effective),
                            "hook decision not accepted here; ignored"
                        );
                        continue;
                    }
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
                    let (decision, failure) = match checked {
                        Ok(Ok(decision)) => (decision, None),
                        Ok(Err(reason)) => {
                            let why = format!("the gate failed: {reason}");
                            (gate_failure(&effective, why.clone()), Some(why))
                        }
                        Err(_) => {
                            let why = "the gate panicked".to_owned();
                            (gate_failure(&effective, why.clone()), Some(why))
                        }
                    };
                    if let Some(why) = &failure {
                        tracing::error!(
                            gate = %entry.name,
                            event = %uva_steering::event_name(&effective),
                            %why,
                            "gate could not answer"
                        );
                    }
                    // An ordinary decision the point does not accept is
                    // a no-op. A gate *failure* is not one: at the four
                    // points that can refuse, `gate_failure` synthesized
                    // the decision the point accepts, so it lands below;
                    // at `llm/request` and `llm/result` no denial exists
                    // to carry, so the failure is reported as a dispatch
                    // failure — the same conclusion the host's typed path
                    // reaches with `DiagnosticKind::GateFailed`.
                    if !effective.accepts(&decision) {
                        if let Some(why) = &failure {
                            failures.push(format!(
                                "gate `{}` could not answer at `{}`: {why}",
                                entry.name,
                                uva_steering::event_name(&effective)
                            ));
                            continue;
                        }
                        tracing::debug!(
                            gate = %entry.name,
                            event = %uva_steering::event_name(&effective),
                            "gate decision not accepted here; ignored"
                        );
                        continue;
                    }
                    apply_decision(
                        &mut arbitration,
                        &mut effective,
                        &mut denied_by,
                        entry.name,
                        decision,
                    );
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
                        Err(_) => tracing::warn!(
                            transformer = %entry.name,
                            "request transformer panicked; skipped"
                        ),
                    }
                }
                Registered::Result(_) => {}
            }
        }
        if arbitration.decided() {
            RegistryPass {
                arbitration,
                denied_by,
                event: None,
                failures,
            }
        } else {
            RegistryPass {
                arbitration,
                denied_by: None,
                event: Some(effective),
                failures,
            }
        }
    }

    /// Run the result transformers over `output`.
    ///
    /// Returns the names that ran, in registration order, when the
    /// aggregate output changed; an unchanged output reports no
    /// names, so the loop writes no provenance row for a no-op.
    pub async fn transform_result(
        &self,
        output: &mut ToolOutput,
        ctx: &HookContext<'_>,
    ) -> Vec<String> {
        let transformers: Vec<(String, Arc<dyn ResultTransformer>)> = self
            .entries_for(TOOL_POST)
            .into_iter()
            .filter_map(|entry| match entry.kind {
                Registered::Result(transformer) => {
                    Some((entry.name, transformer))
                }
                _ => None,
            })
            .collect();
        if transformers.is_empty() {
            return Vec::new();
        }
        let before = output.clone();
        let mut ran = Vec::new();
        for (name, transformer) in transformers {
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
                    tracing::warn!(
                        transformer = %name,
                        "result transformer panicked; skipped"
                    );
                }
            }
        }
        if *output == before {
            return Vec::new();
        }
        ran
    }

    fn validate(&self, key: &'static str) -> Result<(), RegistryError> {
        if is_mount_point(key) {
            Ok(())
        } else {
            Err(RegistryError::UnknownMountPoint(key.to_owned()))
        }
    }

    fn validate_for(
        &self,
        key: &'static str,
        expected: &'static str,
    ) -> Result<(), RegistryError> {
        self.validate(key)?;
        if key == expected {
            Ok(())
        } else {
            Err(RegistryError::WrongMountPoint { key, expected })
        }
    }

    fn add(
        &self,
        key: &'static str,
        name: String,
        kind: Registered,
    ) -> HookRegistration {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.entries)
            .entry(key)
            .or_default()
            .push(Entry { id, name, kind });
        HookRegistration {
            inner: Arc::new(RegistrationInner {
                key,
                id,
                registry: Arc::downgrade(&self.inner),
            }),
        }
    }

    fn entries_for(&self, key: &str) -> Vec<Entry> {
        lock(&self.inner.entries)
            .get(key)
            .cloned()
            .unwrap_or_default()
    }
}

// Fold one accepted decision into the pass.
//
// `ReplaceParams` rewrites the effective event so later registrations
// — and the harness's own typed registrations — see what the earlier
// one left. A denial records its source for the `hook_block` row.
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

// The decision a gate that could not answer synthesizes at `event`'s
// mount point.
//
// A gate fails closed, but "closed" is the point's own word: only
// `before_tool` turns a denial into a refused call the model can read
// (`Block`), and only the points on `HookEvent::accepts`'s termination
// list can end a run (`Terminate`). Synthesizing a blanket `Block` —
// the old behaviour — made the failure a no-op everywhere but
// `before_tool`, because the passes filter unaccepted decisions.
fn gate_failure(event: &HookEvent, why: String) -> HookDecision {
    match event {
        HookEvent::BeforeTool { .. } => HookDecision::Block { reason: why },
        _ => HookDecision::Terminate { reason: why },
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
