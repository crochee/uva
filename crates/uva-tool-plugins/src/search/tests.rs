use std::{collections::HashSet, sync::Arc};

use serde_json::json;
use uva_core::registry::RegistryItem;
use uva_search::{
    CjkTokenizer,
    Embedder,
    HashingEmbedder,
    Memory,
    Registry,
    SearchEngine,
    Searchable,
    Tokenizer,
};
use uva_tool::{Context, ToolOutput, ToolRegistry};

use super::*;

/// A minimal `Searchable` stand-in for a skill-catalog entry.
#[derive(Clone)]
struct Skill {
    id: String,
    title: String,
    body: String,
}

impl RegistryItem for Skill {
    fn name(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.title
    }
}

impl Searchable for Skill {
    fn indexed_fields(&self) -> Vec<(String, f32)> {
        vec![(self.title.clone(), 2.0), (self.body.clone(), 1.0)]
    }
}

fn skill(id: &str, title: &str, body: &str) -> Skill {
    Skill {
        id: id.into(),
        title: title.into(),
        body: body.into(),
    }
}

fn tokenizer() -> Arc<dyn Tokenizer> {
    Arc::new(CjkTokenizer)
}

fn embedder(tokenizer: &Arc<dyn Tokenizer>) -> Arc<dyn Embedder> {
    Arc::new(HashingEmbedder::new(64, Arc::clone(tokenizer)))
}

/// One `Skill` engine holding `docs`.
fn registry_with(docs: Vec<Skill>) -> Arc<Registry> {
    let tk = tokenizer();
    let engine: SearchEngine<Skill> = SearchEngine::new(embedder(&tk), tk);
    for doc in docs {
        engine.add(doc).unwrap();
    }
    let registry = Registry::new();
    registry.register(engine);
    Arc::new(registry)
}

/// Two engines under one Registry with friendly domain labels —
/// the cross-domain case the tool exists for.
fn two_domain_registry() -> Arc<Registry> {
    let tk = tokenizer();
    let skills: SearchEngine<Skill> =
        SearchEngine::new(embedder(&tk), Arc::clone(&tk));
    skills
        .add(skill("skill:pdf", "pdf 提取", "把 PDF 解析成文本"))
        .unwrap();
    let memories: SearchEngine<Memory> = SearchEngine::new(embedder(&tk), tk);
    memories
        .add(Memory {
            id: "memory:pref".into(),
            summary: "PDF 解析偏好".into(),
            content: "用户偏好把 PDF 解析为纯文本".into(),
            tags: vec![],
            created_at: 0.0,
            importance: 1.0,
        })
        .unwrap();
    let registry = Registry::new();
    registry.register_domain("skill", skills);
    registry.register_domain("memory", memories);
    Arc::new(registry)
}

fn ctx() -> Context {
    Context::default()
}

/// The textual projection of a `ToolOutput`.
fn text_of(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| part.text().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assert the output is a model-facing error and return its text.
fn error_of(output: &ToolOutput) -> String {
    assert_eq!(output.is_error, Some(true), "expected an error output");
    text_of(output)
}

/// The agent-view hit list the model receives.
fn hits_of(output: &ToolOutput) -> Vec<serde_json::Value> {
    serde_json::from_str(&text_of(output)).expect("agent-view JSON")
}

#[tokio::test]
async fn name_and_parameters_shape() {
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    assert_eq!(t.name(), SEARCH_TOOL_NAME);
    assert_eq!(t.name(), "search");
    let p = t.parameters();
    assert_eq!(p["type"], "object");
    assert_eq!(
        p["additionalProperties"], false,
        "search schema must reject unknown keys (R124)"
    );
    assert_eq!(p["properties"]["query"]["type"], "string");
    assert_eq!(p["properties"]["limit"]["type"], "integer");
    assert_eq!(p["required"], json!(["query"]));
}

/// R124: pin the MCP-native descriptor hints — read-only and
/// idempotent within a session; `open_world_hint` is left to
/// the host's registry.
#[tokio::test]
async fn annotations_are_mcp_native_read_only() {
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    let a = t
        .annotations()
        .expect("search declares MCP-style annotations");
    assert_eq!(a.read_only_hint, Some(true));
    assert_eq!(a.destructive_hint, Some(false));
    assert_eq!(a.idempotent_hint, Some(true));
    assert_eq!(a.open_world_hint, Some(false));
}

#[tokio::test]
async fn call_returns_agent_view_json() {
    let t = SearchTool::new(registry_with(vec![skill(
        "s1",
        "release notes",
        "wheel landed",
    )]));
    let out = t
        .call(json!({"query": "release notes", "limit": 4}), &ctx())
        .await;
    assert_eq!(out.is_error, None);
    let text = text_of(&out);
    assert!(text.contains("s1"), "hit id must appear: {text}");
}

#[tokio::test]
async fn limit_defaults_to_8_and_caps_at_50() {
    let docs = (0..60)
        .map(|i| {
            skill(
                &format!("s{i}"),
                &format!("common doc {i}"),
                "common token shared",
            )
        })
        .collect();
    let t = SearchTool::new(registry_with(docs));

    let default = t.call(json!({"query": "common"}), &ctx()).await;
    assert_eq!(default.is_error, None);
    assert_eq!(hits_of(&default).len(), 8, "no limit ⇒ 8");

    let capped = t
        .call(json!({"query": "common", "limit": 100}), &ctx())
        .await;
    assert_eq!(capped.is_error, None);
    assert_eq!(hits_of(&capped).len(), 50, "limit caps at 50");

    let floored = t.call(json!({"query": "common", "limit": 0}), &ctx()).await;
    assert_eq!(floored.is_error, None);
    assert_eq!(hits_of(&floored).len(), 1, "limit floors at 1");
}

#[tokio::test]
async fn missing_query_is_model_facing_error() {
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    let out = t.call(json!({}), &ctx()).await;
    let text = error_of(&out);
    assert!(text.contains("Invalid arguments"), "{text}");
    assert!(text.contains("query"), "{text}");
}

#[tokio::test]
async fn blank_query_is_model_facing_error() {
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    let out = t.call(json!({"query": "   "}), &ctx()).await;
    assert!(error_of(&out).contains("query"));
}

#[tokio::test]
async fn empty_registry_is_empty_json_array_not_error() {
    let t = SearchTool::new(Arc::new(Registry::new()));
    let out = t.call(json!({"query": "x"}), &ctx()).await;
    assert_eq!(out.is_error, None);
    assert_eq!(text_of(&out), "[]");
}

#[tokio::test]
async fn cross_domain_query_returns_hits_from_every_engine() {
    let t = SearchTool::new(two_domain_registry());
    let out = t.call(json!({"query": "pdf", "limit": 10}), &ctx()).await;
    assert_eq!(out.is_error, None);
    let hits = hits_of(&out);
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["id"].as_str().unwrap_or_default())
        .collect();
    assert!(
        ids.contains(&"skill:pdf"),
        "skills engine must be searched: {ids:?}"
    );
    assert!(
        ids.contains(&"memory:pref"),
        "memory engine must be searched: {ids:?}"
    );
    for h in &hits {
        let domain = h["domain"].as_str().unwrap_or_default();
        let id = h["id"].as_str().unwrap_or_default();
        let expected = if id.starts_with("skill:") {
            "skill"
        } else {
            "memory"
        };
        assert_eq!(domain, expected, "domain label on {id}: {h:?}");
    }
}

#[tokio::test]
async fn domain_param_restricts_the_fan_out() {
    let t = SearchTool::new(two_domain_registry());
    let out = t
        .call(json!({"query": "pdf", "domain": "memory"}), &ctx())
        .await;
    assert_eq!(out.is_error, None);
    let hits = hits_of(&out);
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| h["domain"] == "memory"),
        "every hit must come from the memory engine: {hits:?}"
    );
}
#[tokio::test]
async fn register_helper_sets_deferred_exposure() {
    let tools = ToolRegistry::new();
    let ok = register_search_tool(
        &tools,
        registry_with(vec![skill("s1", "t", "b")]),
    );
    assert!(ok);
    let d = tools
        .descriptors()
        .into_iter()
        .find(|d| d.name == SEARCH_TOOL_NAME)
        .expect("search registered");
    assert_eq!(d.exposure, uva_tool::ToolExposure::Deferred);
}

#[tokio::test]
async fn deferred_projection_hides_schema_before_first_call() {
    let tools = ToolRegistry::new();
    register_search_tool(&tools, registry_with(vec![skill("s1", "t", "b")]));
    let defs = uva_tool::project_tool_definitions(
        &tools.descriptors(),
        &Default::default(),
        None,
    );
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].input_schema["additionalProperties"], true);
}

#[tokio::test]
async fn deferred_schema_promotes_to_the_tools_own_parameters() {
    let tools = ToolRegistry::new();
    register_search_tool(&tools, registry_with(vec![skill("s1", "t", "b")]));
    let called: HashSet<String> = HashSet::from([SEARCH_TOOL_NAME.to_string()]);
    let defs =
        uva_tool::project_tool_definitions(&tools.descriptors(), &called, None);
    assert_eq!(defs.len(), 1);
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    assert_eq!(defs[0].input_schema, t.parameters());
}

#[test]
fn execution_mode_is_parallel() {
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    assert!(matches!(t.mode(), uva_tool::ExecutionMode::Parallel));
}

#[test]
fn description_mentions_cross_domain() {
    let t = SearchTool::new(registry_with(vec![skill("s1", "t", "b")]));
    let d = t.description();
    assert!(
        d.contains("skill") && d.contains("memory"),
        "description must name the domains it spans: {d}"
    );
}
