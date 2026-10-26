//! The authority matrix: a `Full` plugin can veto a tool call and a
//! `Reduced` one cannot, and a rewrite of a tool result happens at
//! either authority — the matrix grants it to the default.
//!
//! The plugins here are `/bin/sh` running scripted protocol, like the
//! channel tests: the process boundary is the thing under test.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, executor::block_on};
use serde_json::json;
use synthia_core::{AtomicCancelToken, CancelToken, Error, SystemClock};
use synthia_harness::{Agent, AgentInput, DiagnosticKind, Host, ReActAgent};
use synthia_plugin_host::{
    Authority,
    RemotePlugin,
    RemotePluginConfig,
    TransportKind,
};
use synthia_plugin_protocol::PluginCapabilities;
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    types::{ContentPart, TextContent},
};
use synthia_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use synthia_steering::{HookDecision, HookEvent};
use synthia_tool::{Risk, ToolOutput, ToolRegistry};

/// Serves one tool, refuses every call at the gate, and rewrites every
/// result it is handed.
const POLICY_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *synthia/initialize*) reply='{"tools":true,"observe":false,"transform":true,"gate":true}' ;;
    *synthia/tools/list*)
      reply='[{"name":"echo","description":"Echo","input_schema":{"type":"object"}}]'
      ;;
    *synthia/tools/call*) reply='"remote pong"' ;;
    *synthia/gate*) reply='{"type":"block","reason":"policy says no"}' ;;
    *synthia/transform*)
      reply='{"content":[{"type":"text","text":"rewritten by the plugin"}],"metadata":{}}'
      ;;
    *) continue ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$reply"
done
"#;

/// Serves a tool and announces no `gate`: promoting it to `full`
/// would block every call, so the install must refuse it.
const NO_GATE_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *synthia/initialize*) reply='{"tools":true,"observe":false,"transform":false,"gate":false}' ;;
    *synthia/tools/list*)
      reply='[{"name":"echo","description":"Echo","input_schema":{"type":"object"}}]'
      ;;
    *) continue ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$reply"
done
"#;

/// Answers the handshake, then exits the moment the host gates a call:
/// the gate's transport fails, and a gate that cannot answer must not
/// approve.
const DIES_AT_THE_GATE_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *synthia/initialize*) reply='{"tools":true,"observe":false,"transform":false,"gate":true}' ;;
    *synthia/tools/list*)
      reply='[{"name":"echo","description":"Echo","input_schema":{"type":"object"}}]'
      ;;
    *synthia/gate*) exit 0 ;;
    *) continue ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$reply"
done
"#;

fn capabilities(tools: bool, transform: bool) -> PluginCapabilities {
    PluginCapabilities {
        tools,
        observe: false,
        transform,
        gate: false,
    }
}

fn plugin(
    script: &str,
    authority: Authority,
    capabilities: PluginCapabilities,
) -> RemotePlugin {
    RemotePlugin::new(RemotePluginConfig {
        name: "policy".into(),
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            script.to_string(),
        ],
        transport: TransportKind::Stdio,
        capabilities,
        authority,
        risk: Risk::Unknown,
    })
    // The gate tests never wait for a real deadline: a plugin that dies
    // is noticed as soon as its stdout closes.
    .with_call_timeout(Duration::from_secs(5))
}

fn before_tool() -> HookEvent {
    HookEvent::BeforeTool {
        tool_name: "echo".into(),
        call_id: "c1".into(),
        arguments: json!({"text": "ping"}),
    }
}

fn text_of(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_full_authority_plugin_can_block_a_tool_call() {
    let mut host = Host::new();
    let _handle = block_on(
        plugin(POLICY_PLUGIN, Authority::Full, capabilities(true, false))
            .install(&mut host),
    )
    .expect("the plugin should install");

    let arbitration = block_on(host.dispatch_gates(&before_tool()));
    assert_eq!(
        arbitration.decision,
        HookDecision::Block {
            reason: "policy says no".into()
        },
        "the plugin's veto is the host's decision"
    );
}

#[test]
fn a_reduced_authority_plugin_registers_no_gate() {
    let mut host = Host::new();
    let capabilities = capabilities(true, false);
    let _handle = block_on(
        plugin(POLICY_PLUGIN, Authority::Reduced, capabilities)
            .install(&mut host),
    )
    .expect("the plugin should install");

    // Only the tool is registered: nothing that consumes an event, so
    // there is no path by which this plugin could veto a call.
    assert_eq!(host.registration_count(), 1);
    assert!(!host.has_event_registrations());

    let arbitration = block_on(host.dispatch_gates(&before_tool()));
    assert_eq!(arbitration.decision, HookDecision::Allow);
}

#[test]
fn a_gate_transport_error_blocks_instead_of_allowing() {
    let mut host = Host::new();
    let _handle = block_on(
        plugin(
            DIES_AT_THE_GATE_PLUGIN,
            Authority::Full,
            capabilities(true, false),
        )
        .install(&mut host),
    )
    .expect("the plugin should install");

    let arbitration = block_on(host.dispatch_gates(&before_tool()));
    assert!(
        matches!(arbitration.decision, HookDecision::Block { .. }),
        "a gate that could not answer must not approve: {:?}",
        arbitration.decision
    );
    assert!(
        host.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::GateFailed),
        "the block must be explainable afterwards: {:?}",
        host.diagnostics()
    );
}

#[test]
fn a_full_authority_plugin_that_cannot_gate_is_refused_at_install() {
    // Fail-closed is the per-call default; silence at install would
    // turn one config word into a whole-agent outage that only per-call
    // diagnostics explain.
    let mut host = Host::new();
    let error = match block_on(
        plugin(NO_GATE_PLUGIN, Authority::Full, capabilities(true, false))
            .install(&mut host),
    ) {
        Ok(_) => panic!("a plugin that cannot gate cannot be promoted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("requires the plugin to announce"),
        "the refusal names the fix: {error}"
    );
    assert_eq!(host.registration_count(), 0, "nothing is registered");
}

#[test]
fn a_reduced_authority_plugin_rewrites_a_tool_result() {
    let mut host = Host::new();
    let capabilities = capabilities(false, true);
    let _handle = block_on(
        plugin(POLICY_PLUGIN, Authority::Reduced, capabilities)
            .install(&mut host),
    )
    .expect("the plugin should install");

    assert!(host.has_result_rewriters(), "the rewrite is not escalation");

    let mut output = ToolOutput::text("remote pong");
    let changed = block_on(host.apply_result_transformers(&mut output));
    assert_eq!(changed, vec!["policy".to_string()]);
    assert_eq!(text_of(&output), "rewritten by the plugin");
}

#[test]
fn a_transform_the_config_did_not_enable_is_not_registered() {
    let mut host = Host::new();
    let _handle = block_on(
        plugin(POLICY_PLUGIN, Authority::Reduced, capabilities(true, false))
            .install(&mut host),
    )
    .expect("the plugin should install");

    assert!(!host.has_result_rewriters());
}

// A provider that plays one scripted chunk sequence per call: the
// first asks for the plugin's tool, the second answers with text.
struct ScriptedProvider {
    scripts: parking_lot::Mutex<Vec<Vec<StreamChunk>>>,
}

impl ScriptedProvider {
    fn new(scripts: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            scripts: parking_lot::Mutex::new(scripts),
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        Ok(CompletionResponse::default())
    }

    async fn complete_with_stream(
        &self,
        _request: CompletionRequest,
        _cancel_token: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        let script = self.scripts.lock().remove(0);
        for chunk in script {
            on_delta(chunk);
        }
        Ok(CompletionResponse::default())
    }
}

// One model turn that calls the plugin's `echo` tool.
fn tool_call_chunks() -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallStart {
            id: "c1".to_string(),
            name: "echo".to_string(),
            arguments: serde_json::Value::String("{}".to_string()),
        },
        StreamChunk::ToolCallEnd {
            id: "c1".to_string(),
        },
        StreamChunk::IsDone {
            result: Box::new(SamplingResult::default()),
        },
    ]
}

// One model turn that answers in text.
fn text_chunks(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult::default()),
        },
    ]
}

// The whole link: a remote tool's result is rewritten by the same
// plugin, and the log carries exactly one audit row naming it.
#[test]
fn a_remote_rewrite_leaves_one_audit_row_naming_the_plugin() {
    let mut host = Host::new();
    let capabilities = capabilities(true, true);
    let _handle = block_on(
        plugin(POLICY_PLUGIN, Authority::Reduced, capabilities)
            .install(&mut host),
    )
    .expect("the plugin should install");

    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_call_chunks(),
        text_chunks("done"),
    ]));
    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_host(host)
        .with_log_writer(writer);

    let mut stream = block_on(agent.run(
        AgentInput::text("run echo"),
        Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
    ));
    while block_on(stream.next()).is_some() {}

    let rows = block_on(sink.read()).expect("read");
    let audit: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "tool_result_transformed")
        .collect();
    assert_eq!(audit.len(), 1, "one remote rewrite is one row: {rows:#?}");
    let row = SessionEvent::from_value(audit[0]).expect("the row parses");
    let SessionEvent::ToolResultTransformed {
        transformers,
        tool_name,
        call_id,
        before,
        after,
        ..
    } = &row
    else {
        panic!("the row is an audit row: {row:?}");
    };
    assert_eq!(transformers, &["policy"], "the row names the remote plugin");
    assert_eq!(tool_name, "echo");
    assert_eq!(call_id, "c1");
    assert_ne!(before, after, "the row names two versions");
    assert!(!row.is_surface_eligible(), "the row is log-only");
}
