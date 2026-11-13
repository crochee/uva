//! The model-facing tool surface: one pure projection from registered
//! [`ToolDescriptor`]s to the [`ToolDefinition`]s an LLM request
//! carries.
//!
//! A registry answers two different questions — *what can run?* and
//! *what is the model told about?* — and they must not be conflated.
//! [`ToolRegistry`] owns the first; this module
//! owns the second. Nothing here mutates state, so a projection is
//! replay-stable: the same descriptors + transcript + filter always
//! yield the same list, in the same order.
//!
//! ## Exposure rules
//!
//! For each descriptor that survives the visibility filter:
//!
//! | exposure | in the transcript | advertised as |
//! |---|---|---|
//! | `Direct` | either | name + description + full `parameters()` schema |
//! | `Deferred` | no | name + description + open placeholder schema |
//! | `Deferred` | yes | name + description + full `parameters()` schema |
//! | `Hidden` | either | nothing |
//!
//! `is_hidden` (the registry's privacy flag) removes a tool from the
//! projection regardless of its exposure; unlike
//! [`ToolExposure::Hidden`] it *also* makes
//! [`ToolRegistry::run_stream`](crate::ToolRegistry::run_stream)
//! refuse the tool.
//!
//! A `Deferred` tool is advertised with
//! `{"type":"object","additionalProperties":true}` instead of its real
//! schema: the model can call it from the description alone, and the
//! tool's own argument validation still runs on dispatch. This is a
//! deliberate divergence from pi
//! (`packages/ai/src/utils/deferred-tools.ts`), which drops deferred
//! tools from the advertised list entirely and relies on the
//! transcript mentioning their names — a provider that rejects an
//! unadvertised tool name would turn a prompt-economy choice into a
//! hard failure.
//!
//! ## Promotion is transcript-derived
//!
//! [`called_tool_names`] scans the messages that will be sent and
//! returns every tool name they mention, so a `Deferred` tool promotes
//! itself on the next request after its first call without the
//! registry — or any other mutable state — tracking anything.
//!
//! ## Composing the visible-name filter
//!
//! [`project_tool_definitions`] takes an optional set of visible
//! names, so a caller that already decided what this request may
//! advertise — [`ToolSurfacePolicy::visible_tool_names`] is the usual
//! source — can hand the allow-list straight to the projection:
//!
//! ```
//! # use std::sync::Arc;
//! # use uva_tool::{
//! #     ToolRegistry, called_tool_names, project_tool_definitions,
//! # };
//! # let registry = Arc::new(ToolRegistry::new());
//! # let messages: Vec<uva_provider::Message> = Vec::new();
//! let policy = uva_tool::ToolSurfacePolicy::default();
//! let visible = policy.visible_tool_names(&registry.descriptors());
//! let defs = project_tool_definitions(
//!     &registry.descriptors(),
//!     &called_tool_names(&messages),
//!     Some(&visible),
//! );
//! ```
//!
//! Passing `None` advertises every non-hidden tool.
//!
//! ## Deployment-level policy
//!
//! [`ToolSurfacePolicy`] is the serializable form of that decision: a
//! deployment declares groups, an active set, and a cap once.
//! [`ToolSurfacePolicy::write_to_registry`] writes the group verdict
//! onto the registry's [`ToolExposure`]s, while
//! [`ToolSurfacePolicy::visible_tool_names`] is the allow-list half
//! the projection takes — so a config file reaches the same seam as a
//! programmatic caller.

use std::collections::{BTreeMap, HashSet};

use thiserror::Error;
use uva_provider::{ContentPart, Message, ToolDefinition};

use crate::{
    ToolRegistry,
    registry::{ToolDescriptor, ToolExposure, Visibility},
};

/// The schema advertised for a `Deferred` tool before its first call.
///
/// Deliberately permissive: the model chooses arguments from the tool's
/// description, and the tool validates them itself on dispatch.
fn placeholder_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": true,
    })
}

/// Project `descriptors` into the model-facing tool list.
///
/// Ordering follows `descriptors`; [`ToolRegistry::descriptors`](crate::ToolRegistry::descriptors)
/// sorts by name for a stable list. `called` is the set of tool names
/// the transcript already mentions (see [`called_tool_names`]) and
/// drives `Deferred` promotion. `visible` is an optional allow-list —
/// `None` advertises every non-hidden tool.
///
/// See the module docs for the exposure rules and for how
/// [`ToolSurfacePolicy::visible_tool_names`] produces the filter this
/// function takes.
#[must_use]
pub fn project_tool_definitions(
    descriptors: &[ToolDescriptor],
    called: &HashSet<String>,
    visible: Option<&HashSet<String>>,
) -> Vec<ToolDefinition> {
    let mut defs = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        if !is_advertised(descriptor, visible) {
            continue;
        }
        defs.push(definition_for(descriptor, called));
    }
    defs
}

/// Whether `descriptor` reaches the model at all: visibility not
/// `Hidden`, and (when a filter is given) named by it. Reads the
/// descriptor's own [`ToolDescriptor::visibility`] derivation —
/// the same total rule the contract test in
/// `crates/uva-tool/tests/visibility.rs` pins.
fn is_advertised(
    descriptor: &ToolDescriptor,
    visible: Option<&HashSet<String>>,
) -> bool {
    if descriptor.visibility() == Visibility::Hidden {
        return false;
    }
    visible
        .map(|names| names.contains(&descriptor.name))
        .unwrap_or(true)
}

/// The definition one advertised descriptor contributes: full schema,
/// except for a not-yet-called `Deferred` tool, which gets the
/// placeholder.
fn definition_for(
    descriptor: &ToolDescriptor,
    called: &HashSet<String>,
) -> ToolDefinition {
    let schema = match descriptor.exposure {
        ToolExposure::Deferred if !called.contains(&descriptor.name) => {
            placeholder_schema()
        }
        _ => descriptor.parameters.clone(),
    };
    let mut def = ToolDefinition::new(
        descriptor.name.clone(),
        descriptor.description.clone(),
        schema,
    );
    if let Some(annotations) = descriptor.annotations {
        def.annotations = Some(annotations.into());
    }
    def
}

/// Every tool name the transcript mentions: `ToolUse` names (the
/// assistant asking for a tool) plus `ToolResult` names (a result
/// injected or replayed without its matching call, e.g. from a session
/// log).
///
/// The scan is by content shape, not role, so it survives a replayed
/// history whose roles were rewritten. O(messages × parts), no state:
/// promotion needs no mutable registry bookkeeping.
#[must_use]
pub fn called_tool_names(messages: &[Message]) -> HashSet<String> {
    let mut names = HashSet::new();
    for message in messages {
        for part in &message.content {
            match part {
                ContentPart::ToolUse(call) => {
                    names.insert(call.name.clone());
                }
                ContentPart::ToolResult(result) => {
                    if let Some(name) = result.tool_name.as_ref() {
                        names.insert(name.clone());
                    }
                }
                _ => {}
            }
        }
    }
    names
}

/// Why a [`ToolSurfacePolicy`] was rejected.
///
/// Every variant names the group and tool involved, so a deployment
/// wiring a catalog finds the typo without guessing.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SurfacePolicyError {
    /// A group must have a name; an empty one cannot be activated or
    /// logged meaningfully.
    #[error("group name must not be empty")]
    EmptyGroupName,
    /// [`ToolSurfacePolicy::active_groups`] names a group that was
    /// never declared in [`ToolSurfacePolicy::groups`].
    #[error("active group `{name}` was never declared")]
    UnknownGroup {
        /// Undeclared group name.
        name: String,
    },
    /// A group names a tool the registry does not have.
    #[error("group `{group}` names tool `{tool}`, which is not registered")]
    UnknownTool {
        /// Group being declared.
        group: String,
        /// Unregistered tool name.
        tool: String,
    },
    /// The same tool appears twice in one group.
    #[error("group `{group}` names tool `{tool}` twice")]
    DuplicateTool {
        /// Group being declared.
        group: String,
        /// Repeated tool name.
        tool: String,
    },
    /// The tool is claimed by a different group, so this one cannot
    /// also claim it.
    #[error(
        "tool `{tool}` is already in group `{existing}`, so group \
         `{group}` cannot claim it"
    )]
    AlreadyGrouped {
        /// Group being declared.
        group: String,
        /// Contested tool name.
        tool: String,
        /// Group that already claims it — the earlier group in
        /// `BTreeMap` order, so the error is deterministic.
        existing: String,
    },
}

/// A deployment-level description of the model-facing tool surface.
///
/// The policy carries the two levers that decide what the model is
/// told about — named groups with an active set, and a cap on how many
/// tools are advertised — as one serializable value: a deployment
/// declares the surface once,
/// [`ToolSurfacePolicy::write_to_registry`] writes the group verdict
/// onto the registry's [`ToolExposure`]s, and
/// [`ToolSurfacePolicy::visible_tool_names`] produces the allow-list
/// [`project_tool_definitions`] filters with.
///
/// ## Group rule
///
/// | tool's group | [`write_to_registry`](Self::write_to_registry) writes | advertised |
/// |---|---|---|
/// | active | [`ToolExposure::Direct`] | yes |
/// | inactive | [`ToolExposure::Hidden`] | no |
/// | none | untouched | as registered (`Direct` by default) |
///
/// Inactive members become `Hidden` *exposure* — not
/// [`ToolRegistry::set_hidden`]'s privacy flag — so they stay
/// dispatcheable. Narrowing what the model sees is a prompt-economy
/// decision, never a security boundary.
///
/// [`write_to_registry`](Self::write_to_registry) writes **only** tools
/// named by a group; a tool in no group keeps whatever exposure it
/// was registered with. That keeps the policy composable with
/// registration-time `with_exposure` and with other writers (e.g. a
/// server's `deferred`/`hidden` lists).
///
/// ## The cap is the caller's
///
/// `max_visible` is deliberately *not* applied by
/// [`write_to_registry`](Self::write_to_registry): it belongs to one
/// request, not to the registry — the same catalog may be shown
/// capped to a small model and uncapped to a large one.
/// [`visible_tool_names`](Self::visible_tool_names) is the caller-side
/// half: it returns the capped allow-list, and the caller hands it to
/// [`project_tool_definitions`]. Tools the registry already hides
/// (`is_hidden`, or [`ToolExposure::Hidden`]) take no cap slot, so the
/// cap applies after [`ToolRegistry::snapshot`]'s own filtering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSurfacePolicy {
    /// Cap on advertised tools, or `None` for the whole catalog.
    /// Applied by [`ToolSurfacePolicy::visible_tool_names`], not by
    /// [`ToolSurfacePolicy::write_to_registry`].
    pub max_visible: Option<usize>,
    /// Declared groups: group name → member tool names, in
    /// declaration order. A `BTreeMap`, so iteration (and therefore
    /// every error and write) is name-sorted and replay-stable.
    pub groups: BTreeMap<String, Vec<String>>,
    /// Groups whose members are advertised. Every name must be a
    /// declared group.
    pub active_groups: Vec<String>,
}

impl ToolSurfacePolicy {
    /// Whether `group` is listed in [`Self::active_groups`].
    #[must_use]
    pub fn is_active(&self, group: &str) -> bool {
        self.active_groups.iter().any(|name| name == group)
    }

    /// Write this policy's group verdict onto `registry`.
    ///
    /// Every member must be registered and claimed by exactly one
    /// group, every active group must be declared, and group names
    /// must be non-empty. Validation completes before the first
    /// write, so a rejected policy changes nothing.
    ///
    /// # Errors
    ///
    /// Validate the whole policy against `registry` without writing.
    ///
    /// Public so a deployment can pre-flight a policy before binding
    /// it to a registry (the server boot path does this via
    /// [`Self::validate`]; an operator feeding the policy from a UI
    /// can do the same). The registry mutation is a separate step —
    /// see [`Self::write_to_registry`].
    pub fn validate(
        &self,
        registry: &ToolRegistry,
    ) -> Result<(), SurfacePolicyError> {
        // tool → owning group, filled in first-declaration order.
        let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
        for (group, tools) in &self.groups {
            if group.is_empty() {
                return Err(SurfacePolicyError::EmptyGroupName);
            }
            for tool in tools {
                if !registry.contains(tool) {
                    return Err(SurfacePolicyError::UnknownTool {
                        group: group.clone(),
                        tool: tool.clone(),
                    });
                }
                match owner.insert(tool.as_str(), group.as_str()) {
                    None => {}
                    Some(previous) if previous == group => {
                        return Err(SurfacePolicyError::DuplicateTool {
                            group: group.clone(),
                            tool: tool.clone(),
                        });
                    }
                    Some(previous) => {
                        return Err(SurfacePolicyError::AlreadyGrouped {
                            group: group.clone(),
                            tool: tool.clone(),
                            existing: previous.to_string(),
                        });
                    }
                }
            }
        }
        for name in &self.active_groups {
            if !self.groups.contains_key(name) {
                return Err(SurfacePolicyError::UnknownGroup {
                    name: name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Write each group's verdict onto the registry as a per-tool
    /// [`ToolExposure`]: active → `Direct`; inactive → `Hidden`;
    /// groupless → untouched. Single registry-mutating seam for a
    /// [`ToolSurfacePolicy`].
    ///
    /// # Errors
    ///
    /// [`SurfacePolicyError`] on validation failure; the registry
    /// is not mutated.
    pub fn write_to_registry(
        &self,
        registry: &ToolRegistry,
    ) -> Result<(), SurfacePolicyError> {
        self.validate(registry)?;
        for (group, tools) in &self.groups {
            let exposure = if self.is_active(group) {
                ToolExposure::Direct
            } else {
                ToolExposure::Hidden
            };
            for tool in tools {
                // Validated above; `false` here would mean a
                // concurrent unregister between validation and
                // write, which leaves the registry consistent.
                if !registry.set_exposure(tool, exposure) {
                    tracing::warn!(
                        group = %group,
                        tool = %tool,
                        "tool surface policy: tool vanished during write_to_registry"
                    );
                }
            }
        }
        Ok(())
    }

    /// Allow-list [`project_tool_definitions`] should filter with:
    /// every groupless tool, every member of an active group, and —
    /// when [`Self::max_visible`] is set — only the first N in
    /// `descriptors` order. Already-hidden descriptors and MCP
    /// self-management tools take no cap slot.
    ///
    /// Lenient read side: a name not in `descriptors` cannot match,
    /// a duplicate claim is "any active claimant wins". The strict
    /// validator is [`ToolSurfacePolicy::write_to_registry`].
    #[must_use]
    pub fn visible_tool_names(
        &self,
        descriptors: &[ToolDescriptor],
    ) -> HashSet<String> {
        let mut names = HashSet::new();
        let mut budget = self.max_visible;
        for descriptor in descriptors {
            if descriptor.is_hidden
                || descriptor.exposure == ToolExposure::Hidden
                || crate::is_mcp_self_tool(&descriptor.name)
                || !self.admits(&descriptor.name)
            {
                continue;
            }
            match budget {
                Some(0) => break,
                Some(remaining) => budget = Some(remaining - 1),
                None => {}
            }
            names.insert(descriptor.name.clone());
        }
        names
    }

    /// Whether the group rule admits `tool`: groupless tools and
    /// members of active groups are admitted; a member of only
    /// inactive groups is not.
    fn admits(&self, tool: &str) -> bool {
        let mut claimed = false;
        for (group, members) in &self.groups {
            if members.iter().any(|member| member == tool) {
                if self.is_active(group) {
                    return true;
                }
                claimed = true;
            }
        }
        !claimed
    }
}

#[cfg(test)]
mod tests;
