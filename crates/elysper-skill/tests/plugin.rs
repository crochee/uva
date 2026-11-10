//! The `skills` service as a native kernel plugin.
//!
//! The plugin discovers its registry from the configured workspace root
//! at mount time and registers the `skill` tool as a token-scoped entry.
//! The prompt builder's skills section is the app-server's read of this
//! registry (task 10 wires it); the plugin claims no `prompt/sections`
//! effect it does not own.
//!
//! `tools` is mounted on its own first, so unloading `skills` exercises
//! the cascade rule: only the skill plugin's own service and tool entry
//! disappear, while the sibling `tools` service stays live.

use std::{ffi::OsString, path::Path, sync::Arc};

use elysper_kernel::{
    Context,
    MountError,
    MountHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_skill::{
    SkillRegistry,
    SkillTool,
    builtin::skill_tool::{SKILL_TOOL_NAME, register_skill_tool},
    plugin::{KEY_SKILL_CHECKS, KEY_SKILLS, SkillPlugin, SkillPluginConfig},
};
use elysper_tool::{
    ToolEntry,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};

// Pin `$HOME` to a temp directory so discovery sees only the fixture's
// skills instead of the developer's installed set. Only the discovery
// test reads `$HOME`, and the guard restores it on drop (panic or not).
struct ScopedHome {
    previous: Option<OsString>,
    _dir: tempfile::TempDir,
}

impl ScopedHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HOME");
        // SAFETY: no other test in this binary reads `$HOME`
        // concurrently; this is the only discovery test, and the guard
        // restores the previous value on drop.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        Self {
            previous,
            _dir: dir,
        }
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        // SAFETY: see `ScopedHome::new`; the previous value is restored
        // exactly once, from the test thread.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var("HOME", previous),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

fn mount_tools(ctx: &Context) -> MountHandle {
    mount_all(
        ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::new(ToolRegistry::new())))
                as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry`")
}

fn mount_skill(
    ctx: &Context,
    config: SkillPluginConfig,
) -> Result<MountHandle, MountError> {
    mount_all(
        ctx,
        &[Arc::new(SkillPlugin::new(config)) as Arc<dyn KernelPlugin>],
    )
}

fn workspace_config(root: &Path) -> SkillPluginConfig {
    SkillPluginConfig {
        workspace_root: root.to_path_buf(),
    }
}

// Write `<root>/.agents/skills/example/SKILL.md`.
fn write_example_skill(root: &Path) {
    let dir = root.join(".agents").join("skills").join("example");
    std::fs::create_dir_all(&dir).expect("create skill directory");
    std::fs::write(
        dir.join("SKILL.md"),
        "---\n\
         name: example\n\
         description: an example skill\n\
         ---\n\
         \n\
         # example\n",
    )
    .expect("write SKILL.md");
}

#[tokio::test]
async fn provides_the_discovered_registry_and_registers_the_skill_tool() {
    let _home = ScopedHome::new();
    let root = tempfile::tempdir().expect("temp workspace");
    write_example_skill(root.path());

    let ctx = Context::new();
    let tools_mount = mount_tools(&ctx);
    let skills_mount = mount_skill(&ctx, workspace_config(root.path()))
        .expect("mount `skills` over `tools`");

    let service = ctx
        .require::<SkillRegistry>(KEY_SKILLS)
        .expect("`skills` resolves");
    let report = service.collect().await;
    assert_eq!(
        report.skills.len(),
        1,
        "the configured workspace's skill is discovered"
    );
    assert_eq!(report.skills[0].name, "example");
    assert_eq!(
        report.skills[0].description.as_deref(),
        Some("an example skill"),
        "the provider's description is visible through the service"
    );
    assert!(
        report.skills[0].content.contains("# example"),
        "the skill body is visible through the service"
    );

    let tools = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    assert!(tools.contains(SKILL_TOOL_NAME), "`skill` is registered");
    assert_eq!(tools.name_occupants(SKILL_TOOL_NAME), 1);

    drop(skills_mount);
    assert!(
        ctx.require::<SkillRegistry>(KEY_SKILLS).is_err(),
        "unload must withdraw `skills`"
    );
    assert!(
        !tools.contains(SKILL_TOOL_NAME),
        "unload must remove the `skill` tool"
    );
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_ok(),
        "the sibling `tools` service survives a skill unload"
    );

    drop(tools_mount);
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "the `tools` mount still withdraws its own service"
    );
}

#[tokio::test]
async fn unload_removes_only_the_token_it_registered() {
    let root = tempfile::tempdir().expect("temp workspace");
    let ctx = Context::new();
    let tools_mount = mount_tools(&ctx);
    let tools = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    tools.register_entry(ToolEntry::new(Arc::new(SkillTool::new())));
    assert_eq!(tools.name_occupants(SKILL_TOOL_NAME), 1);

    let skills_mount = mount_skill(&ctx, workspace_config(root.path()))
        .expect("mount `skills` over `tools`");
    assert_eq!(
        tools.name_occupants(SKILL_TOOL_NAME),
        2,
        "the plugin's token files a second entry under the same name"
    );

    drop(skills_mount);
    assert!(
        tools.contains(SKILL_TOOL_NAME),
        "the foreign entry stays registered"
    );
    assert_eq!(
        tools.name_occupants(SKILL_TOOL_NAME),
        1,
        "unregister_by_token removes exactly the plugin's entry"
    );

    drop(tools_mount);
}

#[test]
fn mount_fails_loudly_when_a_core_tool_owns_the_name() {
    let root = tempfile::tempdir().expect("temp workspace");
    let ctx = Context::new();
    let tools_mount = mount_tools(&ctx);
    let tools = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    assert!(
        register_skill_tool(&tools),
        "the fixture core-registers `skill`"
    );

    let error = mount_skill(&ctx, workspace_config(root.path()))
        .expect_err("the mount must fail, not silently skip `skill`");
    let MountError::ApplyFailed { plugin_id, source } = error else {
        panic!("expected ApplyFailed, got {error:?}");
    };
    assert_eq!(plugin_id, "skills");
    let message = source.to_string();
    assert!(
        message.contains(SKILL_TOOL_NAME) && message.contains("core"),
        "the error must name the tool and the core owner: {message}"
    );

    assert!(
        ctx.require::<SkillRegistry>(KEY_SKILLS).is_err(),
        "the failed apply must not leave `skills` behind"
    );
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_ok(),
        "a failed `skills` mount does not touch the already-mounted `tools`"
    );
    assert_eq!(
        tools.name_occupants(SKILL_TOOL_NAME),
        1,
        "the core entry is untouched"
    );

    drop(tools_mount);
}

#[test]
fn unmounted_skills_leave_the_tool_registry_intact() {
    let ctx = Context::new();
    let tools_mount = mount_tools(&ctx);
    let tools = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");

    assert!(
        ctx.require::<SkillRegistry>(KEY_SKILLS).is_err(),
        "without the `skills` plugin the service does not exist"
    );
    assert!(
        !tools.contains(SKILL_TOOL_NAME),
        "without the plugin the `skill` tool is never registered"
    );
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_ok(),
        "the rest of the system runs without the optional capability"
    );

    drop(tools_mount);
}

#[test]
fn manifest_matches_provided_key_and_injects_tools() {
    let plugin = SkillPlugin::new(SkillPluginConfig::default());
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "skills");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(
        manifest.provides,
        vec![KEY_SKILLS.to_string(), KEY_SKILL_CHECKS.to_string()]
    );
    assert_eq!(KEY_SKILLS, "skills");
    assert_eq!(KEY_SKILL_CHECKS, "skill-checks");
    assert_eq!(manifest.inject, vec![KEY_TOOLS.to_string()]);
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}

#[test]
fn default_config_anchors_at_the_current_directory() {
    let expected = std::env::current_dir().expect("current directory");
    assert_eq!(SkillPluginConfig::default().workspace_root, expected);
}
