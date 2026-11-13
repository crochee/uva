use std::sync::Arc;

use uva_provider::{Content, ContentPart, Role, ToolResult, ToolUse};

use super::*;
use crate::registry::ToolCategory;

fn descriptor(name: &str, exposure: ToolExposure) -> ToolDescriptor {
    ToolDescriptor {
        name: name.to_string(),
        description: format!("the {name} tool"),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        }),
        category: ToolCategory::Utility,
        is_hidden: false,
        exposure,
        annotations: None,
    }
}

fn names(defs: &[ToolDefinition]) -> Vec<&str> {
    defs.iter().map(|d| d.name.as_str()).collect()
}

/// Assistant message asking for `names`, which is how a call shows
/// up in the transcript.
fn assistant_call(names: &[&str]) -> Message {
    let parts: Vec<ContentPart> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            ContentPart::ToolUse(ToolUse {
                id: format!("call-{i}"),
                name: (*name).to_string(),
                input: serde_json::json!({}),
            })
        })
        .collect();
    Message::new(Role::Assistant, Content::parts(parts))
}

/// Tool-role message whose `ToolResult` carries the tool name.
fn tool_result(name: Option<&str>) -> Message {
    let mut result = ToolResult::new("call-0", "ok");
    result.tool_name = name.map(str::to_string);
    Message::new(Role::Tool, Content::Single(ContentPart::ToolResult(result)))
}

#[test]
fn direct_tool_definition_is_exactly_name_description_and_schema() {
    let descs = vec![descriptor("read_file", ToolExposure::Direct)];
    let defs = project_tool_definitions(&descs, &HashSet::new(), None);
    assert_eq!(
        serde_json::to_value(&defs).unwrap(),
        serde_json::json!([{
            "name": "read_file",
            "description": "the read_file tool",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        }]),
        "Direct must advertise the tool verbatim"
    );
}

#[test]
fn deferred_tool_is_promoted_by_a_called_name() {
    let descs = vec![descriptor("query_db", ToolExposure::Deferred)];
    let before = project_tool_definitions(&descs, &HashSet::new(), None);
    assert_eq!(names(&before), vec!["query_db"]);
    assert_eq!(
        before[0].description, "the query_db tool",
        "the description is what lets the model call a deferred tool"
    );
    assert_eq!(
        before[0].input_schema,
        serde_json::json!({
            "type": "object",
            "additionalProperties": true,
        }),
        "an uncalled Deferred tool must not leak its real schema"
    );

    let called: HashSet<String> =
        ["query_db".to_string()].into_iter().collect();
    let after = project_tool_definitions(&descs, &called, None);
    assert_eq!(
        after[0].input_schema, descs[0].parameters,
        "after its first call the Deferred tool carries its full schema"
    );
}

#[test]
fn called_name_promotes_only_the_tool_it_names() {
    let descs = vec![
        descriptor("alpha", ToolExposure::Deferred),
        descriptor("beta", ToolExposure::Deferred),
    ];
    let called: HashSet<String> = ["alpha".to_string()].into_iter().collect();
    let defs = project_tool_definitions(&descs, &called, None);
    assert_eq!(defs[0].input_schema, descs[0].parameters);
    assert_eq!(
        defs[1].input_schema,
        serde_json::json!({
            "type": "object",
            "additionalProperties": true,
        })
    );
}

#[test]
fn hidden_tools_are_absent_from_every_projection() {
    let mut flagged = descriptor("secret", ToolExposure::Direct);
    flagged.is_hidden = true;
    let descs = vec![
        flagged,
        descriptor("internal", ToolExposure::Hidden),
        descriptor("visible", ToolExposure::Direct),
    ];
    // Even a called name must not resurrect a hidden tool.
    let called: HashSet<String> =
        ["secret".to_string(), "internal".to_string()]
            .into_iter()
            .collect();
    let defs = project_tool_definitions(&descs, &called, None);
    assert_eq!(names(&defs), vec!["visible"]);
}

#[test]
fn called_tool_names_reads_tool_use_and_tool_result_names() {
    let messages = vec![
        Message::user("hello"),
        assistant_call(&["read_file", "query_db"]),
        tool_result(Some("query_db")),
        tool_result(None),
        Message::assistant("thinking"),
    ];
    let called = called_tool_names(&messages);
    let mut sorted: Vec<&str> = called.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    assert_eq!(sorted, vec!["query_db", "read_file"]);
}

#[test]
fn called_tool_names_is_empty_for_a_transcript_without_calls() {
    let messages = vec![Message::user("hi"), Message::assistant("hello")];
    assert!(called_tool_names(&messages).is_empty());
}

#[test]
fn visible_filter_narrows_the_projected_list() {
    let descs = vec![
        descriptor("alpha", ToolExposure::Direct),
        descriptor("beta", ToolExposure::Direct),
        descriptor("gamma", ToolExposure::Direct),
    ];
    let visible: HashSet<String> = ["alpha".to_string(), "gamma".to_string()]
        .into_iter()
        .collect();
    let defs =
        project_tool_definitions(&descs, &HashSet::new(), Some(&visible));
    assert_eq!(names(&defs), vec!["alpha", "gamma"]);

    // The filter is an allow-list: it also withholds a Deferred
    // tool that the transcript has already promoted.
    let descs = vec![
        descriptor("alpha", ToolExposure::Direct),
        descriptor("beta", ToolExposure::Deferred),
    ];
    let called: HashSet<String> = ["beta".to_string()].into_iter().collect();
    let visible: HashSet<String> = ["alpha".to_string()].into_iter().collect();
    let defs = project_tool_definitions(&descs, &called, Some(&visible));
    assert_eq!(names(&defs), vec!["alpha"]);
}

/// The regression contract end to end: a real registry whose tools
/// are all `Direct` and not hidden produces exactly the definitions
/// the pre-projection call sites built from `list(None)` +
/// `Tool::parameters()` — even when the transcript has already
/// mentioned them. Element *contents* must match byte-for-byte;
/// the projection only reorders them (sorted by name, per
/// `ToolRegistry::descriptors`), which is why both sides are sorted
/// before comparing.
#[tokio::test]
async fn registry_with_only_direct_tools_matches_the_legacy_list() {
    use async_trait::async_trait;

    use crate::{Tool, ToolEntry, ToolOutput, ToolRegistry, types::Context};

    struct SchemaTool(&'static str);

    #[async_trait]
    impl Tool for SchemaTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "a tool with a real schema"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {"arg": {"type": "string"}},
                "required": ["arg"],
            })
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _context: &Context,
        ) -> ToolOutput {
            ToolOutput::text("ok")
        }
    }

    let registry = ToolRegistry::new();
    for name in ["beta", "alpha"] {
        registry.register_entry(ToolEntry::new(Arc::new(SchemaTool(name))));
    }

    let mut legacy: Vec<serde_json::Value> =
        uva_core::registry::Registry::list(&registry, None)
            .await
            .expect("in-memory registry lists")
            .into_iter()
            .map(|entry| {
                let tool = entry.tool_instance();
                serde_json::to_value(ToolDefinition::new(
                    tool.name(),
                    tool.description(),
                    tool.parameters(),
                ))
                .expect("definitions are JSON values")
            })
            .collect();
    let mut projected: Vec<serde_json::Value> = project_tool_definitions(
        &registry.descriptors(),
        &called_tool_names(&[assistant_call(&["alpha", "beta"])]),
        None,
    )
    .iter()
    .map(|def| serde_json::to_value(def).expect("definitions are JSON values"))
    .collect();
    let by_name = |a: &serde_json::Value, b: &serde_json::Value| {
        a["name"].as_str().cmp(&b["name"].as_str())
    };
    legacy.sort_by(by_name);
    projected.sort_by(by_name);

    assert_eq!(
        projected, legacy,
        "a Direct-only registry must project to the legacy definitions"
    );
    assert_eq!(projected.len(), 2);
}

// -- ToolSurfacePolicy -------------------------------------------

/// A registry with `names` registered in the given order, each
/// carrying a per-tool schema so a projection mix-up is visible.
fn registry_with(names: &[&str]) -> crate::ToolRegistry {
    let registry = crate::ToolRegistry::new();
    for name in names {
        registry.register_entry(crate::ToolEntry::dynamic(
            (*name).to_string(),
            format!("the {name} tool"),
            serde_json::json!({
                "type": "object",
                "properties": {"for": {"const": name}},
            }),
        ));
    }
    registry
}

fn policy(
    groups: &[(&str, &[&str])],
    active: &[&str],
    max_visible: Option<usize>,
) -> ToolSurfacePolicy {
    ToolSurfacePolicy {
        max_visible,
        groups: groups
            .iter()
            .map(|(group, tools)| {
                (
                    (*group).to_string(),
                    tools.iter().map(|t| (*t).to_string()).collect(),
                )
            })
            .collect(),
        active_groups: active.iter().map(|g| (*g).to_string()).collect(),
    }
}

fn sorted_names(names: &HashSet<String>) -> Vec<&str> {
    let mut out: Vec<&str> = names.iter().map(String::as_str).collect();
    out.sort_unstable();
    out
}

/// `write_to_registry` writes the documented verdict — active
/// group members `Direct`, inactive members `Hidden` exposure,
/// groupless tools untouched — and the projection then advertises
/// exactly the policy's visible set.
#[test]
fn policy_write_to_registry_writes_the_group_verdict_the_projection_reads() {
    let registry = registry_with(&["read", "write", "shell", "free"]);
    let policy = policy(
        &[("files", &["read", "write"]), ("ops", &["shell"])],
        &["files"],
        None,
    );

    policy.write_to_registry(&registry).expect("valid policy");

    assert_eq!(registry.exposure("read"), Some(ToolExposure::Direct));
    assert_eq!(registry.exposure("write"), Some(ToolExposure::Direct));
    assert_eq!(
        registry.exposure("shell"),
        Some(ToolExposure::Hidden),
        "an inactive group member is Hidden exposure"
    );
    assert_eq!(
        registry.exposure("free"),
        Some(ToolExposure::Direct),
        "a groupless tool keeps its registered exposure"
    );

    let descriptors = registry.descriptors();
    assert_eq!(
        names(&project_tool_definitions(
            &descriptors,
            &HashSet::new(),
            Some(&policy.visible_tool_names(&descriptors)),
        )),
        vec!["free", "read", "write"],
        "the model list must follow the group verdict, sorted by name"
    );
    assert!(
        registry.contains("shell"),
        "Hidden exposure must not remove the tool from dispatch"
    );
}

/// A policy that names an unregistered tool, claims one tool from
/// two groups, repeats one inside a group, or activates a group it
/// never declared is a typed error — and each rejection leaves the
/// registry exactly as it was.
#[test]
fn invalid_policies_are_typed_errors_that_change_nothing() {
    let registry = registry_with(&["read"]);
    registry.set_exposure("read", ToolExposure::Deferred);
    let version = registry.version();

    let unknown = policy(&[("files", &["read", "ghost"])], &["files"], None);
    assert_eq!(
        unknown.write_to_registry(&registry),
        Err(SurfacePolicyError::UnknownTool {
            group: "files".to_string(),
            tool: "ghost".to_string(),
        })
    );

    let twice_claimed =
        policy(&[("a", &["read"]), ("b", &["read"])], &[], None);
    assert_eq!(
        twice_claimed.write_to_registry(&registry),
        Err(SurfacePolicyError::AlreadyGrouped {
            group: "b".to_string(),
            tool: "read".to_string(),
            existing: "a".to_string(),
        }),
        "BTreeMap order makes the reported owner deterministic"
    );

    let repeated = policy(&[("a", &["read", "read"])], &["a"], None);
    assert_eq!(
        repeated.write_to_registry(&registry),
        Err(SurfacePolicyError::DuplicateTool {
            group: "a".to_string(),
            tool: "read".to_string(),
        })
    );

    let unknown_group = policy(&[("a", &["read"])], &["nope"], None);
    assert_eq!(
        unknown_group.write_to_registry(&registry),
        Err(SurfacePolicyError::UnknownGroup {
            name: "nope".to_string(),
        })
    );

    assert_eq!(
        registry.exposure("read"),
        Some(ToolExposure::Deferred),
        "a rejected policy must not have written anything"
    );
    assert_eq!(registry.version(), version);
}

/// The cap takes the first N *advertisable* descriptors in
/// registry (name) order; tools already hidden by `is_hidden` or
/// `Hidden` exposure take no slot.
#[test]
fn policy_visible_names_cap_is_registry_order_and_skips_hidden() {
    let registry = registry_with(&["alpha", "beta", "delta", "gamma"]);
    registry.set_hidden("beta", true);
    registry.set_exposure("gamma", ToolExposure::Hidden);
    let descriptors = registry.descriptors();

    let uncapped = policy(&[], &[], None);
    assert_eq!(
        sorted_names(&uncapped.visible_tool_names(&descriptors)),
        vec!["alpha", "delta"],
        "hidden tools never reach the allow-list"
    );

    let capped = policy(&[], &[], Some(1));
    assert_eq!(
        sorted_names(&capped.visible_tool_names(&descriptors)),
        vec!["alpha"],
        "the cap takes the first advertisable name in registry order"
    );

    let wide = policy(&[], &[], Some(9));
    assert_eq!(
        sorted_names(&wide.visible_tool_names(&descriptors)),
        vec!["alpha", "delta"],
        "a cap above the catalog changes nothing"
    );

    let narrowed = policy(&[("core", &["alpha"])], &[], Some(1));
    assert_eq!(
        sorted_names(&narrowed.visible_tool_names(&descriptors)),
        vec!["delta"],
        "the cap counts only what the group rule admits, so alpha \
         (inactive group) frees the slot"
    );
}
