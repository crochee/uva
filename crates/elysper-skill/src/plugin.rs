//! The `skills` service: the skill registry and the `skill` tool as a
//! native kernel plugin.
//!
//! # Two contributions, both reversible
//!
//! [`SkillPlugin`] builds the file-backed [`SkillRegistry`] from its
//! [`SkillPluginConfig`] inside `apply`, provides it to the spine under
//! [`KEY_SKILLS`], and registers [`SkillTool`] into the injected `tools`
//! registry. The tool registration is token-scoped: `apply` keeps the
//! [`RegistrationToken`](elysper_tool::RegistrationToken) that
//! `register_entry_with_token` returned and returns a
//! [`ctx.effect`](elysper_kernel::Context::effect) that calls
//! `unregister_by_token`, so unloading the plugin removes exactly the
//! entry it filed — never a same-named entry another registrant owns.
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
//! The `skill` name is a shipped tool: when a Core tool already owns
//! it, [`ToolRegistry::register_entry_with_token`] refuses the
//! registration. `apply` turns that `None` into a [`PluginError`]
//! naming the tool and the core owner instead of mounting a plugin
//! whose flagship tool silently never appeared.
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

use elysper_core::enable::EnableChecks;
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use elysper_tool::{KEY_TOOLS, ToolEntry, ToolRegistry};
use serde_json::Value;

use crate::{
    SkillRegistry,
    builtin::skill_tool::{SKILL_TOOL_NAME, SkillTool},
};

/// The service key the `skills` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SKILLS: &str = "skills";

/// The service key the skill enablement judge is registered under.
///
/// A profile row may write
/// `enabled = { service = "skill-checks", check = "is_enabled", arg = "<skill>" }`
/// and the host asks this judge whether the skill's
/// `metadata.enabled` frontmatter field is on.
pub const KEY_SKILL_CHECKS: &str = "skill-checks";

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
/// Mount it with [`elysper_kernel::mount_all`] in the same group as (or
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
        manifest.provides.push(KEY_SKILL_CHECKS.to_string());
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
        let judge: Arc<dyn EnableChecks> =
            Arc::new(SkillChecks::discover(&self.config.workspace_root));
        let checks = ctx.provide(KEY_SKILL_CHECKS, judge)?;
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        let entry = ToolEntry::new(Arc::new(SkillTool::new()));
        let Some(token) = tools.register_entry_with_token(entry) else {
            return Err(PluginError::new(format!(
                "the `{KEY_TOOLS}` registry refused to register the \
                 `{SKILL_TOOL_NAME}` tool: a core tool already owns the \
                 name `{SKILL_TOOL_NAME}`"
            )));
        };
        // The closure owns one token clone; `rollback` covers the case
        // where the framework refuses to file the effect at all.
        let rollback = token.clone();
        let tool_registry = Arc::clone(&tools);
        let unregister = ctx.effect(SKILL_TOOL_EFFECT, move || {
            tool_registry.unregister_by_token(token);
            Ok(())
        });
        let effect = match unregister {
            Ok(effect) => effect,
            Err(error) => {
                tools.unregister_by_token(rollback);
                return Err(error);
            }
        };
        Ok(vec![service, checks, effect])
    }
}

/// Answers `is_enabled` from a skill's `metadata.enabled` frontmatter.
///
/// Unknown subjects and unreadable frontmatter answer `None`, which the
/// host reads as fail-closed: a delegated enablement decision the judge
/// cannot make leaves the row out.
struct SkillChecks {
    skills: Vec<crate::skill::Skill>,
}

impl SkillChecks {
    fn discover(workspace_root: &std::path::Path) -> Self {
        Self {
            skills: crate::discovery::discover_skills(workspace_root),
        }
    }
}

impl EnableChecks for SkillChecks {
    fn check(&self, method: &str, arg: &Value) -> Option<bool> {
        if method != "is_enabled" {
            return None;
        }
        let name = arg.as_str()?;
        let skill = self.skills.iter().find(|skill| skill.name == name)?;
        let metadata =
            crate::loader::SkillLoader::parse_frontmatter(&skill.location)
                .ok()?;
        match metadata.metadata.get("enabled") {
            Some(value) => value.as_bool(),
            None => Some(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper_core::enable::EnableChecks;
    use elysper_kernel::{Context, mount_all};
    use elysper_tool::{ToolRegistry, ToolRegistryPlugin};
    use serde_json::json;

    use super::*;

    fn write_skill(root: &std::path::Path, name: &str, enabled: Option<bool>) {
        let dir = root.join(".agents").join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let metadata = match enabled {
            Some(value) => format!("metadata:\n  enabled: {value}\n"),
            None => String::new(),
        };
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: probe\n{metadata}---\n\nbody\n"
            ),
        )
        .unwrap();
    }

    /// The judge answers the three states the host distinguishes:
    /// enabled, disabled, and unknown (which the host reads as
    /// fail-closed).
    #[test]
    fn the_skill_judge_reads_frontmatter_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_skill(root, "probe-on", None);
        write_skill(root, "probe-off", Some(false));
        write_skill(root, "probe-also-on", Some(true));

        let ctx = Context::new();
        let plugins: Vec<Arc<dyn KernelPlugin>> = vec![
            Arc::new(ToolRegistryPlugin::new(Arc::new(ToolRegistry::new()))),
            Arc::new(SkillPlugin::new(SkillPluginConfig {
                workspace_root: root.to_path_buf(),
            })),
        ];
        let _mounted = mount_all(&ctx, &plugins).expect("mount");
        let judge = ctx
            .get::<Arc<dyn EnableChecks>>(KEY_SKILL_CHECKS)
            .expect("lookup")
            .expect("the skills row publishes the judge");

        assert_eq!(
            judge.check("is_enabled", &json!("probe-on")),
            Some(true),
            "absent metadata.enabled means enabled"
        );
        assert_eq!(judge.check("is_enabled", &json!("probe-off")), Some(false));
        assert_eq!(
            judge.check("is_enabled", &json!("probe-also-on")),
            Some(true)
        );
        assert_eq!(
            judge.check("is_enabled", &json!("no-such-skill")),
            None,
            "an unknown subject is unanswered, never enabled"
        );
        assert_eq!(judge.check("other", &json!("probe-on")), None);
    }
}
