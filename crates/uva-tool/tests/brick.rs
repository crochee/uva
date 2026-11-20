//! The brick lifecycle: register tool entries, refuse loudly, roll back,
//! withdraw exactly what was filed.
//!
//! The lifecycle used to be spelled out by every brick for itself, and
//! the copies drifted — one refused with a message that dropped the
//! tool's name, and the rollback path (a brick that files several
//! entries and is refused halfway) had no test at all. This file is
//! that path's test surface: the bricks that use it are thin, and what
//! they do beyond it (their manifest, their tool, their extra
//! injections) is tested in their own crate.

use std::sync::Arc;

use uva_kernel::{Context, MountHandle, Plugin as KernelPlugin, mount_all};
use uva_test_support::FakeTool;
use uva_tool::{
    ToolEntry,
    ToolProvenance,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
    register_tool_entries,
};

/// A registry published as the `tools` service, and the context that
/// resolves it.
///
/// The mount handle is returned too: dropping it unloads the plugin
/// and withdraws the service, so a test that wants the service keeps
/// it alive for as long as it does.
fn mounted() -> (Context, Arc<ToolRegistry>, MountHandle) {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let handle = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `tool-registry`");
    (ctx, registry, handle)
}

/// One entry for a fake tool.
fn entry(name: &str) -> ToolEntry {
    ToolEntry::new(Arc::new(FakeTool::new(name, "ok")))
}

#[test]
fn files_the_entries_and_withdraws_exactly_them() {
    let (ctx, registry, _mount) = mounted();

    let effect = register_tool_entries(
        &ctx,
        vec![entry("alpha"), entry("beta")],
        ToolProvenance::Dynamic,
        "test-brick",
    )
    .expect("the brick files its tools");

    let service = ctx.require::<ToolRegistry>(KEY_TOOLS).expect("`tools`");
    assert!(service.contains("alpha"));
    assert!(service.contains("beta"));
    assert_eq!(registry.tool_count(), 2);

    // A foreign entry filed under the same name is not the brick's to
    // withdraw: the token names exactly what the brick filed.
    assert!(registry.register_entry(entry("alpha")));
    assert_eq!(
        registry.tool_count(),
        2,
        "the foreign entry shares the name it was filed under"
    );

    drop(effect);
    assert!(!service.contains("beta"), "the brick's own entry is gone");
    assert!(
        service.contains("alpha"),
        "the foreign entry under the same name survives"
    );
    assert_eq!(
        registry.tool_count(),
        1,
        "only the foreign entry under the same name remains"
    );
}

#[test]
fn refuses_loudly_and_names_the_tool_and_the_core_owner() {
    let (ctx, registry, _mount) = mounted();
    registry.register_core_entry(entry("taken"));

    let error = register_tool_entries(
        &ctx,
        vec![entry("taken")],
        ToolProvenance::Core,
        "test-brick",
    )
    .expect_err("a name a Core entry owns is refused");

    let message = error.to_string();
    assert!(
        message.contains("taken") && message.contains("core"),
        "the error must name the tool and the core owner: {message}"
    );
    assert_eq!(
        registry.tool_count(),
        1,
        "only the fixture's core entry remains"
    );
}

#[test]
fn rolls_back_what_it_already_filed_when_a_later_entry_is_refused() {
    let (ctx, registry, _mount) = mounted();
    registry.register_core_entry(entry("taken"));

    let error = register_tool_entries(
        &ctx,
        vec![entry("alpha"), entry("taken"), entry("gamma")],
        ToolProvenance::Core,
        "test-brick",
    )
    .expect_err("the second entry is refused");
    assert!(error.to_string().contains("taken"));

    let service = ctx.require::<ToolRegistry>(KEY_TOOLS).expect("`tools`");
    assert!(
        !service.contains("alpha"),
        "the entry filed before the refusal is withdrawn"
    );
    assert!(
        !service.contains("gamma"),
        "the entry after the refusal was never filed"
    );
    assert_eq!(
        registry.tool_count(),
        1,
        "only the fixture's core entry remains"
    );
}

#[test]
fn a_dynamic_entry_does_not_own_its_name() {
    let (ctx, registry, _mount) = mounted();

    let effect = register_tool_entries(
        &ctx,
        vec![entry("row-scoped")],
        ToolProvenance::Dynamic,
        "test-brick",
    )
    .expect("the brick files its tool");

    // A Dynamic entry is a row's to give away: a later Core
    // registration may take the name.
    assert!(
        registry.register_core_entry(entry("row-scoped")),
        "a Core registration may take a Dynamic entry's name"
    );

    drop(effect);
    let service = ctx.require::<ToolRegistry>(KEY_TOOLS).expect("`tools`");
    assert!(
        service.contains("row-scoped"),
        "the Core entry that took the name survives the brick"
    );
}
