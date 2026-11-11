//! The crate's acceptance tests.
//!
//! The host's risk policy has to reach the remote tool, a reduced plugin
//! must expose no gate, and every failure path the channel documents —
//! exit, deadline, non-JSON frame, out-of-order answers, a wedged stdin
//! — has to resolve rather than hang. Tool names have to belong to one
//! plugin each.
//!
//! The plugins here are `/bin/sh` running a few lines of scripted
//! protocol. The process boundary is the thing under test, so a fake
//! *inside* this process would test nothing.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use elysper_harness::Host;
use elysper_plugin_runtime::{
    host::{
        Authority,
        HostError,
        PluginRegistry,
        RemotePlugin,
        RemotePluginConfig,
        TransportKind,
    },
    protocol::{PluginCapabilities, PluginError},
};
use elysper_provider::types::ContentPart;
use elysper_steering::HookEvent;
use elysper_tool::{Context, Risk, Tool, ToolOutput};
use futures::{executor::block_on, join};
use serde_json::json;

/// Serves one tool and answers a call with a fixed string.
const TOOL_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) reply='{"tools":true,"observe":false,"transform":false}' ;;
    *elysper/tools/list*)
      reply='[{"name":"echo","description":"Echo the arguments back","input_schema":{"type":"object","properties":{"text":{"type":"string"}}}}]'
      ;;
    *elysper/tools/call*) reply='"remote pong"' ;;
    *) continue ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$reply"
done
"#;

/// Observes only: records every `elysper/event` line into the file named
/// by its first argument.
const OBSERVER_PLUGIN: &str = r#"
while IFS= read -r line; do
  case "$line" in
    *elysper/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":false,"observe":true,"transform":false}}\n' "$id"
      ;;
    *elysper/event*) printf '%s\n' "$line" >> "$1" ;;
  esac
done
"#;

/// Answers the handshake, records its pid, and then stops reading stdin:
/// a call is never answered and a large enough call cannot even be
/// written. `exec sleep` replaces the shell, so the process that is
/// killed is the one holding stdout — the same pid the script recorded.
const WEDGED_PLUGIN: &str = r#"
printf '%s\n' "$$" > "$1"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false}}\n' "$id" ;;
    *elysper/tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"big","description":"Takes a large argument","input_schema":{"type":"object"}}]}\n' "$id" ;;
    *elysper/tools/call*) exec sleep 5 ;;
  esac
done
"#;

/// Answers the handshake and then exits mid-call.
const EXITS_ON_CALL_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false}}\n' "$id" ;;
    *elysper/tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"doomed","description":"Exits mid-call","input_schema":{"type":"object"}}]}\n' "$id" ;;
    *elysper/tools/call*) exit 0 ;;
  esac
done
"#;

/// Answers the handshake, then writes a line that is not JSON.
const GARBAGE_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false}}\n' "$id" ;;
    *elysper/tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"garbled","description":"Writes junk","input_schema":{"type":"object"}}]}\n' "$id" ;;
    *elysper/tools/call*) printf 'this line is not JSON\n' ;;
  esac
done
"#;

/// Buffers the first call and answers it *after* the second: the answers
/// come back in reverse order.
const REORDER_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false}}\n' "$id" ;;
    *elysper/tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"echo","description":"Echo the arguments back","input_schema":{"type":"object"}}]}\n' "$id" ;;
    *elysper/tools/call*)
      text=$(printf '%s' "$line" | sed -n 's/.*"text":"\([a-z]*\)".*/\1/p')
      if [ -z "$first" ]; then
        first="$id"
        firsttext="$text"
      else
        printf '{"jsonrpc":"2.0","id":%s,"result":"answer-%s"}\n' "$id" "$text"
        printf '{"jsonrpc":"2.0","id":%s,"result":"answer-%s"}\n' "$first" "$firsttext"
      fi
      ;;
  esac
done
"#;

/// Serves one tool whose name is its first argument.
const NAMED_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false}}\n' "$id" ;;
    *elysper/tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"%s","description":"Claims one name","input_schema":{"type":"object"}}]}\n' "$id" "$1" ;;
    *elysper/tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":"ok"}\n' "$id" ;;
  esac
done
"#;

/// Announces no capability at all.
const EMPTY_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":false,"observe":false,"transform":false}}\n' "$id" ;;
  esac
done
"#;

fn config(authority: Authority, risk: Risk) -> RemotePluginConfig {
    RemotePluginConfig {
        name: "fake".into(),
        command: vec!["true".into()],
        transport: TransportKind::Stdio,
        capabilities: PluginCapabilities {
            tools: true,
            observe: false,
            transform: false,
            gate: false,
            publish: false,
        },
        authority,
        risk,
        call_timeout_ms: None,
        env: None,
    }
}

// A config that runs `script`, with `arguments` as the script's `$1…`.
fn scripted(
    name: &str,
    script: &str,
    arguments: &[String],
    capabilities: PluginCapabilities,
) -> RemotePlugin {
    RemotePlugin::new(RemotePluginConfig {
        name: name.to_string(),
        command: shell(script, arguments),
        transport: TransportKind::Stdio,
        capabilities,
        authority: Authority::Reduced,
        risk: Risk::Low,
        call_timeout_ms: None,
        env: None,
    })
}

// `/bin/sh -c <script> sh <args…>`: `$0` is `sh`, so the script's own
// arguments start at `$1`.
fn shell(script: &str, arguments: &[String]) -> Vec<String> {
    let mut argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "sh".to_string(),
    ];
    argv.extend_from_slice(arguments);
    argv
}

fn tools_and_none() -> PluginCapabilities {
    PluginCapabilities {
        tools: true,
        observe: false,
        transform: false,
        gate: false,
        publish: false,
    }
}

fn tool_named(host: &Host, name: &str) -> Arc<dyn Tool> {
    host.tools()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool named `{name}` was registered"))
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

// The failure a tool call reported, as text.
fn failure(output: &ToolOutput) -> String {
    assert!(
        output.is_error.is_some(),
        "the call was expected to fail: {:?}",
        text_of(output)
    );
    text_of(output)
}

// Wait for the plugin to have written one complete line.
fn wait_for(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && text.contains('\n')
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "the plugin never recorded the event"
        );
        sleep(Duration::from_millis(10));
    }
}

// The pid the plugin recorded for itself at startup.
fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the plugin never recorded its pid"
        );
        sleep(Duration::from_millis(10));
    }
}

// Whether a pid is still a live process.
//
// `kill -0` sends no signal; the host reaps what it kills, so a killed
// plugin is gone rather than a zombie.
fn is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn wait_for_exit(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !is_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(10));
    }
}

#[test]
fn remote_tool_overrides_risk_with_the_host_policy() {
    // The tool's risk comes from the host config, never from what the
    // plugin claims about itself — and an unclassified config fails
    // closed: `Unknown` is judged `High`, so the plugin's calls are
    // held for approval.
    let plugin = RemotePlugin::new(config(Authority::Reduced, Risk::Unknown));
    assert_eq!(plugin.remote_risk(), Risk::High);

    let declared = RemotePlugin::new(config(Authority::Reduced, Risk::Low));
    assert_eq!(declared.remote_risk(), Risk::Low);
}

#[test]
fn reduced_authority_exposes_no_gate_method() {
    let plugin = RemotePlugin::new(config(Authority::Reduced, Risk::Low));
    assert!(!plugin.exposes_gate());

    // Elevation is the operator's decision, so it is the one case where
    // the gate exists at all.
    let elevated = RemotePlugin::new(config(Authority::Full, Risk::Low));
    assert!(elevated.exposes_gate());
}

#[test]
fn install_fails_fast_when_the_plugin_exits_immediately() {
    let mut host = Host::new();
    let plugin = RemotePlugin::new(config(Authority::Reduced, Risk::Low));
    let started = Instant::now();
    let error = match block_on(plugin.install(&mut host)) {
        Ok(_) => panic!("a plugin that is already gone cannot install"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            HostError::Plugin {
                kind: PluginError::Unavailable,
                ..
            }
        ),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the install waited out the deadline instead of failing fast"
    );
    assert_eq!(host.registration_count(), 0);
}

#[test]
fn install_registers_the_plugins_tools() {
    let mut host = Host::new();
    let plugin = scripted("echo", TOOL_PLUGIN, &[], tools_and_none());
    let handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the plugin should answer: {error}"),
    };

    let tools = host.tools();
    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool.name(), "echo");
    assert_eq!(tool.description(), "Echo the arguments back");
    assert_eq!(
        tool.parameters(),
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
        })
    );

    // The plugin advertised nothing about risk; the host still judges
    // the tool at the configured value.
    assert_eq!(tool.risk(&json!({})), Risk::Low);

    // A call is one round trip, and its answer is the tool's output.
    let output =
        block_on(tool.call(json!({"text": "ping"}), &Context::default()));
    assert!(output.is_error.is_none(), "the call should succeed");
    assert_eq!(text_of(&output), "remote pong");

    // The handle owns the registration: dropping it uninstalls the tool.
    drop(handle);
    assert!(host.tools().is_empty());
}

#[test]
fn install_registers_an_observer_that_posts_events() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let log = directory.path().join("events.jsonl");
    let mut host = Host::new();
    let plugin = scripted(
        "watcher",
        OBSERVER_PLUGIN,
        &[log.display().to_string()],
        PluginCapabilities {
            tools: false,
            observe: true,
            transform: false,
            gate: false,
            publish: false,
        },
    );
    let handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the plugin should answer: {error}"),
    };
    assert!(host.has_event_registrations());

    let event = HookEvent::BeforeStep {
        agent_name: "main".into(),
        run_idx: 0,
        iteration: 7,
    };
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let state = elysper_context::AgentState::with_window(1_000);
    let sink = elysper_harness::EventSink::new(Arc::new(tx), None);
    let cancel: Arc<dyn elysper_core::CancelToken> =
        Arc::new(elysper_core::AtomicCancelToken::new());
    let ctx = elysper_harness::agent::host::HookContext {
        state: &state,
        events: &sink,
        cancel: &cancel,
    };
    block_on(host.dispatch_hooks(&event, &ctx));

    let logged = wait_for(&log);
    assert!(logged.contains("elysper/event"));
    assert!(logged.contains(r#""type":"before_step""#));
    assert!(logged.contains(r#""iteration":7"#));

    drop(handle);
}

#[test]
fn a_missed_deadline_fails_the_call_and_leaves_the_plugin_alive() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let pid_file = directory.path().join("pid");
    let mut host = Host::new();
    let plugin = scripted(
        "wedged",
        WEDGED_PLUGIN,
        &[pid_file.display().to_string()],
        tools_and_none(),
    )
    .with_call_timeout(Duration::from_millis(400));
    let _handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the handshake should answer: {error}"),
    };
    let pid = wait_for_pid(&pid_file);
    let tool = tool_named(&host, "big");

    // The call is written, never answered: the deadline is all that
    // ends it.
    let started = Instant::now();
    let output =
        block_on(tool.call(json!({"text": "small"}), &Context::default()));
    let text = failure(&output);
    assert!(text.contains("Timeout"), "unexpected output: {text}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the deadline is 400ms, not 5s"
    );

    // The plugin is slow, not gone: one expired call does not brick
    // the plugin. The write landed, so the process is still there.
    assert!(is_alive(pid), "the plugin process is still alive");

    // And the channel is open: a later call reaches the plugin again
    // and meets its own deadline, because the plugin still does not
    // answer.
    let started = Instant::now();
    let output =
        block_on(tool.call(json!({"text": "small"}), &Context::default()));
    let text = failure(&output);
    assert!(
        text.contains("Timeout"),
        "the later call met its own deadline: {text}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the later call ran against the live plugin, not a closed channel"
    );
}

/// The deadline is per plugin and configurable.
///
/// The config names the bound; the host installs it. A plugin whose
/// config says nothing keeps the crate's own default.
#[test]
fn the_configured_deadline_is_the_one_the_calls_get() {
    let mut configured = config(Authority::Reduced, Risk::Low);
    configured.call_timeout_ms = Some(250);
    let plugin = RemotePlugin::new(configured);
    assert_eq!(
        plugin.call_timeout(),
        Duration::from_millis(250),
        "the configured bound is the bound the calls get"
    );

    let default = RemotePlugin::new(config(Authority::Reduced, Risk::Low));
    assert_eq!(
        default.call_timeout(),
        Duration::from_secs(30),
        "a plugin that names no bound keeps the default"
    );
}

#[test]
fn a_wedged_stdin_does_not_hang_the_call() {
    // 2 MiB of arguments cannot fit in a pipe, so once the plugin stops
    // reading, the *write* blocks while holding the stdin lock. Before
    // the deadline covered the write, this call never returned.
    let directory = tempfile::tempdir().expect("a temporary directory");
    let pid_file = directory.path().join("pid");
    let mut host = Host::new();
    let plugin = scripted(
        "wedged",
        WEDGED_PLUGIN,
        &[pid_file.display().to_string()],
        tools_and_none(),
    )
    .with_call_timeout(Duration::from_millis(400));
    let _handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the handshake should answer: {error}"),
    };
    let pid = wait_for_pid(&pid_file);
    let tool = tool_named(&host, "big");

    let started = Instant::now();
    let output = block_on(tool.call(
        json!({"text": "x".repeat(2 * 1024 * 1024)}),
        &Context::default(),
    ));
    let text = failure(&output);
    assert!(text.contains("Timeout"), "unexpected output: {text}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the write was not bounded by the deadline"
    );
    assert!(wait_for_exit(pid), "the plugin process is still alive");
}

#[test]
fn a_mid_flight_exit_fails_the_call_and_later_calls_fast() {
    // The default deadline is 30s: anything that resolves in a
    // millisecond is failing because the process is gone, not because a
    // timer ran out.
    let mut host = Host::new();
    let plugin =
        scripted("doomed", EXITS_ON_CALL_PLUGIN, &[], tools_and_none());
    let _handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the handshake should answer: {error}"),
    };
    let tool = tool_named(&host, "doomed");

    let started = Instant::now();
    let output = block_on(tool.call(json!({"text": "x"}), &Context::default()));
    let text = failure(&output);
    assert!(text.contains("Unavailable"), "unexpected output: {text}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the call waited for the deadline instead of failing on exit"
    );

    let started = Instant::now();
    let output = block_on(tool.call(json!({"text": "x"}), &Context::default()));
    failure(&output);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the second call waited for a deadline on a dead plugin"
    );
}

#[test]
fn a_non_json_frame_is_a_bad_request() {
    let mut host = Host::new();
    let plugin = scripted("garbled", GARBAGE_PLUGIN, &[], tools_and_none());
    let _handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the handshake should answer: {error}"),
    };
    let tool = tool_named(&host, "garbled");

    let output = block_on(tool.call(json!({"text": "x"}), &Context::default()));
    let text = failure(&output);
    assert!(text.contains("BadRequest"), "unexpected output: {text}");

    // The channel closed instead of looping on junk.
    let output = block_on(tool.call(json!({"text": "x"}), &Context::default()));
    failure(&output);
}

#[test]
fn concurrent_calls_are_correlated_out_of_order() {
    let mut host = Host::new();
    let plugin = scripted("reorder", REORDER_PLUGIN, &[], tools_and_none());
    let _handle = match block_on(plugin.install(&mut host)) {
        Ok(handle) => handle,
        Err(error) => panic!("the handshake should answer: {error}"),
    };
    let tool = tool_named(&host, "echo");

    // The plugin answers the second request first, so each caller can
    // only get the right result if the channel correlates by id.
    let context = Context::default();
    let first = tool.call(json!({"text": "first"}), &context);
    let second = tool.call(json!({"text": "second"}), &context);
    let (first, second) = block_on(async { join!(first, second) });

    assert_eq!(text_of(&first), "answer-first");
    assert_eq!(text_of(&second), "answer-second");
}

#[test]
fn two_plugins_cannot_claim_the_same_tool_name() {
    let mut host = Host::new();
    let mut registry = PluginRegistry::new();
    let first = scripted(
        "alpha",
        NAMED_PLUGIN,
        &["collide".to_string()],
        tools_and_none(),
    );
    let second = scripted(
        "beta",
        NAMED_PLUGIN,
        &["collide".to_string()],
        tools_and_none(),
    );
    match block_on(registry.install(&first, &mut host)) {
        Ok(tools) => assert_eq!(tools, vec!["collide".to_string()]),
        Err(error) => panic!("the first plugin should install: {error}"),
    }

    let error = match block_on(registry.install(&second, &mut host)) {
        Ok(tools) => {
            panic!("the second plugin claims a name already taken: {tools:?}")
        }
        Err(error) => error,
    };
    match &error {
        HostError::DuplicateToolName {
            plugin,
            tool,
            owner,
        } => {
            assert_eq!(plugin, "beta");
            assert_eq!(tool, "collide");
            assert_eq!(owner, "alpha");
        }
        other => panic!("unexpected error: {other}"),
    }

    // The refused plugin left the host and the registry as they were.
    assert_eq!(registry.names(), vec!["alpha"]);
    assert_eq!(registry.handles().len(), 1);
    assert_eq!(registry.owner("collide"), Some("alpha"));
    assert_eq!(host.tools().len(), 1);
}

#[test]
fn a_plugin_offering_nothing_is_an_error() {
    // Announcing nothing the config enabled would otherwise return a
    // handle over zero registrations while the process dies unattended.
    let mut host = Host::new();
    let plugin = scripted(
        "empty",
        EMPTY_PLUGIN,
        &[],
        PluginCapabilities {
            tools: true,
            observe: true,
            transform: false,
            gate: false,
            publish: false,
        },
    );
    let error = match block_on(plugin.install(&mut host)) {
        Ok(_) => panic!("a plugin that registers nothing cannot install"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            HostError::Plugin {
                kind: PluginError::BadRequest,
                ..
            }
        ),
        "unexpected error: {error}"
    );
    assert_eq!(host.registration_count(), 0);
}

#[test]
fn the_config_is_parsed_strictly() {
    // The shape a deployment file writes: lowercase strings, sparse
    // capabilities.
    let config: RemotePluginConfig = serde_json::from_value(json!({
        "name": "cfg",
        "command": ["/bin/true"],
        "transport": "stdio",
        "capabilities": {"tools": true},
        "authority": "full",
        "risk": "high",
    }))
    .expect("the documented config shape");
    assert_eq!(config.authority, Authority::Full);
    assert_eq!(config.risk, Risk::High);
    assert_eq!(config.transport, TransportKind::Stdio);

    // A misspelt key must fail: this config decides `authority` and
    // `risk`, so a silent default would be the dangerous direction.
    let typo = serde_json::from_value::<RemotePluginConfig>(json!({
        "name": "cfg",
        "command": ["/bin/true"],
        "transport": "stdio",
        "capabilities": {},
        "authority": "reduced",
        "risk": "low",
        "authoriy": "full",
    }));
    assert!(typo.is_err(), "an unknown key must fail the parse");

    // So must a missing required field.
    let missing = serde_json::from_value::<RemotePluginConfig>(json!({
        "name": "cfg",
        "command": ["/bin/true"],
        "transport": "stdio",
        "capabilities": {},
        "authority": "reduced",
    }));
    assert!(missing.is_err(), "a missing `risk` must fail the parse");

    // The unimplemented transport still has its config shape, so a
    // deployment file does not change when it lands.
    let http: RemotePluginConfig = serde_json::from_value(json!({
        "name": "cfg",
        "command": ["/bin/true"],
        "transport": {"http": {"url": "http://localhost:9000"}},
        "capabilities": {},
        "authority": "reduced",
        "risk": "low",
    }))
    .expect("the http transport shape");
    assert_eq!(
        http.transport,
        TransportKind::Http {
            url: "http://localhost:9000".to_string(),
        }
    );
}

/// The host's risk ceiling fails closed.
///
/// An operator who did not classify the plugin's risk gets the
/// cautious reading: `Risk::High`, so the plugin's calls are held for
/// approval. A declared `low` keeps the declared policy — the
/// operator's word is the operator's word.
#[test]
fn an_unclassified_plugin_risk_fails_closed() {
    let mut config = RemotePluginConfig {
        name: "policy".into(),
        command: vec!["/bin/true".to_string()],
        transport: TransportKind::Stdio,
        capabilities: PluginCapabilities {
            tools: true,
            observe: false,
            transform: false,
            gate: false,
            publish: false,
        },
        authority: Authority::Reduced,
        risk: Risk::Unknown,
        call_timeout_ms: None,
        env: None,
    };

    let unknown = RemotePlugin::new(config.clone());
    assert_eq!(
        unknown.remote_risk(),
        Risk::High,
        "an unclassified risk is judged high, so the calls are held"
    );

    config.risk = Risk::Low;
    let low = RemotePlugin::new(config.clone());
    assert_eq!(
        low.remote_risk(),
        Risk::Low,
        "a declared low risk keeps the declared policy"
    );

    config.risk = Risk::High;
    let high = RemotePlugin::new(config);
    assert_eq!(
        high.remote_risk(),
        Risk::High,
        "a declared high risk keeps the declared policy"
    );
}
