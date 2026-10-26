//! Compose-tool-definitions pipeline contract tests.

use std::collections::HashSet;

use synthia_provider::ToolDefinition;
use synthia_test_support::FakeTool;
use synthia_tool::{ToolEntry, ToolRegistry, ToolRestriction};

fn empty_called() -> HashSet<String> {
    HashSet::new()
}

fn build_registry(count: usize) -> ToolRegistry {
    let registry = ToolRegistry::new();
    for i in 0..count {
        let name = format!("tool_{i}");
        registry.register_entry(ToolEntry::new(std::sync::Arc::new(
            FakeTool::new(&name, "ok"),
        )));
    }
    registry
}

#[test]
fn no_surface_no_restriction_projects_every_tool() {
    let registry = build_registry(5);
    let defs = crate::agent::re_act::projection::compose_tool_definitions(
        &registry.descriptors_cached(),
        None,
        None,
        &[],
        &empty_called(),
    );
    assert_eq!(defs.len(), 5);
}

#[test]
fn surface_policy_branch_receives_a_visible_set() {
    let registry = build_registry(5);
    let _policy: Option<&synthia_tool::ToolSurfacePolicy> = None;
    let defs = crate::agent::re_act::projection::compose_tool_definitions(
        &registry.descriptors_cached(),
        None,
        None,
        &[],
        &empty_called(),
    );
    // `None` policy keeps the projection flat: every tool's
    // name appears in the output.
    let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"tool_0".to_string()));
    assert!(names.contains(&"tool_4".to_string()));
}

#[test]
fn allow_restriction_yields_only_allowed_tools() {
    let registry = build_registry(5);
    let restriction = ToolRestriction::allow(vec!["tool_0".to_string()]);
    let defs = crate::agent::re_act::projection::compose_tool_definitions(
        &registry.descriptors_cached(),
        None,
        Some(&restriction),
        &[],
        &empty_called(),
    );
    let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0], "tool_0");
}

#[test]
fn deny_restriction_yields_every_tool_not_in_the_set() {
    let registry = build_registry(5);
    let restriction = ToolRestriction::deny(vec!["tool_2".to_string()]);
    let defs = crate::agent::re_act::projection::compose_tool_definitions(
        &registry.descriptors_cached(),
        None,
        Some(&restriction),
        &[],
        &empty_called(),
    );
    let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    assert_eq!(names.len(), 4);
    assert!(!names.contains(&"tool_2".to_string()));
}

#[test]
fn empty_delegator_list_returns_registry_only_definitions() {
    let registry = build_registry(3);
    let defs: Vec<ToolDefinition> =
        crate::agent::re_act::projection::compose_tool_definitions(
            &registry.descriptors_cached(),
            None,
            None,
            &[],
            &empty_called(),
        );
    assert_eq!(defs.len(), 3);
}

#[test]
fn empty_registry_returns_empty_definitions() {
    let registry = ToolRegistry::new();
    let defs = crate::agent::re_act::projection::compose_tool_definitions(
        &registry.descriptors_cached(),
        None,
        None,
        &[],
        &empty_called(),
    );
    assert!(defs.is_empty());
}
