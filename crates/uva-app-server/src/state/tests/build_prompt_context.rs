//! Tests for `build_prompt_context` — the
//! `<workspace>/.agents/skills/` directory walker and the
//! live agent-registry snapshotter the assembler consumes.

use std::sync::Arc;

use uva::{
    kernel::{Context, MountHandle, Plugin as KernelPlugin, mount_all},
    skill::SkillPlugin,
    tool::{ToolRegistry, ToolRegistryPlugin},
};

use crate::state::{
    build_prompt_context,
    tests::support::{
        ScopedHome,
        empty_descriptor,
        empty_registry,
        write_skill_md,
    },
};

/// A kernel context with the `skills` service mounted over `root`'s
/// project + user skill roots — the same plugin the boot mounts.
async fn skills_context(root: &std::path::Path) -> (Context, MountHandle) {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    let plugins: Vec<Arc<dyn KernelPlugin>> = vec![
        Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools))),
        Arc::new(SkillPlugin::new(uva::skill::SkillPluginConfig {
            workspace_root: root.to_path_buf(),
        })),
    ];
    let mounted = mount_all(&ctx, &plugins).expect("mount `tools` + `skills`");
    (ctx, mounted)
}

#[tokio::test]
async fn build_prompt_context_with_no_skills_dir_omits_skills_block() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(!out.contains("<available_skills>"));
    assert!(!out.contains("<available_agents>"));
    assert!(out.contains("<identity>"));
}

#[tokio::test]
async fn build_prompt_context_sorts_skills_alphabetically() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    // Insert in REVERSE alphabetical order.
    write_skill_md(
        dir.path(),
        "zoo",
        "---\nname: zoo\ndescription: Z\n---\n\nbody\n",
    );
    write_skill_md(
        dir.path(),
        "apple",
        "---\nname: apple\ndescription: A\n---\n\nbody\n",
    );
    write_skill_md(
        dir.path(),
        "mango",
        "---\nname: mango\ndescription: M\n---\n\nbody\n",
    );
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    let apple = out.find("<name>apple</name>").expect("apple in prompt");
    let mango = out.find("<name>mango</name>").expect("mango in prompt");
    let zoo = out.find("<name>zoo</name>").expect("zoo in prompt");
    assert!(apple < mango && mango < zoo);
}

#[tokio::test]
async fn build_prompt_context_drops_tools_from_assembled_text() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    // Tools are deliberately NOT carried by the prompt
    // context — they travel on the completion-request
    // `tools` channel.
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(!out.contains("<available_tools>"));
    assert!(!out.contains("Use only these tools"));
}

/// `discover_skills` follows the opencode / Anthropic
/// convention: malformed SKILL.md files are silently
/// dropped, not placeheld.
#[tokio::test]
async fn build_prompt_context_drops_malformed_skill_md() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    write_skill_md(dir.path(), "broken", "no delimiters\n");
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(
        !out.contains("<name>broken</name>"),
        "malformed skill must be dropped, not advertised; got:\n{out}"
    );
}

#[tokio::test]
async fn build_prompt_context_with_valid_frontmatter_uses_description() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    write_skill_md(
        dir.path(),
        "good",
        "---\nname: good\ndescription: Use this for X\n---\n\nbody\n",
    );
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(
        out.contains("<description>Use this for X</description>"),
        "description must surface in the XML envelope; got:\n{out}"
    );
}

/// Anthropic Agent Skills makes `description` optional —
/// the loader falls back to the first non-empty body line.
#[tokio::test]
async fn build_prompt_context_falls_back_to_body_when_description_missing() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    write_skill_md(
        dir.path(),
        "nodoc",
        "---\nname: nodoc\n---\n\n# No doc skill\n\nBody.\n",
    );
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(out.contains("<name>nodoc</name>"));
    assert!(
        out.contains("<description>No doc skill</description>"),
        "missing description must fall back to first body line; \
         got:\n{out}"
    );
}

#[tokio::test]
async fn build_prompt_context_skips_dirs_without_skill_md() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(
        dir.path().join(".agents").join("skills").join("incomplete"),
    )
    .unwrap();
    write_skill_md(
        dir.path(),
        "valid",
        "---\nname: valid\ndescription: V\n---\n\nbody\n",
    );
    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(out.contains("<name>valid</name>"));
    assert!(!out.contains("<name>incomplete</name>"));
}

/// User-level skills at `$HOME/.agents/skills/` MUST
/// surface in the prompt manifest (the global layer of
/// the two-layer layout). Project skills win on name
/// collisions.
#[tokio::test]
async fn build_prompt_context_includes_user_skills_from_home() {
    let home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();

    let user_skill_dir =
        home.path().join(".agents").join("skills").join("user-only");
    std::fs::create_dir_all(&user_skill_dir).unwrap();
    std::fs::write(
        user_skill_dir.join("SKILL.md"),
        "---\nname: user-only\ndescription: From the global layer.\n---\n\nBody.\n",
    )
    .unwrap();

    write_skill_md(
        dir.path(),
        "project-only",
        "---\nname: project-only\ndescription: Project.\n---\n\nBody.\n",
    );

    let pc = {
        let (ctx, _mounted) = skills_context(dir.path()).await;
        build_prompt_context(&ctx, &empty_registry(), "agent").await
    };
    let out = pc.assemble(&empty_descriptor());
    assert!(
        out.contains("<name>user-only</name>"),
        "user-level skill must surface; got:\n{out}"
    );
    assert!(
        out.contains("<name>project-only</name>"),
        "project skill must surface; got:\n{out}"
    );
}

/// The `<available_skills>` block is a consumer of the `skills` service,
/// not a direct directory walk: unloading `SkillPlugin` removes it.
#[tokio::test]
async fn unloading_skill_plugin_removes_the_available_skills_block() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    write_skill_md(
        dir.path(),
        "kept",
        "---\nname: kept\ndescription: K\n---\n\nbody\n",
    );

    let (ctx, mut mounted) = skills_context(dir.path()).await;
    let before = build_prompt_context(&ctx, &empty_registry(), "agent").await;
    assert!(
        before
            .assemble(&empty_descriptor())
            .contains("<name>kept</name>"),
        "a mounted `skills` service contributes the block"
    );

    assert!(mounted.dispose_plugin("skills"));
    let after = build_prompt_context(&ctx, &empty_registry(), "agent").await;
    assert!(
        !after
            .assemble(&empty_descriptor())
            .contains("<available_skills>"),
        "the block disappears with the service"
    );
}
