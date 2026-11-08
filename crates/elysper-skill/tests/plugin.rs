//! The `skills` service as a native kernel plugin.
//!
//! The plugin provides the registry and registers the `skill` tool as a
//! token-scoped entry. The prompt builder's skills section is the
//! app-server's read of this registry (task 10 wires it); the plugin
//! claims no `prompt/sections` effect it does not own.
//!
//! `tools` is mounted on its own first, so unloading `skills` exercises
//! the cascade rule: only the skill plugin's own service and tool entry
//! disappear, while the sibling `tools` service stays live.

use std::sync::Arc;

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
    InMemorySkillProvider,
    Skill,
    SkillRegistry,
    SkillTool,
    builtin::skill_tool::{SKILL_TOOL_NAME, register_skill_tool},
    plugin::{KEY_SKILLS, SkillPlugin},
};
use elysper_tool::{
    ToolEntry,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};

fn mount_tools(ctx: &Context, tools: &Arc<ToolRegistry>) -> MountHandle {
    mount_all(
        ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `tool-registry`")
}

fn mount_skill(
    ctx: &Context,
    skills: &Arc<SkillRegistry>,
) -> Result<MountHandle, MountError> {
    mount_all(
        ctx,
        &[Arc::new(SkillPlugin::new(Arc::clone(skills)))
            as Arc<dyn KernelPlugin>],
    )
}

fn in_memory_skills() -> Arc<SkillRegistry> {
    let skill = Skill {
        name: "example".to_string(),
        description: Some("an example skill".to_string()),
        location: std::path::PathBuf::from("/tmp/example/SKILL.md"),
        content: "# example".to_string(),
    };
    let mut registry = SkillRegistry::new();
    registry.register(InMemorySkillProvider::new(vec![skill]));
    Arc::new(registry)
}

#[tokio::test]
async fn provides_the_registry_and_registers_the_skill_tool() {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    let skills = in_memory_skills();
    let tools_mount = mount_tools(&ctx, &tools);

    let skills_mount =
        mount_skill(&ctx, &skills).expect("mount `skills` over `tools`");

    let service = ctx
        .require::<SkillRegistry>(KEY_SKILLS)
        .expect("`skills` resolves");
    assert!(
        Arc::ptr_eq(&service, &skills),
        "the service must be the registry the plugin was built with"
    );
    let report = service.collect().await;
    assert_eq!(report.skills.len(), 1);
    assert_eq!(report.skills[0].name, "example");
    assert!(
        !report.skills[0]
            .description
            .as_deref()
            .unwrap_or_default()
            .is_empty(),
        "the provider's description is visible through the service"
    );

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
    let survivor = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("the sibling `tools` service survives a skill unload");
    assert!(
        Arc::ptr_eq(&survivor, &tools),
        "the surviving `tools` service is the same registry"
    );

    drop(tools_mount);
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "the `tools` mount still withdraws its own service"
    );
}

#[tokio::test]
async fn unload_removes_only_the_token_it_registered() {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    tools.register_entry(ToolEntry::new(Arc::new(SkillTool::new())));
    assert_eq!(tools.name_occupants(SKILL_TOOL_NAME), 1);
    let skills = in_memory_skills();
    let tools_mount = mount_tools(&ctx, &tools);

    let skills_mount =
        mount_skill(&ctx, &skills).expect("mount `skills` over `tools`");
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
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    assert!(
        register_skill_tool(&tools),
        "the fixture core-registers `skill`"
    );
    let skills = in_memory_skills();
    let tools_mount = mount_tools(&ctx, &tools);

    let error = mount_skill(&ctx, &skills)
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
    let tools = Arc::new(ToolRegistry::new());
    let tools_mount = mount_tools(&ctx, &tools);

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
    let plugin = SkillPlugin::new(in_memory_skills());
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "skills");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SKILLS.to_string()]);
    assert_eq!(KEY_SKILLS, "skills");
    assert_eq!(manifest.inject, vec![KEY_TOOLS.to_string()]);
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
