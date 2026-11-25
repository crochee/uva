//! The `translate` command: config.yaml → profile.toml.
//!
//! One migration entry point, run as the real binary (the command is
//! an operator's tool; its contract is the process exit, the output
//! file, and the stderr report).

use std::{path::PathBuf, process::Command};

/// A config.yaml exercising every mapped section plus one unmapped:
/// providers with an inline key, the typed demo entry, the
/// default_provider, a cache section, and an `[agents]` section the
/// translator must report as unmapped.
fn fixture_yaml(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        r#"
default_provider: "demo-echo"
providers:
  demo-echo:
    type: "demo"
    api_key_env: "NONE"
    default_model: "echo-model"
  openai:
    api_key: "sk-inline"
    base_url: "https://api.openai.com/v1"
    models:
      - name: "gpt-4o"
        context_window: 128000
cache:
  enabled: true
  threshold: 0.5
default_agent: "default"
agents:
  default:
    max_steps: 12
    strategy: "react"
    allowed_tools: ["read", "write"]
    denied_tools: ["shell"]
    compaction:
      enabled: true
      reserve_tokens: 1024
      keep_recent_tokens: 512
      min_messages_between_compaction: 4
"#,
    )
    .expect("write fixture");
    path
}

fn run_translate(config: &std::path::Path, out: &std::path::Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
        .arg("translate")
        .arg("--config")
        .arg(config)
        .arg("--out")
        .arg(out)
        .status()
        .expect("spawn the binary");
    assert!(status.success(), "translate exits 0: {status}");
}

#[test]
fn translate_writes_a_profile_with_the_model_adapter_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fixture_yaml(dir.path());
    let out = dir.path().join("profile.toml");
    run_translate(&config, &out);

    let text = std::fs::read_to_string(&out).expect("the output file");
    let profile = uva_app_server::profile::Profile::from_toml_str(&text)
        .expect("the output parses as a profile");
    let row = profile
        .core
        .iter()
        .find(|entry| entry.id == "model-adapter")
        .expect("the model-adapter row is present");
    assert_eq!(row.name, "model-adapter");

    // The row config is a ModelAdapterConfig: the provider catalogue
    // carries the providers and the default, the cache section the
    // yaml's cache block.
    assert_eq!(row.config["default_provider"], "demo-echo");
    assert_eq!(
        row.config["providers"]["demo-echo"]["default_model"],
        "echo-model"
    );
    assert_eq!(
        row.config["providers"]["openai"]["default_model"], "gpt-4o",
        "the legacy models[] shape is bridged"
    );
    assert_eq!(row.config["cache"]["enabled"], true);
    assert_eq!(row.config["cache"]["threshold"], 0.5);

    // The emitted row config must be mountable: the factory parses it
    // straight back into a ModelAdapterConfig, so the translation is
    // only done when that deserialization succeeds (absent optionals
    // are omitted, never rendered as TOML strings).
    let parsed: uva::runtime::ModelAdapterConfig =
        serde_json::from_value(row.config.clone())
            .expect("the row config parses as a ModelAdapterConfig");
    assert!(parsed.cache.is_some(), "the yaml cache section is carried");
    assert!(
        parsed.resilience.is_none(),
        "an absent section stays absent"
    );
    assert_eq!(parsed.catalogue.default_provider, "demo-echo");
}

/// The document layer is authoritative: a row the document does not
/// name is a plugin the deployment does not mount. A migration that
/// emitted only the migrated row would boot into a deployment without
/// the tool registry and without the agent loop — the boot fails on
/// the missing `tools` service — so the output must carry the whole
/// assembly.
#[test]
fn the_translated_document_carries_the_whole_assembly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fixture_yaml(dir.path());
    let out = dir.path().join("profile.toml");
    run_translate(&config, &out);

    let text = std::fs::read_to_string(&out).expect("the output file");
    let profile = uva_app_server::profile::Profile::from_toml_str(&text)
        .expect("the output parses as a profile");
    let ids: Vec<&str> = profile.core.iter().map(|e| e.id.as_str()).collect();
    for expected in [
        "tool-registry",
        "session-store",
        "hooks",
        "model-adapter",
        "agent-loop",
    ] {
        assert!(
            ids.contains(&expected),
            "the document keeps `{expected}`: {ids:?}"
        );
    }

    // The agent-loop row states the loop the config file described:
    // the strategy alias, the allow/deny lists, the compaction policy.
    let loop_row = profile
        .core
        .iter()
        .find(|entry| entry.id == "agent-loop")
        .expect("the agent-loop row is present");
    assert_eq!(loop_row.config["strategy"], "react");
    assert_eq!(
        loop_row.config["tool_restriction"]["allow"],
        serde_json::json!(["read", "write"])
    );
    assert_eq!(
        loop_row.config["tool_restriction"]["deny"],
        serde_json::json!(["shell"])
    );
    assert_eq!(loop_row.config["compaction"]["enabled"], true);

    // Every row the migration does not touch keeps no config of its
    // own: the built-in defaults build it.
    let untouched = profile
        .core
        .iter()
        .filter(|entry| entry.id != "model-adapter" && entry.id != "agent-loop")
        .all(|entry| entry.config.is_null());
    assert!(untouched, "only the migrated rows carry a config");
}

#[test]
fn translate_reports_unmapped_sections() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fixture_yaml(dir.path());
    let out = dir.path().join("profile.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
        .arg("translate")
        .arg("--config")
        .arg(&config)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("spawn the binary");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("agents"),
        "the unmapped section is named: {stderr}"
    );
    assert!(
        stderr.contains("model-adapter"),
        "the mapped sections are reported: {stderr}"
    );
}
