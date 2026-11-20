//! The **brick** lifecycle: register tool entries in the `tools`
//! service, and withdraw exactly those entries when the brick
//! unloads.
//!
//! A brick is a kernel plugin whose whole job is to make something
//! callable: it registers its entries in the [`ToolRegistry`] the
//! `tools` service hands out, and it takes them back on unload. The
//! registry is the [`ToolRegistryPlugin`]'s to provide; the brick is
//! the brick's own.
//!
//! # Why one owner
//!
//! Nine bricks used to spell this lifecycle out for themselves —
//! resolve the service, register the entries, refuse loudly when the
//! registry refuses, roll back what was already registered, file one
//! effect that withdraws exactly what was filed. The copies drifted:
//! one of them refused with a different message that dropped the
//! tool's name, and the rollback path (a brick that registers several
//! entries and is refused halfway) had no test at all. The lifecycle
//! is one thing; it lives here.
//!
//! # What the brick still owns
//!
//! The *manifest* — the plugin id, the services it injects, the
//! services it provides — belongs to each brick: no two bricks inject
//! the same set, and two of them publish a service of their own
//! besides their tools. The brick also owns the **provenance** of
//! what it registers; see [`ToolProvenance`] below.
//!
//! # Core or Dynamic
//!
//! A `Core` entry owns its name: once it is registered, no later
//! registration may take that name, and a second registration under it
//! is refused. A `Dynamic` entry does not own its name — a later
//! `Core` registration may take it.
//!
//! The rule for which is which: a **shipped** tool is `Core` (the
//! model-facing `read` / `write` / `todo` / `web` / `shell` / `task`
//! bricks), and a tool whose lifetime is a **profile row's** is
//! `Dynamic` (the `scheduler` / `skill` / `search` bricks) — its name
//! is a row's to give away. The provenance is a fact about each
//! brick, so it is a parameter here rather than something this module
//! guesses; the rule it follows is stated once, above.

use std::sync::Arc;

use uva_core::registry::RegistryItem;
use uva_kernel::{Context, EffectHandle, PluginError};

use crate::{
    plugin::KEY_TOOLS,
    registry::{RegistrationToken, ToolEntry, ToolProvenance, ToolRegistry},
};

/// Register `entries` in the `tools` service and withdraw them when
/// the brick unloads.
///
/// The returned [`EffectHandle`] is the brick's withdrawal: dropping
/// it (which is what unloading a brick does) removes exactly the
/// entries this call filed — a foreign entry filed under the same
/// name stays.
///
/// # Refusal
///
/// A registration is refused when a `Core` entry already owns the
/// name. The refusal is loud and it names the tool: the mount fails
/// with a [`PluginError`] rather than mounting a brick whose tool
/// never appeared. When several entries are filed and a later one is
/// refused, the ones already filed are withdrawn first — the registry
/// is left as the brick found it.
///
/// # Errors
///
/// Fails when the `tools` service is not mounted, when the registry
/// refuses an entry, or when the kernel refuses to file the
/// withdrawal effect (in which case the entries are withdrawn anyway).
pub fn register_tool_entries(
    ctx: &Context,
    entries: Vec<ToolEntry>,
    provenance: ToolProvenance,
    label: impl Into<String>,
) -> Result<EffectHandle, PluginError> {
    let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
    let mut tokens: Vec<RegistrationToken> = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = RegistryItem::name(&entry).to_string();
        let registered = match provenance {
            ToolProvenance::Core => tools.register_core_entry_with_token(entry),
            ToolProvenance::Dynamic => tools.register_entry_with_token(entry),
        };
        let Some(token) = registered else {
            // The registry refuses a name a Core entry already owns.
            // Undo what this brick already filed, so a refused brick
            // leaves the registry as it found it.
            for token in &tokens {
                tools.unregister_by_token(token.clone());
            }
            return Err(PluginError::new(format!(
                "the `{KEY_TOOLS}` registry refused to register the \
                 `{name}` tool: a core tool already owns the name `{name}`"
            )));
        };
        tokens.push(token);
    }

    // The closure owns the tokens; `rollback` covers the case where
    // the framework refuses to file the effect at all.
    let rollback = tokens.clone();
    let registry = Arc::clone(&tools);
    let effect = match ctx.effect(label, move || {
        for token in &tokens {
            registry.unregister_by_token(token.clone());
        }
        Ok(())
    }) {
        Ok(effect) => effect,
        Err(error) => {
            for token in &rollback {
                tools.unregister_by_token(token.clone());
            }
            return Err(error);
        }
    };
    Ok(effect)
}
