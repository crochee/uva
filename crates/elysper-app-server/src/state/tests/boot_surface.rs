//! Tests for the optional integrations the boot wires —
//! MCP server registration (R21/R30).

use std::sync::Arc;

use elysper::core::Clock;

use crate::state::{
    plugin_tool_registry,
    register_configured_mcp_servers,
    seeded_tool_registry,
    tests::support::ScopedHome,
};

/// Write `servers` as the workspace's own `.agents/mcp.json` — the
/// project layer of the declaration lookup.
fn write_mcp_json(workspace: &std::path::Path, servers: serde_json::Value) {
    let agents = workspace.join(".agents");
    std::fs::create_dir_all(&agents).expect("the .agents dir");
    std::fs::write(
        agents.join("mcp.json"),
        serde_json::to_string(&serde_json::json!({ "mcpServers": servers }))
            .expect("the declaration serializes"),
    )
    .expect("mcp.json is written");
}

/// R21/R30: an enabled MCP server in the workspace's declarations
/// has its tools published into the registry at boot, and its
/// client is retained on the returned list.
#[tokio::test]
async fn boot_registers_configured_mcp_server_tools() {
    let _home = ScopedHome::new();
    let script = r#"
read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"boot-test","version":"1"}}}'
read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"remote_add","description":"Add","inputSchema":{"type":"object"}}]}}'
sleep 5
"#;

    let dir = tempfile::tempdir().unwrap();
    write_mcp_json(
        dir.path(),
        serde_json::json!({
            "boot-test": {
                "command": "sh",
                "args": ["-c", script],
            },
        }),
    );

    let registry = seeded_tool_registry();
    let supervisor = Arc::new(elysper::mcp::McpSupervisor::new());
    let (clients, health) = register_configured_mcp_servers(
        supervisor,
        &registry,
        elysper::core::SharedClock::system().now(),
        dir.path(),
    )
    .await;

    assert_eq!(clients.len(), 1, "one server must register");
    assert_eq!(clients[0].0, "boot-test");
    assert_eq!(clients[0].1.server_name(), "boot-test");
    assert_eq!(health.len(), 1, "one health entry per declared server");
    assert_eq!(health[0].name, "boot-test");
    assert!(health[0].healthy(), "{:?}", health[0]);
    assert_eq!(health[0].registered_tools, 1);

    let entry = registry
        .get("mcp__boot-test__remote_add")
        .expect("namespaced remote_add must be registered");
    assert_eq!(entry.tool_instance().name(), "mcp__boot-test__remote_add");
    assert!(
        registry
            .snapshot()
            .iter()
            .any(|m| m.name == "mcp__boot-test__remote_add"),
        "remote tool must appear in the catalog"
    );

    write_mcp_json(
        dir.path(),
        serde_json::json!({
            "off": {
                "command": "definitely-not-a-command",
                "enabled": false,
            },
        }),
    );
    let registry2 = plugin_tool_registry();
    let supervisor = Arc::new(elysper::mcp::McpSupervisor::new());
    let (none, health) = register_configured_mcp_servers(
        supervisor,
        &registry2,
        elysper::core::SharedClock::system().now(),
        dir.path(),
    )
    .await;
    assert!(none.is_empty(), "disabled server must not spawn");
    assert!(
        health.is_empty(),
        "a disabled server is not even probed: {health:?}"
    );
}

/// R21: a server that cannot spawn is skipped, not fatal.
#[tokio::test]
async fn boot_survives_a_broken_mcp_server() {
    let _home = ScopedHome::new();
    let dir = tempfile::tempdir().unwrap();
    write_mcp_json(
        dir.path(),
        serde_json::json!({
            "broken": {
                "command": "/nonexistent/mcp-server-binary",
            },
        }),
    );

    let registry = seeded_tool_registry();
    let supervisor = Arc::new(elysper::mcp::McpSupervisor::new());
    let (clients, health) = register_configured_mcp_servers(
        supervisor,
        &registry,
        elysper::core::SharedClock::system().now(),
        dir.path(),
    )
    .await;
    assert!(clients.is_empty(), "broken server must be skipped");
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].name, "broken");
    assert!(!health[0].healthy(), "{:?}", health[0]);
    assert_eq!(health[0].state, elysper::mcp::HealthState::Reconnecting);
    assert!(registry.snapshot().iter().any(|m| m.name == "read"));
}
