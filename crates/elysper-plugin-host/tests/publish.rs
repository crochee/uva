//! The plugin → host event direction: `elysper/publish`.
//!
//! The sink is attached where the plugin is installed, and the
//! capability gate is load-bearing: a plugin that did not declare
//! `publish` in its `elysper/initialize` answer must have its
//! notification ignored — no crash, no closed channel — while its
//! other roles keep working.
//!
//! The plugins are `/bin/sh` scripts speaking the stdio protocol —
//! the process boundary is the thing under test, so a fake inside
//! this process would test nothing.

use std::{
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use elysper_harness::Host;
use elysper_plugin_host::{
    Authority,
    PluginEventSink,
    PublishedEvent,
    RemotePlugin,
    RemotePluginConfig,
    TransportKind,
};
use elysper_plugin_protocol::{PluginCapabilities, methods};
use elysper_tool::{Context, Risk, Tool};
use futures::executor::block_on;
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Answers the handshake declaring `publish`, serves one tool, and
/// publishes one event right after the tool list. The host installs
/// the sink while completing the handshake — before it ever asks for
/// the tool list — so a notification written after that answer
/// cannot lose the race with the wiring.
const PUBLISHING_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false,"publish":true}}\n' "$id" ;;
    *elysper/tools/list*)
      printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"publisher","description":"Publishes one event","input_schema":{"type":"object"}}]}\n' "$id"
      printf '{"jsonrpc":"2.0","method":"elysper/publish","params":{"kind":"custom","payload":{"x":1}}}\n'
      ;;
    *elysper/tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":"remote pong"}\n' "$id" ;;
  esac
done
"#;

/// Serves one tool, never declares `publish`, and sends a publish
/// notification anyway right after the tool list — then has to still
/// answer a tool call: the notification must be ignored, not paid
/// for with the channel.
const SMUGGLING_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false}}\n' "$id" ;;
    *elysper/tools/list*)
      printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"echo","description":"Echo the arguments back","input_schema":{"type":"object"}}]}\n' "$id"
      printf '{"jsonrpc":"2.0","method":"elysper/publish","params":{"kind":"smuggled","payload":{"x":1}}}\n'
      ;;
    *elysper/tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":"remote pong"}\n' "$id" ;;
  esac
done
"#;

/// Writes the publish notification **before** answering the
/// handshake that declares `publish`. The settle cannot precede the
/// `initialize` answer — the host settles only after reading and
/// parsing it — and the reader reads lines in order, so the frame
/// reaches the reader strictly inside the handshake window: the test
/// exercises the buffer deterministically, never the direct path.
const EAGER_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*)
      printf '{"jsonrpc":"2.0","method":"elysper/publish","params":{"kind":"eager","payload":{"x":7}}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false,"publish":true}}\n' "$id"
      ;;
    *elysper/tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"echo","description":"Echo","input_schema":{"type":"object"}}]}\n' "$id" ;;
    *elysper/tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":"remote pong"}\n' "$id" ;;
  esac
done
"#;

/// The sink the tests install: every event lands in one list, which
/// the test thread reads while the reader thread writes.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<PublishedEvent>>);

impl PluginEventSink for RecordingSink {
    fn publish(&self, event: PublishedEvent) {
        self.0.lock().push(event);
    }
}

fn publishing_config(script: &str, publish: bool) -> RemotePluginConfig {
    RemotePluginConfig {
        name: "publisher".to_string(),
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            script.to_string(),
            "sh".to_string(),
        ],
        transport: TransportKind::Stdio,
        capabilities: PluginCapabilities {
            tools: true,
            observe: false,
            transform: false,
            gate: false,
            publish,
        },
        authority: Authority::Reduced,
        risk: Risk::Low,
        call_timeout_ms: None,
        env: None,
    }
}

fn tool_named(host: &Host, name: &str) -> Arc<dyn Tool> {
    host.tools()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool named `{name}` was registered"))
}

fn text_of(output: &elysper_tool::ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| part.text().map(str::to_string))
        .collect()
}

/// Wait until the sink holds `n` events.
fn wait_for_events(sink: &RecordingSink, n: usize) -> Vec<PublishedEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = sink.0.lock().clone();
        if events.len() >= n {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "the sink never saw the published event"
        );
        sleep(Duration::from_millis(10));
    }
}

/// Wait until enough time has passed that a notification the sink
/// never saw would have landed.
fn wait_for_quiet() {
    sleep(Duration::from_millis(300));
}

#[test]
fn publish_notifications_reach_the_installed_sink() {
    let sink = Arc::new(RecordingSink::default());
    let mut host = Host::new();
    let plugin = RemotePlugin::new(publishing_config(PUBLISHING_PLUGIN, true))
        .with_event_sink(Arc::clone(&sink) as Arc<dyn PluginEventSink>);
    let handle =
        block_on(plugin.install(&mut host)).expect("the plugin installs");

    let events = wait_for_events(&sink, 1);
    assert_eq!(
        events,
        vec![PublishedEvent {
            kind: "custom".to_string(),
            payload: json!({"x": 1}),
        }],
        "the sink sees exactly what the plugin published"
    );

    // The notification cost the channel nothing: the tool it serves
    // still answers.
    let tool = tool_named(&host, "publisher");
    let output = block_on(tool.call(json!({}), &Context::default()));
    assert!(output.is_error.is_none(), "the call should succeed");
    assert_eq!(text_of(&output), "remote pong");

    drop(handle);
}

/// The capability gate: a plugin that never declared `publish` has
/// its notification ignored — the sink is never called, the channel
/// stays open, and a later tool call succeeds.
#[test]
fn undeclared_publish_is_ignored_and_the_channel_survives() {
    let sink = Arc::new(RecordingSink::default());
    let mut host = Host::new();
    // The operator's ceiling names the role; the plugin's handshake
    // does not — so the gate that matters is the plugin's own
    // declaration, exactly as the other capabilities read it.
    let plugin = RemotePlugin::new(publishing_config(SMUGGLING_PLUGIN, true))
        .with_event_sink(Arc::clone(&sink) as Arc<dyn PluginEventSink>);
    let handle =
        block_on(plugin.install(&mut host)).expect("the plugin installs");

    // The smuggling notification was written during the install; a
    // grace period is the honest check that it never lands.
    wait_for_quiet();
    assert!(
        sink.0.lock().is_empty(),
        "an undeclared publish must be dropped, not delivered"
    );

    // Ignored is not fatal: the plugin's tool still answers over the
    // same channel.
    let tool = tool_named(&host, "echo");
    let output =
        block_on(tool.call(json!({"text": "ping"}), &Context::default()));
    assert!(
        output.is_error.is_none(),
        "the channel must survive an undeclared publish: {:?}",
        text_of(&output)
    );
    assert_eq!(text_of(&output), "remote pong");

    drop(handle);
}

/// The method constant is the one the host listens for, so the frame
/// the fixtures write and the frame the host routes agree by
/// construction rather than by copy-paste.
#[test]
fn the_publish_method_is_the_constant() {
    assert_eq!(methods::PUBLISH, "elysper/publish");
    // The frame's params are the wire type the host parses: both
    // sides of the notification share one definition.
    let params: Value = json!({"kind": "custom", "payload": {"x": 1}});
    let parsed: elysper_plugin_protocol::PluginPublish =
        serde_json::from_value(params).unwrap();
    assert_eq!(parsed.kind, "custom");
    assert_eq!(parsed.payload, json!({"x": 1}));
}

/// A plugin that publishes inside the handshake window — the frame is
/// on the wire before the `initialize` answer, so the reader must
/// buffer it and the settle must flush it — is heard, not silently
/// dropped: the capability was declared, the race is the host's.
#[test]
fn a_publish_before_the_handshake_settles_is_still_heard() {
    let sink = Arc::new(RecordingSink::default());
    let mut host = Host::new();
    let plugin = RemotePlugin::new(publishing_config(EAGER_PLUGIN, true))
        .with_event_sink(Arc::clone(&sink) as Arc<dyn PluginEventSink>);
    block_on(plugin.install(&mut host)).expect("the plugin installs");

    let events = wait_for_events(&sink, 1);
    assert_eq!(
        events,
        vec![PublishedEvent {
            kind: "eager".to_string(),
            payload: json!({"x": 7}),
        }],
        "the publish written next to the initialize answer must \
         survive the handshake window"
    );
}
