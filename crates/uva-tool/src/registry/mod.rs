//! Tool registry storage, registration, scoped cleanup, materialization,
//! dispatch, snapshots, and `Registry` integration.
//!
//! # Module layout
//!
//! - `catalog`: the descriptor / provenance / exposure types.
//! - `entry`: the registration value type + the internal storage row.
//! - `ToolRegistry` (this file): storage, registration, mutation.
//! - `scope` / `snapshot` / `dispatch`: method continuations.
//! - `registry_trait`: the `uva_core::Registry` impl, kept private
//!   to the crate.

use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;

use crate::{
    traits::{ExecutionMode, Tool},
    types::ToolOutput,
};

mod catalog;
mod dispatch;
mod entry;
mod registry_trait;
mod scope;
mod snapshot;

pub use catalog::{
    ToolAnnotations,
    ToolCategory,
    ToolDescriptor,
    ToolExposure,
    ToolMetadataSnapshot,
    ToolProvenance,
    ToolProvenanceRecord,
    Visibility,
};
pub use entry::ToolEntry;
use entry::{ProviderEntry, entry_from_provider};
pub use scope::{RegistrationScope, RegistrationToken};

/// Unified tool registry.
pub struct ToolRegistry {
    pub(crate) inner: RwLock<ToolRegistryInner>,
    /// Max parallel tool invocations per dispatch call: the bound
    /// `run_stream`'s merged stream keeps on in-flight tool drains.
    pub(crate) max_concurrent: usize,
    /// Monotonically increasing version counter, bumped on
    /// every register/unregister. Lets callers cheaply key a
    /// snapshot cache (e.g. `collect_tool_defs` in
    /// `crates/uva-app-server/src/routes/tool.rs`) by the
    /// current version without holding the registry lock or
    /// diffing the full tool list.
    pub(crate) version: std::sync::atomic::AtomicU64,
    /// Version-keyed memo of [`ToolRegistry::descriptors`]: the
    /// projection inputs the agent loop rebuilds every iteration. Holds
    /// `(registry version, descriptors)`; a version mismatch recomputes.
    pub(crate) descriptor_cache:
        parking_lot::Mutex<Option<(u64, Arc<Vec<ToolDescriptor>>)>>,
    /// Version-keyed memo of [`ToolRegistry::snapshot`]: the catalog
    /// every "what should the model see?" consumer starts from
    /// (the surface projection, the server's tool listing, tier caps).
    /// Same contract as `descriptor_cache` — `(registry version,
    /// snapshots)`, recomputed on a version mismatch.
    pub(crate) snapshot_cache:
        parking_lot::Mutex<Option<(u64, Arc<Vec<ToolMetadataSnapshot>>)>>,
    /// When `true`, `run_stream` validates every tool call's `input`
    /// against the resolved tool's `parameters()` (a JSON Schema)
    /// before the tool runs. Mismatches synthesize a
    /// `ToolOutput::error` listing the dotted-path violations, so the
    /// model can self-correct in the same turn. Default `false` —
    /// validation is opt-in so the hot dispatch path is unchanged
    /// for consumers that already validate inside `Tool::call` or
    /// trust the provider's tool-call arguments.
    pub(crate) validate_arguments: bool,
}

#[derive(Clone)]
pub(crate) struct ToolRegistryInner {
    /// Tool name → entries (LIFO for non-core tools).
    pub(crate) tools: HashMap<String, Vec<ProviderEntry>>,
    /// Next registration token.
    pub(crate) next_registration: u64,
}

/// The single dispatch decision for one model-issued tool call:
/// either the tool to execute with its (validated) input, or the
/// refusal the dispatcher reports instead.
///
/// Produced by [`ToolRegistry::resolve_dispatch`] — the one source
/// of the hidden-refusal + argument-validation decision that
/// `run_stream`, `dispatch_one`, and the harness loop all dispatch
/// on.
pub enum DispatchPlan {
    /// Resolve succeeded: run this tool with this input.
    Execute(Arc<dyn Tool>, serde_json::Value),
    /// The call is refused before any tool runs.
    Refused {
        /// Name exactly as it appeared in the call.
        tool_name: String,
        /// Why the call was refused.
        reason: DispatchRefusal,
    },
}

impl std::fmt::Debug for DispatchPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Arc<dyn Tool>` has no Debug; the tool's name is the
        // useful rendering.
        match self {
            DispatchPlan::Execute(tool, input) => f
                .debug_tuple("Execute")
                .field(&tool.name())
                .field(input)
                .finish(),
            DispatchPlan::Refused { tool_name, reason } => f
                .debug_struct("Refused")
                .field("tool_name", tool_name)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// Why a [`DispatchPlan::Refused`] refused a call.
#[derive(Debug)]
pub enum DispatchRefusal {
    /// No dispatchable tool by that name. Also the answer for a
    /// hidden tool: the two are indistinguishable by contract
    /// (pinned by
    /// `dispatch_one_refuses_hidden_tool_with_not_found_shape`).
    Unknown,
    /// The call's `input` failed JSON-Schema validation against the
    /// tool's `parameters()`; the violations are listed so the
    /// model can self-correct in the same turn.
    InvalidArguments {
        violations: Vec<uva_core::SchemaViolation>,
    },
}

impl DispatchRefusal {
    /// The error `ToolOutput` a dispatcher synthesises for this
    /// refusal — the single rendering both the registry's
    /// dispatchers and the harness loop report. Formats kept
    /// byte-identical to the pre-`resolve_dispatch` dispatcher
    /// (pinned by the argument-validation tests and example).
    #[must_use]
    pub fn tool_output(&self, tool_name: &str) -> ToolOutput {
        match self {
            DispatchRefusal::Unknown => {
                ToolOutput::error(format!("tool not found: {tool_name}"))
            }
            DispatchRefusal::InvalidArguments { violations } => {
                // Format the violations as a numbered list so the
                // model gets every field it needs to fix in one
                // read. An empty `violations` is unreachable in
                // practice (`validate_against_schema` returns
                // `Err(vec)` only on failure), but we handle it
                // defensively to keep the error path total.
                if violations.is_empty() {
                    return ToolOutput::error(format!(
                        "invalid arguments for tool `{tool_name}`"
                    ));
                }
                let lines: Vec<String> =
                    violations.iter().map(|v| format!("- {v}")).collect();
                ToolOutput::error(format!(
                    "invalid arguments for tool `{tool_name}` ({} violation{}):\n{}",
                    violations.len(),
                    if violations.len() == 1 { "" } else { "s" },
                    lines.join("\n"),
                ))
            }
        }
    }
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(ToolRegistryInner {
                tools: HashMap::new(),
                next_registration: 1,
            }),
            max_concurrent: 5,
            version: std::sync::atomic::AtomicU64::new(0),
            descriptor_cache: parking_lot::Mutex::new(None),
            snapshot_cache: parking_lot::Mutex::new(None),
            validate_arguments: false,
        }
    }

    /// Enable JSON-Schema validation of every tool call's arguments
    /// against the tool's `parameters()` before dispatch.
    ///
    /// Off by default — the common case is "the provider's tool-call
    /// arguments are already well-formed, the tool validates inside
    /// `call`". A deployment that wants the registry to surface a
    /// schema mismatch as an `is_error` tool result (with the
    /// dotted-path violations in the body) flips this on once at
    /// boot:
    ///
    /// ```ignore
    /// let registry = ToolRegistry::new()
    ///     .with_argument_validation(true);
    /// ```
    ///
    /// Validation is paid once per `run_stream` call, in the same
    /// read-lock span as the tool lookup, so a misbehaving call
    /// costs a synthesized error Result instead of running a tool
    /// it shouldn't. Empty / non-object schemas pass through
    /// (matches `uva_core::validate_against_schema` — unknown
    /// keywords are annotations, not assertions).
    #[must_use]
    pub fn with_argument_validation(mut self, on: bool) -> Self {
        self.validate_arguments = on;
        self
    }

    /// Whether the registry validates tool-call arguments against
    /// the tool's `parameters()` before dispatch.
    #[must_use]
    pub fn argument_validation_enabled(&self) -> bool {
        self.validate_arguments
    }

    /// Resolve one tool call to a [`DispatchPlan`]: the single
    /// source of the hidden-refusal + argument-validation decision
    /// behind `run_stream`, `dispatch_one`, and the harness loop's
    /// tool routing. Takes the read lock; a batch that wants one
    /// lock span resolves its calls from `run_stream` instead.
    #[must_use]
    pub fn resolve_dispatch(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> DispatchPlan {
        let inner = self.inner.read();
        self.resolve_plan_locked(&inner, name, input)
    }

    /// [`ToolRegistry::resolve_dispatch`] under a read lock the
    /// caller already holds, so a whole batch resolves in one lock
    /// span before any await point.
    fn resolve_plan_locked(
        &self,
        inner: &ToolRegistryInner,
        name: &str,
        input: serde_json::Value,
    ) -> DispatchPlan {
        // `is_hidden` is the registry's privacy flag: a hidden tool
        // is never advertised and is refused here, with the same
        // not-found shape as an unknown name. `ToolExposure::Hidden`
        // is the softer, advertisement-only level and stays
        // dispatchable.
        let Some(entry) = inner.tools.get(name).and_then(|e| e.last()) else {
            return DispatchPlan::Refused {
                tool_name: name.to_string(),
                reason: DispatchRefusal::Unknown,
            };
        };
        if entry.is_hidden {
            return DispatchPlan::Refused {
                tool_name: name.to_string(),
                reason: DispatchRefusal::Unknown,
            };
        }
        if self.validate_arguments
            && let Err(violations) = uva_core::validate_against_schema(
                &entry.tool.parameters(),
                &input,
            )
        {
            return DispatchPlan::Refused {
                tool_name: name.to_string(),
                reason: DispatchRefusal::InvalidArguments { violations },
            };
        }
        DispatchPlan::Execute(Arc::clone(&entry.tool), input)
    }

    /// Register a `ToolEntry` directly — wraps the inner `Arc<dyn Tool>`
    /// into a `ProviderEntry` and inserts it.
    ///
    /// Returns `true` if the tool was inserted; `false` if a Core tool
    /// with the same name already exists and won the immutability
    /// guard.
    pub fn register_entry(&self, entry: ToolEntry) -> bool {
        let mut inner = self.inner.write();
        let inserted = self.register_entry_inner(&mut inner, entry).is_some();
        if inserted {
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        inserted
    }

    /// Register `entry` only if no tool of that name is registered.
    ///
    /// The existence check and the insert share one write guard, so
    /// two callers racing for the same name cannot both see it free
    /// and then both insert: exactly one wins and the loser leaves
    /// the registry untouched. [`Self::get`] followed by
    /// [`Self::register_entry`] is the same decision without that
    /// atomicity, and lets the second caller shadow the first.
    ///
    /// Returns `true` if the tool was inserted; `false` if the name
    /// was already taken, by a Core tool or by a dynamic one.
    pub fn register_entry_if_absent(&self, entry: ToolEntry) -> bool {
        let mut inner = self.inner.write();
        let name = uva_core::RegistryItem::name(&entry).to_string();
        if inner.tools.contains_key(&name) {
            return false;
        }
        let inserted = self.register_entry_inner(&mut inner, entry).is_some();
        if inserted {
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        inserted
    }

    /// Register a tool the binary itself ships, marking it
    /// [`ToolProvenance::Core`]: the name then cannot be taken by a
    /// later registration, so a runtime `POST`, a host tool or a
    /// plugin tool is refused instead of shadowing it. A second core
    /// registration under a core-owned name is refused too.
    ///
    /// Returns `true` if the tool was inserted.
    pub fn register_core_entry(&self, entry: ToolEntry) -> bool {
        let name = uva_core::RegistryItem::name(&entry).to_string();
        let mut inner = self.inner.write();
        if core_occupies(&inner, &name) {
            return false;
        }
        let token = RegistrationToken(inner.next_registration);
        inner.next_registration += 1;
        inner.tools.entry(name).or_default().push(ProviderEntry {
            provider_token: token,
            provenance: ToolProvenance::Core,
            is_hidden: entry.is_hidden(),
            exposure: entry.exposure(),
            tool: entry.tool_instance(),
        });
        drop(inner);
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// Whether a Core tool owns `name`.
    ///
    /// This is the immutability predicate registration refuses on,
    /// exposed so a caller that learns a name out of band — a
    /// plugin's claimed tools, an operator's config entry — can ask
    /// the same question before reporting a registration as a
    /// success.
    #[must_use]
    pub fn core_owns(&self, name: &str) -> bool {
        core_occupies(&self.inner.read(), name)
    }

    /// Insert a [`ToolEntry`] into `inner.tools`, allocating a fresh
    /// `RegistrationToken` for it. Caller must hold the write lock on
    /// `inner`. Returns the token used if the entry was inserted, or
    /// `None` if a core tool already occupies the name.
    fn register_entry_inner(
        &self,
        inner: &mut ToolRegistryInner,
        entry: ToolEntry,
    ) -> Option<RegistrationToken> {
        let tool = entry.tool_instance();
        let name = uva_core::RegistryItem::name(&entry).to_string();
        if core_occupies(inner, &name) {
            return None;
        }
        let token = RegistrationToken(inner.next_registration);
        inner.next_registration += 1;
        let provider_entry = ProviderEntry {
            provider_token: token.clone(),
            provenance: ToolProvenance::Dynamic,
            is_hidden: entry.is_hidden(),
            exposure: entry.exposure(),
            tool,
        };
        inner.tools.entry(name).or_default().push(provider_entry);
        Some(token)
    }

    /// Register a core tool and return the withdrawal token.
    ///
    /// The same immutability the plain core registration gives —
    /// the name cannot be shadowed by a later dynamic registration —
    /// plus the token a plugin's effect needs to withdraw it. The
    /// core tool's first registration is authoritative: a second one
    /// under a core-owned name is refused with `None`.
    pub fn register_core_entry_with_token(
        &self,
        entry: ToolEntry,
    ) -> Option<RegistrationToken> {
        let name = uva_core::RegistryItem::name(&entry).to_string();
        let mut inner = self.inner.write();
        if core_occupies(&inner, &name) {
            return None;
        }
        let token = RegistrationToken(inner.next_registration);
        inner.next_registration += 1;
        inner.tools.entry(name).or_default().push(ProviderEntry {
            provider_token: token.clone(),
            provenance: ToolProvenance::Core,
            is_hidden: entry.is_hidden(),
            exposure: entry.exposure(),
            tool: entry.tool_instance(),
        });
        drop(inner);
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(token)
    }

    /// Register a `ToolEntry` directly and return the token that
    /// identifies it.
    ///
    /// The token is what a later withdrawal names:
    /// [`Self::unregister_by_token`] removes exactly the entries
    /// registered under it and leaves every other entry filed under
    /// the same name alone — a name can hold a stack of entries, and
    /// withdrawing one of them must not take the others with it.
    ///
    /// Returns `None` if a Core tool with the same name already
    /// exists and won the immutability guard.
    pub fn register_entry_with_token(
        &self,
        entry: ToolEntry,
    ) -> Option<RegistrationToken> {
        let mut inner = self.inner.write();
        let token = self.register_entry_inner(&mut inner, entry)?;
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(token)
    }

    /// Remove all entries whose `tool.name()` matches the given plain
    /// name. Returns `true` if anything was removed.
    pub fn unregister_by_name(&self, name: &str) -> bool {
        let mut inner = self.inner.write();
        let removed = inner.tools.remove(name).is_some();
        if removed {
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        removed
    }

    /// Change how much of `name` the model is shown, after
    /// registration. Returns whether the tool exists: `false` — with
    /// no mutation — when no tool by that name is registered, so a
    /// caller never mistakes a typo for a successful policy.
    ///
    /// The write lands on the entry the read paths resolve to (the
    /// last-registered provider entry for the name — the same one
    /// [`ToolRegistry::descriptors`], [`ToolRegistry::snapshot`] and
    /// `run_stream` use), so the change reaches the model-facing
    /// projection immediately. Unlike [`ToolExposure::Hidden`], an
    /// exposure change never alters dispatch. A successful *change*
    /// bumps [`ToolRegistry::version`], which is what invalidates the
    /// server's model-facing definition cache; setting a value the
    /// entry already carries is a no-op that still returns `true`
    /// and leaves the version alone.
    pub fn set_exposure(&self, name: &str, exposure: ToolExposure) -> bool {
        self.mutate_entry(name, |entry| {
            let changed = entry.exposure != exposure;
            entry.exposure = exposure;
            changed
        })
    }

    /// Set `name`'s privacy flag after registration. `hidden = true`
    /// removes the tool from every listing **and** makes
    /// [`ToolRegistry::run_stream`] refuse a model-issued call,
    /// mirroring [`ToolEntry::with_is_hidden`]; `false` restores it.
    ///
    /// Returns whether the tool exists (`false` mutates nothing), and
    /// bumps [`ToolRegistry::version`] on a successful change.
    pub fn set_hidden(&self, name: &str, hidden: bool) -> bool {
        self.mutate_entry(name, |entry| {
            let changed = entry.is_hidden != hidden;
            entry.is_hidden = hidden;
            changed
        })
    }

    /// How much of `name` the model is shown, or `None` when the tool
    /// is not registered.
    ///
    /// Reads the same entry [`ToolRegistry::descriptors`] projects.
    /// Hiding is orthogonal: a hidden tool still reports the exposure
    /// it carries.
    #[must_use]
    pub fn exposure(&self, name: &str) -> Option<ToolExposure> {
        self.inner
            .read()
            .tools
            .get(name)
            .and_then(|entries| entries.last())
            .map(|entry| entry.exposure)
    }

    /// The scheduling [`ExecutionMode`] of the tool `name`
    /// resolves to, read under the same lock
    /// [`ToolRegistry::exposure`] uses — without materializing a
    /// `ToolEntry`. Missing tools resolve to
    /// [`ExecutionMode::Sequential`], the conservative default:
    /// an unknown call fails in the bucket that cannot fan out.
    #[must_use]
    pub fn execution_mode(&self, name: &str) -> ExecutionMode {
        self.inner
            .read()
            .tools
            .get(name)
            .and_then(|entries| entries.last())
            .map(|entry| entry.tool.mode())
            .unwrap_or(ExecutionMode::Sequential)
    }

    #[must_use]
    /// The scheduling [`ExecutionMode`] of **this call**: the
    /// tool's per-call classification
    /// ([`Tool::mode_for`]) over its
    /// arguments, and the conservative default for a name that does
    /// not resolve.
    ///
    /// The batch scheduler asks this, not [`Self::execution_mode`]:
    /// a tool may be safe beside another read for one set of
    /// arguments and not for the next.
    pub fn execution_mode_for(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> ExecutionMode {
        self.inner
            .read()
            .tools
            .get(name)
            .and_then(|entries| entries.last())
            .map(|entry| entry.tool.mode_for(input))
            .unwrap_or(ExecutionMode::Sequential)
    }

    /// The entry `name` currently resolves to: the last-registered
    /// provider wins (the same LIFO top every read path —
    /// [`ToolRegistry::exposure`], [`ToolRegistry::descriptors`],
    /// `run_stream` — resolves), cloned out under one read lock.
    /// Unlike [`ToolRegistry::snapshot`] this deliberately sees
    /// hidden tools: dispatch and wiring questions need the real
    /// binding. The `uva_core::registry::Registry` impl delegates
    /// here, so the HTTP surface and this method cannot drift.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<ToolEntry> {
        let inner = self.inner.read();
        inner
            .tools
            .get(name)
            .and_then(|entries| entries.last())
            .map(entry_from_provider)
    }

    /// The files a call to `name` touches.
    ///
    /// The tool answers for itself (it knows how it resolves its own
    /// arguments); a name the registry does not have touches nothing.
    #[must_use]
    /// The files a call to `name` touches.
    ///
    /// The tool answers for itself (it knows how it resolves its own
    /// arguments); a name the registry does not have touches nothing.
    pub fn locations_for(
        &self,
        name: &str,
        input: &serde_json::Value,
        context: &crate::types::Context,
    ) -> Vec<crate::types::ToolLocation> {
        self.get(name)
            .map(|entry| entry.tool.locations(input, context))
            .unwrap_or_default()
    }

    /// Apply `mutate` to the entry `name` resolves to. Returns
    /// whether the tool existed; bumps the version only when `mutate`
    /// reports a real change. One write-lock acquisition, no awaits,
    /// so callers may invoke it from a sync context.
    fn mutate_entry(
        &self,
        name: &str,
        mutate: impl FnOnce(&mut ProviderEntry) -> bool,
    ) -> bool {
        let mut inner = self.inner.write();
        let Some(entry) = inner.tools.get_mut(name).and_then(|e| e.last_mut())
        else {
            return false;
        };
        let changed = mutate(entry);
        drop(inner);
        if changed {
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        true
    }

    /// Whether a tool named `name` is registered — under the same
    /// key `run_stream` dispatches by, and **including hidden
    /// tools**. This answers a wiring question ("is this name
    /// real?") rather than the model-facing one, so unlike
    /// [`ToolRegistry::snapshot`] it deliberately sees the whole
    /// index; callers that want the advertised set use `snapshot`.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.inner.read().tools.contains_key(name)
    }

    /// How many entries are filed under `name`.
    ///
    /// A name can hold a **stack** of entries: the registry files a
    /// second dynamic tool under a name that is already taken, and
    /// the later one wins the dispatch. That is what makes this
    /// count the question a caller must ask before it takes a name
    /// somebody else already holds: one entry is the caller's own,
    /// two or more means somebody else is under the name too.
    ///
    /// A name a Core tool owns reports one — the Core entry — even
    /// when a dynamic entry was refused against it, because a
    /// refused registration never reaches the index.
    #[must_use]
    pub fn name_occupants(&self, name: &str) -> usize {
        self.inner
            .read()
            .tools
            .get(name)
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    /// Return the number of registered tools (LIFO top-only count).
    pub fn tool_count(&self) -> usize {
        let inner = self.inner.read();
        inner.tools.len()
    }

    /// Current monotonic version of the registry's tool set.
    /// Bumped on every successful `register_entry` /
    /// `unregister_by_name`. Cheap (one relaxed atomic load) —
    /// callers can use this to key a snapshot cache without
    /// holding the registry lock.
    pub fn version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// The immutability rule, in one place: a name a Core tool owns is not
// registrable again.
fn core_occupies(inner: &ToolRegistryInner, name: &str) -> bool {
    inner.tools.get(name).is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.provenance == ToolProvenance::Core)
    })
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        let inner = self.inner.read().clone();
        Self {
            inner: RwLock::new(inner),
            max_concurrent: self.max_concurrent,
            version: std::sync::atomic::AtomicU64::new(
                self.version.load(std::sync::atomic::Ordering::Relaxed),
            ),
            // The memo is a pure function of (registrations, version);
            // both travel with the clone, so the cached list stays
            // valid.
            descriptor_cache: parking_lot::Mutex::new(
                self.descriptor_cache.lock().clone(),
            ),
            snapshot_cache: parking_lot::Mutex::new(
                self.snapshot_cache.lock().clone(),
            ),
            validate_arguments: self.validate_arguments,
        }
    }
}

#[cfg(test)]
mod tests;
