//! The `plugins/list` wire contract: the resolved and skipped
//! record an operator reads, and the method name it is answered
//! under.

use std::sync::Arc;

use elysper::{
    harness::{Host, Plugin, PluginHandle},
    session::manager::SessionRegistry,
    tool::{Context, Tool, ToolAnnotations, ToolOutput},
};
use serde_json::{Value, json};

use crate::{
    composition::{Composition, StaticPlugins},
    config::{PluginEntry, PluginsConfig},
    routes::plugins::list_plugins,
    state::AppState,
};

/// A tool that declares itself destructive, so the aggregated risk is
/// `high` rather than the `unknown` a bare tool declares.
struct DestructiveTool;

#[async_trait::async_trait]
impl Tool for DestructiveTool {
    fn name(&self) -> &str {
        "dangerous_tool"
    }

    fn description(&self) -> &str {
        "a test tool that may destroy things"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object" })
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            destructive_hint: Some(true),
            ..ToolAnnotations::default()
        })
    }

    async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
        ToolOutput::text("ok")
    }
}

/// The static plugin that registers [`DestructiveTool`].
struct Destructive;

impl Plugin for Destructive {
    fn name(&self) -> &str {
        "destructive"
    }

    fn install(&self, host: &mut Host) -> PluginHandle {
        PluginHandle::new(vec![host.tool(Arc::new(DestructiveTool))])
    }
}

fn destructive() -> Arc<dyn Plugin> {
    Arc::new(Destructive)
}

/// An `AppState` whose composition is the one the fixture built.
struct Fixture {
    state: Arc<AppState>,
    /// `$HOME` pointed at an empty directory, so the bundle scan
    /// reads only the fixture's workspace — a developer machine's
    /// real `~/.agents` cannot leak bundles into the projection.
    _home: crate::state::tests::support::ScopedHome,
    _temp: tempfile::TempDir,
}

/// Compose `plugins` into a fresh test state's host, so the projection
/// reads a composition the way boot leaves it.
async fn fixture(plugins: Vec<PluginEntry>, statics: StaticPlugins) -> Fixture {
    let home = crate::state::tests::support::ScopedHome::new();
    let temp = tempfile::tempdir().expect("a temp workspace");
    let root = temp.path().to_path_buf();
    let mut state =
        AppState::for_test(SessionRegistry::new(root.clone()), root).await;
    let mut host = state.plugin_host.clone();
    let composition = Composition::build(
        &PluginsConfig { plugins },
        &statics,
        &state.tool_registry,
        &mut host,
    )
    .await;
    state.plugins = composition;
    Fixture {
        state: Arc::new(state),
        _home: home,
        _temp: temp,
    }
}

#[tokio::test]
async fn reports_the_resolved_and_skipped_sets() {
    let fixture = fixture(
        vec![
            PluginEntry::Static {
                name: "destructive".to_string(),
            },
            // Nothing in this binary's table is registered under this
            // name: the skip an operator most needs to see, because
            // the deployment believes it configured the plugin.
            PluginEntry::Static {
                name: "not-linked".to_string(),
            },
        ],
        StaticPlugins::new().register("destructive", destructive),
    )
    .await;

    let body = list_plugins(Arc::clone(&fixture.state))
        .await
        .expect("the composition reads");
    let json = serde_json::to_value(&body).expect("the response serializes");

    assert_eq!(
        json,
        json!({
            "resolved": [{
                "name": "destructive",
                "kind": "static",
                // A static plugin is in-process code: the authority it
                // reports is the parity `full` names, not a grant the
                // composition can withhold.
                "authority": "full",
                "risk": "high",
                "tools": ["dangerous_tool"],
            }],
            "skipped": [{
                "name": "not-linked",
                "reason": "no static plugin factory is registered under \
                           this name",
            }],
            "bundles": [],
        })
    );
}

#[tokio::test]
async fn an_empty_composition_reports_empty_sets() {
    let fixture = fixture(Vec::new(), StaticPlugins::new()).await;

    let body = list_plugins(Arc::clone(&fixture.state))
        .await
        .expect("the composition reads");
    let json = serde_json::to_value(&body).expect("the response serializes");

    // The default deployment: no plugin configured, nothing skipped,
    // no bundle declared. Not an error, and not an absent field — a
    // client can tell "this build runs no plugins" from "the
    // method failing".
    assert_eq!(
        json,
        json!({ "resolved": [], "skipped": [], "bundles": [] })
    );
}

#[tokio::test]
async fn a_remote_that_cannot_start_reports_its_reason() {
    let fixture = fixture(
        vec![PluginEntry::Remote {
            name: "dead".to_string(),
            command: vec!["definitely-not-a-plugin-binary".to_string()],
            transport: "stdio".to_string(),
            capabilities: vec!["tools".to_string()],
            authority: "reduced".to_string(),
            risk: "unknown".to_string(),
            call_timeout_ms: None,
            env: None,
        }],
        StaticPlugins::new(),
    )
    .await;

    let body = list_plugins(Arc::clone(&fixture.state))
        .await
        .expect("the composition reads");
    let json = serde_json::to_value(&body).expect("the response serializes");

    assert_eq!(json["resolved"], json!([]));
    assert_eq!(json["skipped"][0]["name"], json!("dead"));
    let reason = json["skipped"][0]["reason"]
        .as_str()
        .expect("the skip carries its reason");
    assert!(
        reason.contains("cannot start"),
        "the reason names the failure, got {reason:?}"
    );
}

#[tokio::test]
async fn a_remote_plugin_reports_its_granted_authority_and_risk() {
    // `/bin/sh` serving one tool: the process boundary is real, the
    // plugin is not. It announces `gate` because the config promotes it
    // to full authority, and a promotion the plugin cannot serve is
    // refused at install.
    let listed = json!([{
        "name": "guard_check",
        "description": "a test tool",
        "input_schema": { "type": "object" },
    }])
    .to_string();
    let script = format!(
        r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) reply='{{"tools":true,"observe":false,"transform":false,"gate":true}}' ;;
    *elysper/tools/list*) reply='{listed}' ;;
    *) continue ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$id" "$reply"
done
"#
    );

    let fixture = fixture(
        vec![PluginEntry::Remote {
            name: "guard".to_string(),
            command: vec!["/bin/sh".to_string(), "-c".to_string(), script],
            transport: "stdio".to_string(),
            capabilities: vec!["tools".to_string(), "gate".to_string()],
            authority: "full".to_string(),
            risk: "high".to_string(),
            call_timeout_ms: None,
            env: None,
        }],
        StaticPlugins::new(),
    )
    .await;

    let body = list_plugins(Arc::clone(&fixture.state))
        .await
        .expect("the composition reads");
    let json = serde_json::to_value(&body).expect("the response serializes");

    assert_eq!(
        json,
        json!({
            "resolved": [{
                "name": "guard",
                "kind": "remote",
                "authority": "full",
                "risk": "high",
                "tools": ["guard_check"],
            }],
            "skipped": [],
            "bundles": [],
        })
    );
}

#[tokio::test]
async fn the_method_is_served_under_its_wire_name() {
    let fixture = fixture(
        vec![PluginEntry::Static {
            name: "destructive".to_string(),
        }],
        StaticPlugins::new().register("destructive", destructive),
    )
    .await;

    // Through the real method table, so the wire name the process
    // answers under is part of what this asserts — the transport
    // knows no method by name; the table is the whole contract.
    let mut router = crate::rpc::RpcRouter::new();
    crate::rpc::methods::register_all(&mut router);
    let response = router
        .dispatch(
            Arc::clone(&fixture.state),
            r#"{"jsonrpc":"2.0","id":1,"method":"plugins/list"}"#,
        )
        .await
        .expect("a request frame is answered");
    assert_eq!(response["jsonrpc"], json!("2.0"));
    assert_eq!(response["id"], json!(1));
    assert!(
        response.get("error").is_none(),
        "the method answers with a result: {response}"
    );

    let json = &response["result"];
    assert_eq!(json["resolved"][0]["name"], json!("destructive"));
    assert_eq!(json["resolved"][0]["kind"], json!("static"));
    assert_eq!(json["skipped"], json!([]));
}
