//! The plugin → host event direction, end to end.
//!
//! A plugin process that answers the `elysper/initialize` handshake
//! declaring `publish` and then writes one `elysper/publish`
//! notification must reach the session its sink was bound to: a
//! durable `plugin` row in the session's log, and the live-only
//! `SystemEvent::Plugin` ping on its event stream. The sink is
//! attached where the plugin is installed (a plugin process cannot
//! name a session it never saw) and bound to one session here.
//!
//! The plugin is a `/bin/sh` script speaking the stdio protocol —
//! the process boundary is the thing under test, so a fake inside
//! this process would test nothing.

use std::{sync::Arc, time::Duration};

use elysper::{
    harness::{AgentEvent, Host, SystemEvent},
    plugin_host::{
        Authority,
        PluginEventSink,
        PluginRegistry,
        RemotePlugin,
        RemotePluginConfig,
        TransportKind,
    },
    plugin_protocol::PluginCapabilities,
    session::SessionSink,
    tool::Risk,
};
use elysper_server::state::AppState;

/// Answers the handshake declaring `publish`, serves one tool, and
/// publishes one event right after the tool list.
///
/// The order is load-bearing: the host installs the event sink while
/// completing the handshake, *before* it ever asks for the tool
/// list, so a notification written after that answer cannot lose
/// the race with the wiring — the test stays deterministic without
/// sleeps.
const PUBLISHING_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":true,"observe":false,"transform":false,"publish":true}}\n' "$id" ;;
    *elysper/tools/list*)
      printf '{"jsonrpc":"2.0","id":%s,"result":[{"name":"publisher","description":"Publishes one event","input_schema":{"type":"object"}}]}\n' "$id"
      printf '{"jsonrpc":"2.0","method":"elysper/publish","params":{"kind":"custom","payload":{"x":1}}}\n'
      ;;
  esac
done
"#;

/// `/bin/sh -c <script>`: the argv the host spawns.
fn shell(script: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "sh".to_string(),
    ]
}

/// Wait until the session's log holds a `plugin` row, then return
/// it. Polling (not one read) because the row lands on the
/// controller's task after the sink hands the event over.
async fn wait_for_plugin_row(sink: &dyn SessionSink) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let rows = sink.read().await.expect("the session log reads");
        if let Some(row) = rows.iter().find(|row| row["type"] == "plugin") {
            return row.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no `plugin` row ever landed in the session log"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The first `SystemEvent::Plugin` on the live stream, failing the
/// test if anything else fills the window first.
async fn wait_for_live_plugin(
    live: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
) -> (String, serde_json::Value) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no live plugin event ever reached the broadcast"
        );
        let event = tokio::time::timeout(Duration::from_secs(2), live.recv())
            .await
            .expect("a live event arrives")
            .expect("the controller holds the broadcaster open");
        if let AgentEvent::System(SystemEvent::Plugin { kind, payload }) = event
        {
            return (kind, payload);
        }
    }
}

#[tokio::test]
async fn a_publishing_plugin_reaches_the_bound_session() {
    let temp = tempfile::TempDir::new().unwrap();
    let registry = elysper::session::manager::SessionRegistry::new(
        temp.path().join("sessions"),
    );
    let state = AppState::for_test(registry, temp.path().to_path_buf()).await;

    // The session the sink binds to. Opening it through the app
    // state is the path every route takes, so the controller owns
    // the session's one writer and its live stream.
    let controller = state
        .get_or_create_session_controller("u", "plugin-publish")
        .await
        .expect("the session opens");
    assert!(
        !controller.is_remote(),
        "a single-node test must own the session"
    );

    // Subscribe before the plugin can publish: the broadcast only
    // carries what a subscriber was there for.
    let mut live = controller.subscribe();

    // The seam under test: the sink is bound to this session here,
    // and attached where the plugin is installed below — the
    // composition a deployment owns once it decides how to route a
    // host-level event to a session.
    let sink: Arc<dyn PluginEventSink> = controller
        .plugin_event_sink()
        .expect("the owning controller exposes a session-bound sink");

    let mut host = Host::new();
    let mut plugins = PluginRegistry::new();
    let plugin = RemotePlugin::new(RemotePluginConfig {
        name: "publisher".to_string(),
        command: shell(PUBLISHING_PLUGIN),
        transport: TransportKind::Stdio,
        capabilities: PluginCapabilities {
            tools: true,
            observe: false,
            transform: false,
            gate: false,
            publish: true,
        },
        authority: Authority::Reduced,
        risk: Risk::Low,
        call_timeout_ms: None,
    })
    .with_event_sink(sink);
    plugins
        .install(&plugin, &mut host)
        .await
        .expect("the publishing plugin installs");

    // The durable record: a `plugin` row in the session's log.
    let session_sink = state.session_manager.sink("u", "plugin-publish");
    let row = wait_for_plugin_row(session_sink.as_ref()).await;
    assert_eq!(row["kind"], "custom", "the row: {row}");
    assert_eq!(row["payload"]["x"], 1, "the row: {row}");

    // The live ping: the same event, broadcast as it happened.
    let (kind, payload) = wait_for_live_plugin(&mut live).await;
    assert_eq!(kind, "custom");
    assert_eq!(payload["x"], 1);
}
