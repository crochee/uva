//! Tests for the runtime plugin composition layer.
//!
//! The remote plugins here are `/bin/sh` running scripted protocol,
//! like `elysper-plugin-runtime`'s own tests: the process boundary is
//! real, the plugin is not.

use std::{path::PathBuf, sync::Arc};

use elysper::{
    harness::{
        Agent,
        Host,
        Plugin,
        PluginHandle,
        PromptContext,
        default_descriptor,
    },
    tool::{Context, Tool, ToolEntry, ToolOutput, ToolRegistry},
};
use serde_json::{Value, json};

use crate::{
    composition::{Composition, PluginSource, StaticPlugins},
    config::{PluginEntry, PluginsConfig, ServerConfig},
};

/// The argv for a `/bin/sh` plugin that serves exactly `tools` and
/// answers no other capability.
fn remote_serving(tools: &[&str]) -> Vec<String> {
    let listed = Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool,
                    "description": "a test tool",
                    "input_schema": {"type": "object"},
                })
            })
            .collect(),
    )
    .to_string();
    let script = format!(
        r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) reply='{{"tools":true,"observe":false,"transform":false,"gate":false}}' ;;
    *elysper/tools/list*) reply='{listed}' ;;
    *) continue ;;
  esac
  printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$id" "$reply"
done
"#
    );
    vec!["/bin/sh".to_string(), "-c".to_string(), script]
}

/// A minimal tool, so the static-plugin test can prove a
/// registration reached the host.
struct NamedTool {
    name: String,
}

#[async_trait::async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "a test tool"
    }

    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }

    async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
        ToolOutput::text("ok")
    }
}

/// A statically linked plugin: the table maps its name to this
/// factory, and its install puts one tool into the host.
struct StaticEcho;

impl Plugin for StaticEcho {
    fn name(&self) -> &str {
        "static-echo"
    }

    fn install(&self, host: &mut Host) -> PluginHandle {
        PluginHandle::new(vec![host.tool(Arc::new(NamedTool {
            name: "static_echo".to_string(),
        }))])
    }
}

fn static_echo() -> Arc<dyn Plugin> {
    Arc::new(StaticEcho)
}

/// A static plugin whose install registers nothing.
struct EmptyPlugin;

impl Plugin for EmptyPlugin {
    fn name(&self) -> &str {
        "empty"
    }

    fn install(&self, _host: &mut Host) -> PluginHandle {
        PluginHandle::default()
    }
}

fn empty_plugin() -> Arc<dyn Plugin> {
    Arc::new(EmptyPlugin)
}

/// A static plugin that registers the tools it was built with.
struct MultiTool {
    name: &'static str,
    tools: Vec<&'static str>,
}

impl Plugin for MultiTool {
    fn name(&self) -> &str {
        self.name
    }

    fn install(&self, host: &mut Host) -> PluginHandle {
        let registrations = self
            .tools
            .iter()
            .map(|tool| {
                host.tool(Arc::new(NamedTool {
                    name: (*tool).to_string(),
                }))
            })
            .collect();
        PluginHandle::new(registrations)
    }
}

/// One tool free, one a core tool owns: the F4 fixture.
fn two_tools_plugin() -> Arc<dyn Plugin> {
    Arc::new(MultiTool {
        name: "two-tools",
        tools: vec!["free_one", "core_tool"],
    })
}

/// A second static plugin claiming the name the first one claims.
fn second_static_echo() -> Arc<dyn Plugin> {
    Arc::new(MultiTool {
        name: "second-echo",
        tools: vec!["static_echo"],
    })
}

/// A static plugin claiming a name a remote plugin already owns.
fn remote_name_plugin() -> Arc<dyn Plugin> {
    Arc::new(MultiTool {
        name: "claims-echo",
        tools: vec!["echo"],
    })
}

/// The argv for the one-tool plugin most tests use.
fn echo_command() -> Vec<String> {
    remote_serving(&["echo"])
}

/// A static plugin claiming one name — the fixture the shipped-name
/// refusals use.
fn claiming_plugin(name: &'static str, tool: &'static str) -> Arc<dyn Plugin> {
    Arc::new(MultiTool {
        name,
        tools: vec![tool],
    })
}

/// The factory for the plugin that claims the shipped `skill` tool.
fn claims_skill() -> Arc<dyn Plugin> {
    claiming_plugin("claims-skill", "skill")
}

/// The factory for the plugin that claims a published MCP tool name.
fn claims_mcp() -> Arc<dyn Plugin> {
    claiming_plugin("claims-mcp", "mcp__files__read_file")
}

/// The factory for the plugin that claims another component's
/// registration.
fn claims_host() -> Arc<dyn Plugin> {
    claiming_plugin("claims-host", "host_tool")
}

fn remote_entry(name: &str, command: Vec<String>) -> PluginEntry {
    PluginEntry::Remote {
        name: name.to_string(),
        command,
        transport: "stdio".to_string(),
        capabilities: vec!["tools".to_string()],
        authority: "reduced".to_string(),
        risk: "unknown".to_string(),
        call_timeout_ms: None,
        env: None,
    }
}

fn names(composition: &Composition) -> Vec<(String, PluginSource)> {
    composition
        .resolved()
        .iter()
        .map(|plugin| (plugin.name.clone(), plugin.source))
        .collect()
}

/// Parse a deployment document. Every config test goes through
/// [`ServerConfig`], not `PluginsConfig` in isolation: the section's
/// placement and shape are part of what is under test.
fn parse_server_config(yaml: &str) -> ServerConfig {
    serde_yaml::from_str(yaml).expect("the deployment document must parse")
}

/// The first fenced yaml block after `heading`.
fn yaml_block_after<'a>(document: &'a str, heading: &str) -> &'a str {
    let section = document
        .split_once(heading)
        .unwrap_or_else(|| panic!("the guide must carry `{heading}`"))
        .1;
    let fenced = section
        .split_once("```yaml\n")
        .unwrap_or_else(|| panic!("`{heading}` must carry a yaml block"))
        .1;
    fenced
        .split_once("```")
        .unwrap_or_else(|| panic!("the yaml block must be closed"))
        .0
}

// -- the config surface --------------------------------------------

#[test]
fn a_plugins_config_is_the_sequence_itself() {
    let parsed: PluginsConfig =
        serde_yaml::from_str("- name: in-tree\n  kind: static\n").unwrap();
    assert!(matches!(
        &parsed.plugins[0],
        PluginEntry::Static { name } if name == "in-tree"
    ));
}

#[test]
fn the_server_config_parses_the_plugin_entries() {
    let config = parse_server_config(
        r#"
plugins:
  - name: eraser
    kind: remote
    command: ["python", "-m", "eraser"]
    transport: stdio
    capabilities: [tools, observe]
    authority: reduced
    risk: unknown
"#,
    );
    assert_eq!(config.plugins.plugins.len(), 1);
    match &config.plugins.plugins[0] {
        PluginEntry::Remote {
            name,
            authority,
            transport,
            capabilities,
            ..
        } => {
            assert_eq!(name, "eraser");
            assert_eq!(authority, "reduced");
            assert_eq!(transport, "stdio");
            assert_eq!(capabilities, &["tools", "observe"]);
        }
        other => panic!("expected a remote plugin, got {other:?}"),
    }
}

/// The exact block `DEPLOYMENT.md` publishes must parse through
/// `ServerConfig`: a deployment written from the docs is the thing
/// the config surface exists for.
#[test]
fn the_deployment_guides_plugin_block_parses_through_the_server_config() {
    let document = include_str!("../../../../DEPLOYMENT.md");
    let yaml =
        yaml_block_after(document, "### `[plugins]` — runtime composition");
    let config: ServerConfig = serde_yaml::from_str(yaml)
        .expect("the documented [plugins] block must parse");
    assert_eq!(config.plugins.plugins.len(), 1);
    match &config.plugins.plugins[0] {
        PluginEntry::Remote {
            name,
            transport,
            capabilities,
            authority,
            risk,
            ..
        } => {
            assert_eq!(name, "redactor");
            assert_eq!(transport, "stdio");
            assert_eq!(capabilities, &["tools"]);
            assert_eq!(authority, "reduced");
            assert_eq!(risk, "unknown");
        }
        other => panic!("the guide must show a remote entry, got {other:?}"),
    }
}

/// The per-plugin call deadline is configurable, and it is the
/// plugin's own: one slow plugin cannot stretch another's.
#[test]
fn a_remote_entry_names_its_own_call_timeout() {
    let config = parse_server_config(
        r#"
plugins:
  - name: slow
    kind: remote
    command: ["slow"]
    call_timeout_ms: 5000
  - name: bare
    kind: remote
    command: ["bare"]
"#,
    );
    match &config.plugins.plugins[0] {
        PluginEntry::Remote {
            call_timeout_ms, ..
        } => {
            assert_eq!(
                *call_timeout_ms,
                Some(5000),
                "the configured deadline is read"
            );
        }
        other => panic!("expected a remote plugin, got {other:?}"),
    }
    match &config.plugins.plugins[1] {
        PluginEntry::Remote {
            call_timeout_ms, ..
        } => {
            assert_eq!(
                *call_timeout_ms, None,
                "a plugin that names no deadline keeps the host's default"
            );
        }
        other => panic!("expected a remote plugin, got {other:?}"),
    }
}

#[test]
fn authority_full_is_accepted() {
    let config = parse_server_config(
        r#"
plugins:
  - name: gate
    kind: remote
    command: ["gate"]
    transport: stdio
    capabilities: [tools]
    authority: full
    risk: high
"#,
    );
    assert_eq!(config.plugins.plugins.len(), 1);
}

#[test]
fn a_remote_entry_defaults_transport_capabilities_authority_and_risk() {
    let config = parse_server_config(
        r#"
plugins:
  - name: bare
    kind: remote
    command: ["bare"]
"#,
    );
    match &config.plugins.plugins[0] {
        PluginEntry::Remote {
            transport,
            capabilities,
            authority,
            risk,
            ..
        } => {
            assert_eq!(transport, "stdio");
            assert!(capabilities.is_empty());
            assert_eq!(authority, "reduced");
            assert_eq!(risk, "unknown");
        }
        other => panic!("expected a remote plugin, got {other:?}"),
    }
}

#[test]
fn a_static_entry_needs_only_a_name() {
    let config = parse_server_config(
        r#"
plugins:
  - name: in-tree
    kind: static
"#,
    );
    assert!(matches!(
        &config.plugins.plugins[0],
        PluginEntry::Static { name } if name == "in-tree"
    ));
}

#[test]
fn an_unknown_transport_names_the_field_and_the_accepted_values() {
    let error = serde_yaml::from_str::<ServerConfig>(
        r#"
plugins:
  - name: bad
    kind: remote
    command: ["bad"]
    transport: carrier-pigeon
"#,
    )
    .expect_err("an unknown transport must be a parse error")
    .to_string();
    assert!(error.contains("transport"), "{error}");
    assert!(error.contains("stdio"), "{error}");
    assert!(error.contains("carrier-pigeon"), "{error}");
}

#[test]
fn an_unknown_authority_names_the_field_and_the_accepted_values() {
    let error = serde_yaml::from_str::<ServerConfig>(
        r#"
plugins:
  - name: bad
    kind: remote
    command: ["bad"]
    authority: root
"#,
    )
    .expect_err("an unknown authority must be a parse error")
    .to_string();
    assert!(error.contains("authority"), "{error}");
    assert!(error.contains("reduced"), "{error}");
    assert!(error.contains("full"), "{error}");
}

#[test]
fn an_unknown_risk_names_the_field_and_the_accepted_values() {
    let error = serde_yaml::from_str::<ServerConfig>(
        r#"
plugins:
  - name: bad
    kind: remote
    command: ["bad"]
    risk: catastrophic
"#,
    )
    .expect_err("an unknown risk must be a parse error")
    .to_string();
    assert!(error.contains("risk"), "{error}");
    assert!(error.contains("low"), "{error}");
    assert!(error.contains("high"), "{error}");
    assert!(error.contains("unknown"), "{error}");
}

#[test]
fn an_unknown_capability_names_the_field_and_the_accepted_values() {
    let error = serde_yaml::from_str::<ServerConfig>(
        r#"
plugins:
  - name: bad
    kind: remote
    command: ["bad"]
    capabilities: [telepathy]
"#,
    )
    .expect_err("an unknown capability must be a parse error")
    .to_string();
    assert!(error.contains("capabilities"), "{error}");
    assert!(error.contains("telepathy"), "{error}");
}

/// A misspelled key must fail loudly: `authority_level` is not
/// `authority`, and silently ignoring it would leave the plugin at
/// the default authority the operator did not choose.
#[test]
fn an_unknown_field_is_a_parse_error() {
    let error = serde_yaml::from_str::<ServerConfig>(
        r#"
plugins:
  - name: bad
    kind: remote
    command: ["bad"]
    authority_level: full
"#,
    )
    .expect_err("an unknown field must be a parse error")
    .to_string();
    assert!(error.contains("authority_level"), "{error}");
}

#[test]
fn a_server_config_without_a_plugins_section_composes_nothing() {
    let config: ServerConfig = serde_yaml::from_str("{}").unwrap();
    assert!(config.plugins.plugins.is_empty());
}

// -- the composition -----------------------------------------------

#[test]
fn the_static_factory_table_starts_empty() {
    let table = StaticPlugins::new();
    assert!(
        table.get("anything").is_none(),
        "the table starts empty; a fork registers its own factories"
    );
}

#[test]
#[should_panic(expected = "duplicate static plugin factory `twice`")]
fn a_duplicate_static_factory_name_is_refused() {
    let _ = StaticPlugins::new()
        .register("twice", static_echo)
        .register("twice", static_echo);
}

#[tokio::test]
async fn composition_installs_a_remote_plugin_and_a_static_one() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let table = StaticPlugins::new().register("static-echo", static_echo);
    let config = PluginsConfig {
        plugins: vec![
            remote_entry("shell", echo_command()),
            PluginEntry::Static {
                name: "static-echo".to_string(),
            },
        ],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert_eq!(
        names(&composition),
        vec![
            ("shell".to_string(), PluginSource::Remote),
            ("static-echo".to_string(), PluginSource::Static),
        ]
    );
    assert!(composition.skipped().is_empty());
    assert!(registry.contains("echo"), "the remote tool is installed");
    assert!(
        registry.contains("static_echo"),
        "the static tool is installed"
    );
    assert_eq!(composition.tool_owner("echo"), Some("shell"));
    assert_eq!(
        composition.registration_count(),
        2,
        "one registration per installed plugin"
    );
}

#[tokio::test]
async fn a_plugin_tool_can_be_marked_deferred_by_tools_config() {
    // Task 16 ordering: the `[tools]` section is applied AFTER the
    // plugin bind, so it can name a tool a plugin registered. Applied
    // before the bind the registry had no such tool and the entry was
    // skipped as unknown — the config was silently unaddressable.
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let table = StaticPlugins::new().register("static-echo", static_echo);
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "static-echo".to_string(),
        }],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;
    assert!(composition.skipped().is_empty());

    let applied = crate::state::apply_tools_config(
        registry.as_ref(),
        &crate::config::ToolsConfig {
            deferred: vec!["static_echo".to_string()],
            ..crate::config::ToolsConfig::default()
        },
    );

    assert!(
        applied.skipped.is_empty(),
        "the plugin's tool is addressable once the bind has run: {:?}",
        applied.skipped
    );
    assert_eq!(applied.deferred, vec!["static_echo"]);
    assert_eq!(
        registry.exposure("static_echo"),
        Some(elysper::tool::ToolExposure::Deferred),
        "the config's verdict reached the plugin's tool"
    );
}

#[tokio::test]
async fn a_dead_remote_command_is_a_diagnostic_not_an_error() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![remote_entry(
            "dead",
            vec!["definitely-not-a-plugin-binary".to_string()],
        )],
    };
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;

    assert!(composition.resolved().is_empty());
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "dead");
    assert!(
        composition.skipped()[0].reason.contains("cannot start"),
        "{}",
        composition.skipped()[0].reason
    );
}

#[tokio::test]
async fn a_plugin_promoted_to_full_without_a_gate_is_skipped() {
    // The handshake announces no `gate`, so the install refuses the
    // promotion; the composition reports it and carries on.
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Remote {
            name: "would-gate".to_string(),
            command: echo_command(),
            transport: "stdio".to_string(),
            capabilities: vec!["tools".to_string()],
            authority: "full".to_string(),
            risk: "unknown".to_string(),
            call_timeout_ms: None,
            env: None,
        }],
    };
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;

    assert!(composition.resolved().is_empty());
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "would-gate");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("gate"), "{reason}");
    assert!(reason.contains("full"), "{reason}");
}

/// Two plugins cannot claim one tool name: the second is skipped
/// with the reason, before it can shadow the first.
#[tokio::test]
async fn a_second_plugin_claiming_an_owned_tool_is_skipped_with_its_reason() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![
            remote_entry("first", echo_command()),
            remote_entry("second", echo_command()),
        ],
    };
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;

    assert_eq!(composition.resolved().len(), 1);
    assert_eq!(composition.resolved()[0].name, "first");
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "second");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("echo"), "{reason}");
    assert!(reason.contains("first"), "{reason}");
}

#[tokio::test]
async fn an_unknown_static_name_is_a_diagnostic_not_an_error() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "ghost".to_string(),
        }],
    };
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;

    assert!(composition.resolved().is_empty());
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "ghost");
    assert!(
        composition.skipped()[0].reason.contains("static"),
        "{}",
        composition.skipped()[0].reason
    );
}

/// A static plugin that installs nothing is refused like a remote
/// plugin that registers nothing: reporting it resolved would claim
/// a plugin that contributes nothing.
#[tokio::test]
async fn a_static_plugin_that_registers_nothing_is_skipped() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let table = StaticPlugins::new().register("empty", empty_plugin);
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "empty".to_string(),
        }],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert!(composition.resolved().is_empty());
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "empty");
    assert!(
        composition.skipped()[0].reason.contains("no registration"),
        "{}",
        composition.skipped()[0].reason
    );
}

/// Validity must not depend on which constructor built the entry:
/// a programmatically built entry with a value the config parser
/// would reject is skipped with a diagnostic, not defaulted.
#[tokio::test]
async fn a_programmatic_entry_cannot_bypass_validation() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Remote {
            name: "bad".to_string(),
            command: echo_command(),
            transport: "stdio".to_string(),
            capabilities: vec!["tools".to_string()],
            authority: "root".to_string(),
            risk: "unknown".to_string(),
            call_timeout_ms: None,
            env: None,
        }],
    };
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;

    assert!(composition.resolved().is_empty());
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "bad");
    assert!(
        composition.skipped()[0].reason.contains("authority"),
        "{}",
        composition.skipped()[0].reason
    );
    assert!(
        !registry.contains("echo"),
        "nothing may be spawned for an entry that failed validation"
    );
}

/// A bind the host refuses must install nothing at all: every
/// configured entry is skipped with the bind's own reason, and
/// `resolved()` claims none of them. Installing first was the bug —
/// every plugin registered into the *foreign* registry before the
/// refusal, while `resolved()` listed them here.
#[tokio::test]
async fn a_refused_bind_installs_nothing_and_skips_every_entry() {
    let elsewhere = Arc::new(ToolRegistry::new());
    let deployment = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    // The host already lives somewhere else, so `build`'s bind is
    // refused instead of silently doing nothing.
    assert!(host.bind(Arc::clone(&elsewhere)).is_empty());
    let config = PluginsConfig {
        plugins: vec![
            remote_entry("shell", echo_command()),
            PluginEntry::Static {
                name: "static-echo".to_string(),
            },
        ],
    };
    let table = StaticPlugins::new().register("static-echo", static_echo);
    let composition =
        Composition::build(&config, &table, &deployment, &mut host).await;

    assert!(
        composition.resolved().is_empty(),
        "a refused bind resolves nothing: {:?}",
        names(&composition)
    );
    assert_eq!(composition.skipped().len(), 2);
    for skipped in composition.skipped() {
        assert!(
            skipped.reason.contains("RegistryAlreadyBound"),
            "{}: {}",
            skipped.name,
            skipped.reason
        );
    }
    assert_eq!(composition.registration_count(), 0);
    assert!(
        !deployment.contains("echo"),
        "the refused bind wrote nothing to the deployment registry"
    );
    assert!(
        !elsewhere.contains("echo") && !elsewhere.contains("static_echo"),
        "the foreign registry must not receive the plugins either"
    );
    assert!(elsewhere.descriptors().is_empty());
}

/// The refusal is the bind's own, not a slice of the host's
/// diagnostic history: a second `build` over the same host reports
/// its own refusal once, not the first one's as well. A history read
/// re-reported the earlier refusal (and misnamed it), and its
/// 64-entry cap could drop refusals while `resolved()` still claimed
/// them.
#[tokio::test]
async fn a_second_build_does_not_re_report_the_first_binds_refusal() {
    let elsewhere = Arc::new(ToolRegistry::new());
    let deployment = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    assert!(host.bind(Arc::clone(&elsewhere)).is_empty());
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "static-echo".to_string(),
        }],
    };
    let table = StaticPlugins::new().register("static-echo", static_echo);

    let first =
        Composition::build(&config, &table, &deployment, &mut host).await;
    let second =
        Composition::build(&config, &table, &deployment, &mut host).await;

    for composition in [&first, &second] {
        assert_eq!(
            composition.skipped().len(),
            1,
            "one entry, one refusal: {:?}",
            composition.skipped()
        );
        assert_eq!(composition.skipped()[0].name, "static-echo");
        assert!(
            composition.skipped()[0]
                .reason
                .contains("RegistryAlreadyBound")
        );
        assert!(composition.resolved().is_empty());
    }
    assert!(
        elsewhere.descriptors().is_empty(),
        "neither build installs into the registry that owns the host"
    );
}

/// A plugin whose second tool the registry refuses is withdrawn
/// whole: the tool it did register leaves with it, so `resolved()`
/// plus `skipped()` still partition what is live. A plugin that
/// half-works is a plugin an operator cannot reason about.
#[tokio::test]
async fn a_static_plugin_with_one_refused_tool_is_withdrawn_whole() {
    let registry = Arc::new(ToolRegistry::new());
    assert!(
        registry.register_core_entry(ToolEntry::new(Arc::new(NamedTool {
            name: "core_tool".to_string(),
        }))),
        "the fixture registers the core tool"
    );
    let mut host = Host::new();
    let table = StaticPlugins::new().register("two-tools", two_tools_plugin);
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "two-tools".to_string(),
        }],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert!(
        composition.resolved().is_empty(),
        "a half-installed plugin is not resolved"
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "two-tools");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("core_tool"), "{reason}");
    assert!(reason.contains("already owns"), "{reason}");
    assert!(
        !registry.contains("free_one"),
        "the plugin's other tool is withdrawn with it"
    );
    assert!(
        registry.contains("core_tool"),
        "the core tool keeps the name"
    );
    assert_eq!(
        composition.registration_count(),
        0,
        "no handle survives a withdrawn install"
    );
}

/// A remote plugin whose tool the registry refuses is withdrawn too,
/// and its claims are freed: the next entry may use the names.
#[tokio::test]
async fn a_refused_remote_plugin_is_withdrawn_and_frees_its_claims() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![
            // Serves `free_one` and `core_tool`; the core tool's name
            // is taken, so the whole install is refused.
            remote_entry(
                "two-tools",
                remote_serving(&["free_one", "core_tool"]),
            ),
            // The same names are free for the next entry only if the
            // refused install released its claims.
            remote_entry("free-again", remote_serving(&["free_one"])),
        ],
    };
    assert!(registry.register_core_entry(ToolEntry::new(Arc::new(
        NamedTool {
            name: "core_tool".to_string(),
        }
    ))));
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;

    assert_eq!(
        names(&composition),
        vec![("free-again".to_string(), PluginSource::Remote)]
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "two-tools");
    assert!(composition.skipped()[0].reason.contains("core_tool"));
    assert!(registry.contains("free_one"));
    assert_eq!(composition.tool_owner("free_one"), Some("free-again"));
}

/// Two `static` entries cannot claim one tool name: the same check
/// remote claims get, because the later install would shadow the
/// earlier one and `static_tool_owner`'s find-first would then
/// attribute a refusal involving that name to the wrong install.
#[tokio::test]
async fn two_static_entries_claiming_one_tool_name_skip_the_later() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let table = StaticPlugins::new()
        .register("static-echo", static_echo)
        .register("second-echo", second_static_echo);
    let config = PluginsConfig {
        plugins: vec![
            PluginEntry::Static {
                name: "static-echo".to_string(),
            },
            PluginEntry::Static {
                name: "second-echo".to_string(),
            },
        ],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert_eq!(
        names(&composition),
        vec![("static-echo".to_string(), PluginSource::Static)]
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "second-echo");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("static_echo"), "{reason}");
    assert!(
        reason.contains("static-echo"),
        "the reason names the owner: {reason}"
    );
    assert_eq!(
        composition.registration_count(),
        1,
        "the duplicate's handle is gone, the first plugin's is not"
    );
    assert!(registry.contains("static_echo"));
}

/// A static plugin cannot take a name a remote plugin already owns:
/// the later entry would shadow the remote tool with an in-process
/// one.
#[tokio::test]
async fn a_static_plugin_cannot_claim_a_remote_plugins_tool() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let table =
        StaticPlugins::new().register("claims-echo", remote_name_plugin);
    let config = PluginsConfig {
        plugins: vec![
            remote_entry("shell", echo_command()),
            PluginEntry::Static {
                name: "claims-echo".to_string(),
            },
        ],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert_eq!(
        names(&composition),
        vec![("shell".to_string(), PluginSource::Remote)]
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "claims-echo");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("echo"), "{reason}");
    assert!(reason.contains("shell"), "{reason}");
    assert_eq!(composition.tool_owner("echo"), Some("shell"));
}

/// R4: the composition owns the handles, so a remote tool stays
/// registered while it is alive and leaves the registry when the
/// composition is dropped.
#[tokio::test]
async fn a_kept_composition_keeps_the_remote_tool_registered() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![remote_entry("shell", echo_command())],
    };
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;
    assert!(registry.contains("echo"), "the handle owns the tool");

    drop(composition);
    assert!(
        !registry.contains("echo"),
        "dropping the only handle withdraws the registration"
    );
}

/// The boot's handover: once [`Composition::take_registrations`] has
/// moved the handles, the composition's drop no longer uninstalls —
/// the kernel's `process-plugins` entry owns the lifetime, and
/// dropping the taken set is what withdraws the tool.
#[tokio::test]
async fn taken_registrations_transfer_the_lifetime_off_the_composition() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let config = PluginsConfig {
        plugins: vec![remote_entry("shell", echo_command())],
    };
    let mut composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;
    assert!(registry.contains("echo"), "the handle owns the tool");

    let registrations = composition.take_registrations();
    drop(composition);
    assert!(
        registry.contains("echo"),
        "the taken set, not the composition, is the lifetime"
    );

    drop(registrations);
    assert!(
        !registry.contains("echo"),
        "dropping the taken set withdraws the registration"
    );
}

/// The boot path: the composition is installed into the deployment
/// host, the boot agent receives a clone of that host, and binding it
/// puts the plugin's tool in the node's registry. The composition —
/// not the agent — is what keeps the registration alive.
#[tokio::test]
async fn assembly_installs_the_composition_into_the_agents_registry() {
    let registry = Arc::new(ToolRegistry::new());
    let config = PluginsConfig {
        plugins: vec![remote_entry("shell", echo_command())],
    };
    let mut host = Host::new();
    let composition = Composition::build(
        &config,
        &StaticPlugins::new(),
        &registry,
        &mut host,
    )
    .await;
    // The run input the agent-loop factory consumes: a `RunConfig`
    // carrying the composition's host clone.
    let agent = elysper::harness::plugin::RunConfig {
        provider: Arc::new(elysper::test_support::FakeProvider::new(vec![])),
        tool_registry: Arc::clone(&registry),
        workspace_root: PathBuf::new(),
        descriptor: default_descriptor(String::new()),
        prompt_context: Arc::new(PromptContext::default()),
        max_iterations: 1,
        delegators: Vec::new(),
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        subagent_writers: None,
        context_manager: None,
        compactor: None,
        host: Some(host.clone()),
        spawner: None,
    }
    .build();

    assert_eq!(agent.descriptor().name, "agent");
    assert!(registry.contains("echo"), "the agent can call the plugin");
    assert_eq!(composition.resolved().len(), 1);
    drop(composition);
    assert!(
        !registry.contains("echo"),
        "the composition is what keeps the registration alive"
    );
}

/// A plugin cannot take the name of the **shipped `skill` tool**.
///
/// The `skill` tool is registered as core (see
/// `elysper_skill::register_skill_tool`), so the registry refuses
/// the plugin's registration and the composition withdraws the whole
/// install with a diagnostic. Without the core provenance the
/// plugin's entry would sit above the shipped one — the registry
/// resolves a name LIFO — and the plugin's `skill` would answer
/// every call.
#[tokio::test]
async fn a_plugin_cannot_claim_the_shipped_skill_tool() {
    let registry = Arc::new(ToolRegistry::new());
    assert!(
        elysper::skill::register_skill_tool(&registry),
        "the fixture registers the shipped skill tool"
    );
    let mut host = Host::new();
    let table = StaticPlugins::new().register("claims-skill", claims_skill);
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "claims-skill".to_string(),
        }],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert!(
        composition.resolved().is_empty(),
        "a plugin that claims a shipped name is not resolved"
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "claims-skill");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("skill"), "{reason}");
    assert!(
        registry.core_owns("skill"),
        "the shipped tool keeps the name"
    );
    assert_eq!(
        registry.name_occupants("skill"),
        1,
        "the plugin's entry never reached the registry"
    );
}

/// A plugin cannot take the name of an **MCP-published tool**.
///
/// An MCP server's tool is a name the deployment ships, so it is
/// registered as core: the plugin's registration is refused and the
/// install is withdrawn with a diagnostic.
#[tokio::test]
async fn a_plugin_cannot_claim_a_published_mcp_tool_name() {
    let registry = Arc::new(ToolRegistry::new());
    // The name an MCP server's tool is published under.
    let published = "mcp__files__read_file";
    assert!(
        registry.register_core_entry(ToolEntry::new(Arc::new(NamedTool {
            name: published.to_string(),
        }))),
        "the fixture registers the published MCP tool"
    );
    let mut host = Host::new();
    let table = StaticPlugins::new().register("claims-mcp", claims_mcp);
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "claims-mcp".to_string(),
        }],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert!(
        composition.resolved().is_empty(),
        "a plugin that claims a published name is not resolved"
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "claims-mcp");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains(published), "{reason}");
    assert_eq!(
        registry.name_occupants(published),
        1,
        "the plugin's entry never reached the registry"
    );
}

/// The live registry is consulted, not only the core names: a name a
/// **dynamic** registration already holds is refused too.
///
/// The core rule covers the shipped names; this one covers every
/// other registration the deployment made. Without it a plugin could
/// shadow a host-registered tool — the registry files both entries
/// and the later one wins the dispatch.
#[tokio::test]
async fn a_plugin_cannot_shadow_another_components_registration() {
    let registry = Arc::new(ToolRegistry::new());
    // An ordinary (dynamic) registration: not core, but taken.
    assert!(
        registry.register_entry(ToolEntry::new(Arc::new(NamedTool {
            name: "host_tool".to_string(),
        }))),
        "the fixture registers a host tool"
    );
    let mut host = Host::new();
    let table = StaticPlugins::new().register("claims-host", claims_host);
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "claims-host".to_string(),
        }],
    };
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;

    assert!(
        composition.resolved().is_empty(),
        "a plugin that shadows another registration is not resolved"
    );
    assert_eq!(composition.skipped().len(), 1);
    assert_eq!(composition.skipped()[0].name, "claims-host");
    let reason = &composition.skipped()[0].reason;
    assert!(reason.contains("host_tool"), "{reason}");
    assert_eq!(
        registry.name_occupants("host_tool"),
        1,
        "the plugin's entry was withdrawn with the install"
    );
}
