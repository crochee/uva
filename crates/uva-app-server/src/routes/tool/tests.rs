use super::*;

/// A hermetic `AppState` for route tests (same shape as the
/// helper in `routes::health`).
async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state = AppState::for_test(sessions, dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// A tool with nothing but a name, for the route fixtures.
struct NamedTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl uva::tool::Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a test tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &uva::tool::Context,
    ) -> uva::tool::ToolOutput {
        uva::tool::ToolOutput::text("ok")
    }
}

fn schema(argument: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { argument: {"type": "string"} },
        "required": [argument],
    })
}

/// The tools endpoint reports the *model-facing* list — the same
/// projection the agent loop uses. A `Deferred` tool is advertised
/// with the permissive placeholder rather than its real schema,
/// and an `is_hidden` tool is absent entirely.
#[tokio::test]
async fn collect_tool_defs_projects_exposure_and_privacy() {
    let (state, _dir) = test_state().await;
    {
        let registry = state.tool_registry.as_ref();
        registry.register_entry(uva::tool::ToolEntry::dynamic(
            "alpha".to_string(),
            "a direct tool".to_string(),
            schema("a"),
        ));
        registry.register_entry(
            uva::tool::ToolEntry::dynamic(
                "beta".to_string(),
                "a deferred tool".to_string(),
                schema("b"),
            )
            .with_exposure(uva::tool::ToolExposure::Deferred),
        );
        registry.register_entry(
            uva::tool::ToolEntry::dynamic(
                "secret".to_string(),
                "a private tool".to_string(),
                schema("s"),
            )
            .with_is_hidden(true),
        );
    }

    let defs = collect_tool_defs(&state).await;
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "got {names:?}");
    assert!(names.contains(&"beta"), "got {names:?}");
    assert!(
        !names.contains(&"secret"),
        "an is_hidden tool must not be advertised; got {names:?}"
    );
    let alpha = defs
        .iter()
        .find(|d| d.name == "alpha")
        .expect("alpha is advertised");
    assert_eq!(
        alpha.input_schema,
        schema("a"),
        "a Direct tool keeps its real schema"
    );
    let beta = defs
        .iter()
        .find(|d| d.name == "beta")
        .expect("beta is advertised");
    assert_eq!(
        beta.input_schema,
        serde_json::json!({
            "type": "object",
            "additionalProperties": true,
        }),
        "a Deferred tool is advertised with the placeholder schema"
    );
}

#[test]
fn tool_info_serializes_name_and_description() {
    let info = ToolInfo {
        name: "bash".to_string(),
        description: "Execute shell commands".to_string(),
    };
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["name"], "bash");
    assert_eq!(json["description"], "Execute shell commands");
}

#[test]
fn tool_detail_serializes_with_input_schema() {
    let detail = ToolDetail {
        name: "bash".to_string(),
        description: "Execute shell commands".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"cmd": {"type": "string"}},
            "required": ["cmd"],
        }),
        provenance: "core".to_string(),
        output_definition: uva::tool::ToolOutputDefinition::passthrough("bash")
            .with_kind(uva::tool::RenderKind::Shell),
    };
    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["name"], "bash");
    assert_eq!(json["input_schema"]["type"], "object");
    assert_eq!(json["input_schema"]["required"][0], "cmd");
    assert_eq!(json["provenance"], "core");
    // R17: the render contract rides the detail envelope.
    assert_eq!(json["output_definition"]["kind"], "shell");
    assert_eq!(json["output_definition"]["name"], "bash");
}

/// A shipped tool cannot be unregistered.
///
/// The core immutability rule says a core-owned name cannot be
/// taken; deleting the entry would free the name for a plugin to
/// claim, which is the substitution the rule exists to prevent.
/// The delete is refused, and the tool stays.
#[tokio::test]
async fn a_shipped_tool_cannot_be_unregistered() {
    let (state, _dir) = test_state().await;
    let registry = state.tool_registry.as_ref();
    assert!(
        registry.register_core_entry(uva::tool::ToolEntry::new(
            std::sync::Arc::new(NamedTool { name: "shipped" }),
        )),
        "the fixture registers a shipped tool"
    );

    let error = match unregister_tool(Arc::clone(&state), "shipped".to_string())
        .await
    {
        Ok(_) => panic!("a shipped tool must not be unregistered"),
        Err(error) => error,
    };
    assert_eq!(
        error.code, "already_exists",
        "the refusal says the name is not the caller's to free"
    );
    assert!(
        registry.contains("shipped"),
        "the shipped tool keeps its name"
    );
}

/// A dynamic tool is unregisterable: the route exists for it.
#[tokio::test]
async fn a_dynamic_tool_can_be_unregistered() {
    let (state, _dir) = test_state().await;
    let registry = state.tool_registry.as_ref();
    assert!(
        registry.register_entry(uva::tool::ToolEntry::new(
            std::sync::Arc::new(NamedTool { name: "temporary" }),
        )),
        "the fixture registers a dynamic tool"
    );

    let _ = unregister_tool(Arc::clone(&state), "temporary".to_string())
        .await
        .expect("a dynamic tool is unregistered");
    assert!(!registry.contains("temporary"), "the dynamic tool is gone");
}

/// Two reads of an unchanged registry answer byte-for-byte the
/// same body.
///
/// The catalog is cached on the state keyed by the registry's
/// version, and the handler reads the cached list by reference —
/// so a cache hit does not re-project the definitions, and the
/// same registry version cannot produce two different answers.
#[tokio::test]
async fn two_reads_of_an_unchanged_registry_answer_identically() {
    let (state, _dir) = test_state().await;
    {
        let registry = state.tool_registry.as_ref();
        registry.register_entry(uva::tool::ToolEntry::new(Arc::new(
            NamedTool { name: "alpha" },
        )));
        registry.register_entry(uva::tool::ToolEntry::new(Arc::new(
            NamedTool { name: "beta" },
        )));
    }

    let first = list_tools(Arc::clone(&state), PageQuery::default())
        .await
        .expect("the first list");
    let second = list_tools(Arc::clone(&state), PageQuery::default())
        .await
        .expect("the second list");

    let first_body = serde_json::to_value(&first).expect("serialises");
    let second_body = serde_json::to_value(&second).expect("serialises");
    assert_eq!(
        first_body, second_body,
        "an unchanged registry must answer the same body twice"
    );
    assert!(first.data.len() >= 2, "the registered tools are listed");
    let names: Vec<&str> =
        first.data.iter().map(|tool| tool.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "alpha is listed: {names:?}");
    assert!(names.contains(&"beta"), "beta is listed: {names:?}");
}

/// The list answers in the route's declared order, and a `-` prefix
/// turns it.
///
/// These are the two facts the old handler kept in two places — a
/// whitelist const for the first and a `reverse()` for the second,
/// with nothing tying them together.
#[tokio::test]
async fn the_tool_list_orders_by_name_and_a_minus_prefix_turns_it() {
    let (state, _dir) = test_state().await;
    {
        let registry = state.tool_registry.as_ref();
        for name in ["zeta", "alpha", "mike"] {
            registry.register_entry(uva::tool::ToolEntry::new(Arc::new(
                NamedTool { name },
            )));
        }
    }

    let up = all_tool_names(Arc::clone(&state), PageQuery::default()).await;
    let mut ascending = up.clone();
    ascending.sort_unstable();
    assert_eq!(up, ascending, "the default order is by name ascending");

    let down =
        all_tool_names(Arc::clone(&state), PageQuery::new().with_sort("-name"))
            .await;
    ascending.reverse();
    assert_eq!(down, ascending, "`-name` reverses the same order");
}

/// Every tool name the route hands out, page by page ✓.
///
/// The mounted set outgrew one page ✓, so a test that reads one page
/// would read a prefix and call it the list ✓.
async fn all_tool_names(state: Arc<AppState>, page: PageQuery) -> Vec<String> {
    let mut names = Vec::new();
    let mut query = page;
    loop {
        let listed = list_tools(Arc::clone(&state), query.clone())
            .await
            .expect("the list");
        names.extend(listed.data.iter().map(|tool| tool.name.clone()));
        match listed.next_cursor {
            Some(cursor) => query = query.with_cursor(cursor),
            None => return names,
        }
    }
}

/// A sort the route does not declare is refused, and so is a value
/// that names no field at all.
#[tokio::test]
async fn a_sort_the_route_does_not_declare_is_refused() {
    let (state, _dir) = test_state().await;

    let err =
        list_tools(Arc::clone(&state), PageQuery::new().with_sort("kind"))
            .await
            .err()
            .expect("`kind` is not a tool sort key");
    assert_eq!(err.code, "invalid_item", "got {err:?}");

    let err = list_tools(Arc::clone(&state), PageQuery::new().with_sort(""))
        .await
        .err()
        .expect("an empty sort names no field");
    assert_eq!(err.code, "validation", "got {err:?}");
}

/// The page is bounded by `limit`, the envelope reports the whole
/// set, and the cursor names where to resume.
#[tokio::test]
async fn a_limit_bounds_the_page_and_offers_the_next_cursor() {
    let (state, _dir) = test_state().await;
    {
        let registry = state.tool_registry.as_ref();
        for name in ["aaa", "bbb", "ccc"] {
            registry.register_entry(uva::tool::ToolEntry::new(Arc::new(
                NamedTool { name },
            )));
        }
    }

    let all = list_tools(Arc::clone(&state), PageQuery::default())
        .await
        .expect("the whole set");
    let total = all.total.expect("the list carries its total") as usize;
    assert!(total >= 3, "the fixture lists its three tools");

    let first = list_tools(Arc::clone(&state), PageQuery::new().with_limit(1))
        .await
        .expect("one row");
    assert_eq!(first.data.len(), 1);
    assert_eq!(first.total, Some(total as u64));
    assert!(first.next_cursor.is_some(), "more rows remain");
}

/// A client-owned registration without a session is refused: the
/// core would have nowhere to send the calls.
#[tokio::test]
async fn a_client_owned_tool_names_its_session() {
    let (state, _dir) = test_state().await;
    let error = match register_tool(
        Arc::clone(&state),
        RegisterToolRequest {
            name: "client_tool".to_string(),
            description: "runs on the client".to_string(),
            input_schema: schema("a"),
            owner: ToolOwner::Client,
            session_id: None,
        },
    )
    .await
    {
        Ok(_) => panic!("a client-owned tool must name its session"),
        Err(error) => error,
    };
    assert_eq!(error.code, "validation");
}

/// A client-owned registration installs the entry under the
/// client's own name, with the description and schema it declared.
#[tokio::test]
async fn a_client_owned_tool_is_listed_with_its_declaration() {
    let (state, _dir) = test_state().await;
    let info = register_tool(
        Arc::clone(&state),
        RegisterToolRequest {
            name: "deploy".to_string(),
            description: "deploys the editor's build".to_string(),
            input_schema: schema("target"),
            owner: ToolOwner::Client,
            session_id: Some("s-1".to_string()),
        },
    )
    .await
    .expect("the client-owned tool registers");
    assert_eq!(info.name, "deploy");
    let detail = get_tool(Arc::clone(&state), "deploy".to_string()).await;
    let detail = detail.expect("the entry is addressable");
    assert_eq!(detail.description, "deploys the editor's build");
    assert_eq!(detail.input_schema, schema("target"));
    assert_eq!(detail.provenance, "dynamic");
}

/// The same name cannot be owned twice: the registry is one per
/// node, and a second client's declaration would shadow the first.
#[tokio::test]
async fn a_client_owned_name_is_taken_once() {
    let (state, _dir) = test_state().await;
    let request = || RegisterToolRequest {
        name: "once".to_string(),
        description: "once only".to_string(),
        input_schema: schema("a"),
        owner: ToolOwner::Client,
        session_id: Some("s-1".to_string()),
    };
    register_tool(Arc::clone(&state), request())
        .await
        .expect("the first registration takes the name");
    let error = match register_tool(Arc::clone(&state), request()).await {
        Ok(_) => panic!("the name is taken"),
        Err(error) => error,
    };
    assert_eq!(error.code, "already_exists");
}
