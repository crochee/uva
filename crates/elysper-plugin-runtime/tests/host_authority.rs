//! The authority matrix: a `Full` plugin can veto a tool call and a
//! `Reduced` one cannot, and a rewrite of a tool result happens at
//! either authority — the matrix grants it to the default.
//!
//! The plugins here are `/bin/sh` running scripted protocol, like the
//! channel tests: the process boundary is the thing under test.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use elysper_core::{AtomicCancelToken, CancelToken, Error, SystemClock};
use elysper_harness::{
    Agent,
    AgentEvent,
    AgentInput,
    DiagnosticKind,
    EventSink,
    Host,
    ReActAgent,
    Suspension,
    SystemEvent,
    agent::host::HookContext,
};
use elysper_plugin_runtime::{
    host::{Authority, RemotePlugin, RemotePluginConfig, TransportKind},
    protocol::PluginCapabilities,
};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    types::{ContentPart, TextContent},
};
use elysper_session::{
    LogWriter,
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use elysper_steering::HookDecision;
use elysper_tool::{Risk, ToolOutput, ToolRegistry};
use futures::{StreamExt, executor::block_on};
use serde_json::json;

/// The dispatch context a registration sees: an empty run state, a
/// sink with no writer and a fresh cancel token.
fn context() -> (elysper_context::AgentState, EventSink, Arc<dyn CancelToken>) {
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    (
        elysper_context::AgentState::with_window(1_000),
        EventSink::new(Arc::new(tx), None),
        Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
    )
}

macro_rules! hook_ctx {
    ($state:ident, $sink:ident, $cancel:ident, $ctx:ident) => {
        #[allow(unused_variables)]
        let ($state, $sink, $cancel) = context();
        let $ctx = HookContext {
            state: &$state,
            events: &$sink,
            cancel: &$cancel,
        };
    };
}

/// Serves one tool, refuses every call at the gate, and rewrites every
/// result it is handed.
const POLICY_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) reply='{"tools":true,"observe":false,"transform":true,"gate":true}' ;;
    *elysper/tools/list*)
      reply='[{"name":"echo","description":"Echo","input_schema":{"type":"object"}}]'
      ;;
    *elysper/tools/call*) reply='"remote pong"' ;;
    *elysper/gate*) reply='{"type":"block","reason":"policy says no"}' ;;
    *elysper/transform*)
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
    *elysper/initialize*) reply='{"tools":true,"observe":false,"transform":false,"gate":false}' ;;
    *elysper/tools/list*)
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
    *elysper/initialize*) reply='{"tools":true,"observe":false,"transform":false,"gate":true}' ;;
    *elysper/tools/list*)
      reply='[{"name":"echo","description":"Echo","input_schema":{"type":"object"}}]'
      ;;
    *elysper/gate*) exit 0 ;;
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
        publish: false,
    }
}

/// The same, with the `gate` role enabled: the operator's ceiling
/// has to name the role for the plugin to get one.
fn gate_capabilities(tools: bool, transform: bool) -> PluginCapabilities {
    PluginCapabilities {
        tools,
        observe: false,
        transform,
        gate: true,
        publish: false,
    }
}

/// The plugin's config, with the pieces the tests vary.
fn plugin_config(
    authority: Authority,
    capabilities: PluginCapabilities,
) -> RemotePluginConfig {
    RemotePluginConfig {
        name: "policy".into(),
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            POLICY_PLUGIN.to_string(),
        ],
        transport: TransportKind::Stdio,
        capabilities,
        authority,
        // A declared `low` risk: these tests are about the
        // authority matrix, not about the risk ceiling. An
        // unclassified risk is judged `high` — and a `high` call is
        // held for approval before it ever reaches the gate.
        risk: Risk::Low,
        call_timeout_ms: None,
        env: None,
    }
}

fn plugin(
    script: &str,
    authority: Authority,
    capabilities: PluginCapabilities,
) -> RemotePlugin {
    let mut config = plugin_config(authority, capabilities);
    config.command =
        vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    RemotePlugin::new(config)
    // The gate tests never wait for a real deadline: a plugin that dies
    // is noticed as soon as its stdout closes.
    .with_call_timeout(Duration::from_secs(5))
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
        plugin(
            POLICY_PLUGIN,
            Authority::Full,
            gate_capabilities(true, false),
        )
        .install(&mut host),
    )
    .expect("the plugin should install");

    let arbitration = {
        hook_ctx!(state, sink, cancel, ctx);
        let seams = elysper_harness::seams::Seams::new(host.clone());
        block_on(seams.tool_pre(&ctx, "echo", "c1", &json!({"text": "ping"})))
    };
    assert_eq!(
        arbitration.decision,
        HookDecision::Block {
            reason: "policy says no".into()
        },
        "the plugin's veto is the host's decision"
    );
    // The veto ran as middleware on the seam chain — the spine is the
    // dispatch surface an external plugin's gate lands on.
    assert!(
        host.waterfall(elysper_harness::seams::TOOL_PRE)
            .is_some_and(|chain| !chain.is_empty()),
        "the remote gate registers on the tool/pre-execute waterfall"
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

    let arbitration = {
        hook_ctx!(state, sink, cancel, ctx);
        let seams = elysper_harness::seams::Seams::new(host.clone());
        block_on(seams.tool_pre(&ctx, "echo", "c1", &json!({"text": "ping"})))
    };
    assert_eq!(arbitration.decision, HookDecision::Allow);
}

#[test]
fn a_gate_transport_error_blocks_instead_of_allowing() {
    let mut host = Host::new();
    let _handle = block_on(
        plugin(
            DIES_AT_THE_GATE_PLUGIN,
            Authority::Full,
            gate_capabilities(true, false),
        )
        .install(&mut host),
    )
    .expect("the plugin should install");

    let arbitration = {
        hook_ctx!(state, sink, cancel, ctx);
        let seams = elysper_harness::seams::Seams::new(host.clone());
        block_on(seams.tool_pre(&ctx, "echo", "c1", &json!({"text": "ping"})))
    };
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
        plugin(
            NO_GATE_PLUGIN,
            Authority::Full,
            gate_capabilities(true, false),
        )
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
    let changed = {
        hook_ctx!(state, sink, cancel, ctx);
        block_on(host.apply_result_transformers(&mut output, &ctx))
    };
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
        .expect("the host binds to the agent's registry")
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

/// The capability list is the operator's ceiling, `gate` included.
///
/// A plugin promoted to `Full` but not enabled for the `gate` role
/// gets no gate: the list says which roles this deployment lets the
/// plugin use, and a role the operator did not name is withheld —
/// whatever the authority says and whatever the plugin implements.
#[test]
fn the_gate_capability_is_the_operators_ceiling() {
    let mut with_gate = capabilities(true, false);
    with_gate.gate = true;

    let promoted = plugin("", Authority::Full, with_gate);
    assert!(
        promoted.grants_gate(),
        "full authority plus the gate capability grants the gate"
    );
    assert!(
        promoted.exposes_gate(),
        "the promotion itself is the authority half"
    );

    let withheld = plugin("", Authority::Full, capabilities(true, false));
    assert!(
        !withheld.grants_gate(),
        "the capability list withholds the gate even at full authority"
    );

    let reduced = plugin("", Authority::Reduced, with_gate);
    assert!(!reduced.grants_gate(), "a reduced plugin never gets a gate");
}

/// The risk ceiling fails closed, and the failure is an interruption.
///
/// A plugin nobody classified is judged `high`, so its calls are held
/// for approval: the run suspends with an approval request instead of
/// calling the plugin. A plugin the operator declared `low` runs
/// without an interruption — the declared policy is the operator's.
#[test]
fn an_unclassified_plugin_is_held_for_approval() {
    for (risk, held) in [(Risk::Unknown, true), (Risk::Low, false)] {
        let mut host = Host::new();
        let mut config =
            plugin_config(Authority::Reduced, capabilities(true, false));
        config.risk = risk;
        let plugin = RemotePlugin::new(config);
        let _handle = block_on(plugin.install(&mut host)).expect("install");

        let sink = Arc::new(InMemorySessionSink::new("s"));
        let writer =
            Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
        let provider = Arc::new(ScriptedProvider::new(vec![
            tool_call_chunks(),
            text_chunks("done"),
        ]));
        let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
            .with_host(host)
            .expect("the host binds to the agent's registry")
            .with_log_writer(writer);

        let mut stream = block_on(agent.run(
            AgentInput::text("run echo"),
            Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
        ));
        let mut suspended = false;
        while let Some(event) = block_on(stream.next()) {
            if matches!(
                event,
                AgentEvent::System(SystemEvent::Suspended(
                    Suspension::AwaitingApproval { .. }
                ))
            ) {
                suspended = true;
            }
        }

        assert_eq!(
            suspended, held,
            "risk {risk:?}: the call is held for approval only when the \
             plugin's risk was not classified"
        );
    }
}
