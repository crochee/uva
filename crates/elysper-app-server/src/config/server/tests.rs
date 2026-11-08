use super::*;

// -- default_* helper functions ---------------------------------

/// The 4 `default_*` helpers MUST return the documented
/// constant values (so JSON-deserialized configs without
/// those fields get the right defaults).
#[test]
fn default_helpers_return_pinned_values() {
    assert_eq!(default_version(), "1.0");
    assert_eq!(default_host(), "127.0.0.1");
    assert_eq!(default_port(), 8080);
    assert_eq!(default_max_agents(), 5);
}

// -- BindAddress (R54) -------------------------------------------

/// The CLI wins over the config, and the config wins over the
/// defaults—the precedence a deployment expects.
#[test]
fn bind_address_resolution_precedence() {
    let config = ServerConfig {
        host: "0.0.0.0".to_string(),
        port: 9000,
        ..ServerConfig::default()
    };

    // CLI/env (clap merges them into the same field) beats config.
    assert_eq!(
        BindAddress::resolve(
            BindOverrides {
                host: Some("10.0.0.1".to_string()),
                port: Some(7000),
            },
            Some(&config),
        ),
        BindAddress {
            host: "10.0.0.1".to_string(),
            port: 7000
        }
    );

    // Config beats the defaults.
    assert_eq!(
        BindAddress::resolve(BindOverrides::default(), Some(&config)),
        BindAddress {
            host: "0.0.0.0".to_string(),
            port: 9000
        }
    );

    // Nothing anywhere → the documented defaults.
    assert_eq!(
        BindAddress::resolve(BindOverrides::default(), None),
        BindAddress {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT
        }
    );
}

/// Each part resolves on its own: `--port` does not disturb the
/// config's `host`, and vice versa. (A tuple-shaped resolver that
/// took "the CLI's value" would lose one of the two.)
#[test]
fn bind_address_resolves_host_and_port_independently() {
    let config = ServerConfig {
        host: "0.0.0.0".to_string(),
        port: 9000,
        ..ServerConfig::default()
    };
    let address = BindAddress::resolve(
        BindOverrides {
            host: None,
            port: Some(7000),
        },
        Some(&config),
    );
    assert_eq!(address.host, "0.0.0.0", "host comes from the config");
    assert_eq!(address.port, 7000, "port comes from the CLI");
    assert_eq!(address.to_string(), "0.0.0.0:7000");
}

// -- ServerConfig::default --------------------------------------

/// `ServerConfig::default()` MUST populate every field with its
/// documented default.
#[test]
fn server_config_default_fills_every_field() {
    let c = ServerConfig::default();
    assert_eq!(c.version, "1.0");
    assert_eq!(c.host, "127.0.0.1");
    assert_eq!(c.port, 8080);
    assert_eq!(c.max_agents, 5);
    assert!(c.providers.is_empty());
    assert!(c.agents.is_empty());
    assert!(c.default_agent.is_none());
    assert!(c.tools.deferred.is_empty());
    assert!(c.tools.hidden.is_empty());
    assert!(c.tools.groups.is_empty());
    assert!(c.tools.active_groups.is_empty());
    assert!(c.tools.max_visible.is_none());
}

/// The `[tools]` section deserializes with its documented shape,
/// and an omitted section is an empty surface (the no-op applied
/// at boot).
#[test]
fn tools_section_deserializes_and_defaults_empty() {
    let c: ServerConfig = serde_json::from_str(
        r#"{"tools": {
            "deferred": ["query_db"],
            "hidden": ["admin"],
            "groups": {"files": ["read", "write"]},
            "active_groups": ["files"],
            "max_visible": 7
        }}"#,
    )
    .unwrap();
    assert_eq!(c.tools.deferred, vec!["query_db"]);
    assert_eq!(c.tools.hidden, vec!["admin"]);
    assert_eq!(c.tools.groups["files"], vec!["read", "write"]);
    assert_eq!(c.tools.active_groups, vec!["files"]);
    assert_eq!(c.tools.max_visible, Some(7));

    let empty: ServerConfig = serde_json::from_str("{}").unwrap();
    assert!(empty.tools.deferred.is_empty());
    assert!(empty.tools.hidden.is_empty());
    assert!(empty.tools.groups.is_empty());
    assert!(empty.tools.active_groups.is_empty());
    assert!(empty.tools.max_visible.is_none());
}

/// Two calls to `ServerConfig::default()` MUST produce equal
/// values (deterministic, no shared state).
#[test]
fn server_config_default_is_deterministic() {
    let a = ServerConfig::default();
    let b = ServerConfig::default();
    // Pin all fields except collections (which are
    // independent anyway).
    assert_eq!(a.version, b.version);
    assert_eq!(a.host, b.host);
    assert_eq!(a.port, b.port);
    assert_eq!(a.max_agents, b.max_agents);
    assert_eq!(a.default_agent, b.default_agent);
}

/// `ServerConfig` MUST derive `Debug + Clone` (used by
/// server startup and config-reload paths).
#[test]
fn server_config_supports_debug_and_clone() {
    let c = ServerConfig::default();
    let _ = format!("{c:?}");
    let cloned = c.clone();
    assert_eq!(cloned.version, c.version);
    assert_eq!(cloned.port, c.port);
}

// -- ServerConfig::load edge cases ------------------------------

/// `ServerConfig::load` for a non-existent path MUST return
/// `Ok(ServerConfig::default())` (the documented
/// first-run-convenience behavior).
#[test]
fn load_missing_file_returns_default() {
    let path = PathBuf::from("/none/a/expected/path/config.json");
    let c = ServerConfig::load(&path).expect("missing file must not error");
    assert_eq!(c.version, DEFAULT_VERSION);
    assert_eq!(c.host, DEFAULT_HOST);
    assert_eq!(c.port, DEFAULT_PORT);
    assert_eq!(c.max_agents, DEFAULT_MAX_AGENTS);
}

/// `ServerConfig::load` for a `.yaml` extension MUST parse
/// as YAML and apply field overrides.
#[test]
fn load_yaml_file_with_field_override() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, "port: 9000\nmax_agents: 10\n").unwrap();
    let c = ServerConfig::load(&path).unwrap();
    assert_eq!(c.port, 9000);
    assert_eq!(c.max_agents, 10);
}

/// `ServerConfig::load` for a `.json` extension MUST parse
/// as JSON.
#[test]
fn load_json_file_with_field_override() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"port": 9001, "host": "10.0.0.1"}"#).unwrap();
    let c = ServerConfig::load(&path).unwrap();
    assert_eq!(c.port, 9001);
    assert_eq!(c.host, "10.0.0.1");
}

/// `ServerConfig::load` MUST return `Err` for malformed
/// JSON.
#[test]
fn load_malformed_json_returns_err() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "{ not json").unwrap();
    let result = ServerConfig::load(&path);
    assert!(result.is_err());
}

/// `ServerConfig::load` MUST return `Err` for malformed YAML.
#[test]
fn load_malformed_yaml_returns_err() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.yaml");
    std::fs::write(&path, ":bad:\n  :\n  : :").unwrap();
    let result = ServerConfig::load(&path);
    assert!(result.is_err());
}

/// `ServerConfig::load` MUST treat files without a
/// `.yaml` extension as JSON.
#[test]
fn load_unknown_extension_treated_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.txt");
    // Write valid JSON—any non-yaml extension falls
    // through to JSON parsing.
    std::fs::write(&path, r#"{"port": 7777}"#).unwrap();
    let c = ServerConfig::load(&path).unwrap();
    assert_eq!(c.port, 7777);
}
