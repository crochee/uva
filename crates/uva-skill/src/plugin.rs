//! The `skills` service: the skill registry and the `skill` tool as a
//! native kernel plugin.
//!
//! # Two contributions, both reversible
//!
//! [`SkillPlugin`] builds the file-backed [`SkillRegistry`] from its
//! [`SkillPluginConfig`] inside `apply`, provides it to the spine under
//! [`KEY_SKILLS`], and registers [`SkillTool`] into the injected `tools`
//! registry. The tool registration is token-scoped, and the lifecycle
//! lives in [`register_tool_entries`]:
//! the effect `apply` returns calls `unregister_by_token`, so unloading
//! the plugin removes exactly the entry it filed — never a same-named
//! entry another registrant owns.
//!
//! # The prompt section belongs to the consumer
//!
//! The registry is what the app-server's prompt builder reads to
//! contribute the `<available_skills>` block; the plugin deliberately
//! registers no `prompt/sections` effect. A kernel-owned section effect
//! that no builder read would be a promise the system does not keep, so
//! the wiring stays with the consumer that assembles the prompt.
//!
//! # Refusal is loud
//!
//! The `skill` tool's lifetime is the profile row's, so it registers
//! as a `Dynamic` entry: the name is the row's to give away. When a
//! Core tool already owns it, the registry refuses the registration —
//! [`uva_tool::ToolRegistry::register_entry_with_token`] answers
//! `None` — and `apply` turns that into a [`PluginError`] naming the
//! tool and the core owner instead of mounting a plugin whose
//! flagship tool silently never appeared.
//!
//! # The service value's shape
//!
//! [`SkillRegistry`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_SKILLS, Arc<SkillRegistry>)` and a consumer
//! writes `ctx.require::<SkillRegistry>(KEY_SKILLS)`. The registry is
//! built from [`SkillPluginConfig::workspace_root`] at mount time; the
//! boot passes configuration, not a pre-built object.
//!
//! # Lifecycle
//!
//! The manifest injects `tools`, so the kernel orders this plugin after
//! the registry provider and parks it while `tools` is absent. `apply`
//! returns both handles; unload revokes the tool entry first (reverse
//! order) and then the service, and a failed apply rolls the whole
//! group back.

use std::{path::PathBuf, sync::Arc};

use uva_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use uva_tool::{KEY_TOOLS, ToolEntry, ToolProvenance, register_tool_entries};

use crate::{SkillRegistry, builtin::skill_tool::SkillTool};

/// The service key the `skills` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SKILLS: &str = "skills";

/// The plugin id the `skills` manifest declares.
const SKILLS_ID: &str = "skills";

/// Label of the effect that withdraws the `skill` tool registration.
const SKILL_TOOL_EFFECT: &str = "skill-tool";

/// Configuration for the `skills` plugin.
///
/// The plugin discovers the workspace's skill files at mount time from
/// this root; nothing is built before the mount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillPluginConfig {
    /// Workspace root the file provider walks for project skills.
    pub workspace_root: PathBuf,
}

impl Default for SkillPluginConfig {
    fn default() -> Self {
        // The process working directory; a vanished cwd still leaves
        // a usable relative root.
        let workspace_root =
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self { workspace_root }
    }
}

/// The `skills` kernel plugin: provides [`KEY_SKILLS`] and registers
/// the `skill` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module docs](crate::plugin)
/// for the token scoping and the prompt-section boundary.
pub struct SkillPlugin {
    manifest: PluginManifest,
    config: SkillPluginConfig,
}

impl SkillPlugin {
    /// Build the plugin over `config`.
    ///
    /// `apply` discovers the workspace's skill files into a fresh
    /// [`SkillRegistry`] and publishes it as the `skills` service, then
    /// registers the `skill` tool.
    #[must_use]
    pub fn new(config: SkillPluginConfig) -> Self {
        let mut manifest =
            PluginManifest::new(SKILLS_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SKILLS.to_string());
        manifest.inject.push(KEY_TOOLS.to_string());
        Self { manifest, config }
    }
}

impl KernelPlugin for SkillPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let registry = Arc::new(SkillRegistry::discover_files(
            self.config.workspace_root.as_path(),
        ));
        let service = ctx.provide_arc(KEY_SKILLS, registry)?;
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(Arc::new(SkillTool::new()))],
            ToolProvenance::Dynamic,
            SKILL_TOOL_EFFECT,
        )?;
        Ok(vec![service, effect])
    }
}
