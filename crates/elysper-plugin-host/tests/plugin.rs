//! The `process-plugins` native entry.
//!
//! The plugin wraps the harness [`Host`] the deployment's `[plugins]`
//! section populated: mounting it binds that host to the `tools`
//! service, and unloading it drops the registrations it carries. A
//! `Registration` withdraws only when its last clone drops, so in the
//! boot path the composition hands its registrations over instead of
//! keeping a copy.
//!
//! The fixtures here are the ones the channel already uses: a plain
//! in-process tool for the mount/unload contract, and a real
//! `/bin/sh` protocol process for the remote path. The process
//! boundary is what makes the remote test meaningful, so it is not
//! faked inside this process.

use std::sync::Arc;

use elysper_harness::Host;
use elysper_kernel::{
    Context,
    MountError,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_plugin_host::{
    Authority,
    ProcessPluginsPlugin,
    RemotePlugin,
    RemotePluginConfig,
    TransportKind,
};
use elysper_plugin_protocol::PluginCapabilities;
use elysper_tool::{
    Context as ToolContext,
    Risk,
    Tool,
    ToolOutput,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};
use futures::executor::block_on;
use serde_json::{Value, json};

/// A tool with a fixed name, enough to prove the host's registration
/// reached the `tools` service.
struct NamedTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a process-plugins test tool"
    }

    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }

    async fn call(&self, _input: Value, _context: &ToolContext) -> ToolOutput {
        ToolOutput::text("ok")
    }
}

/// Serves one `echo` tool over the real protocol.
const ECHO_PLUGIN: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *elysper/initialize*) reply='{"tools":true,"observe":false,"transform":false}' ;;
    *elysper/tools/list*)
      reply='[{"name":"echo","description":"Echo the arguments back","input_schema":{"type":"object"}}]'
      ;;
    *elysper/tools/call*) reply='"remote pong"' ;;
    *) continue ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$reply"
done
"#;

fn remote_config() -> RemotePluginConfig {
    RemotePluginConfig {
        name: "fake".to_string(),
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            ECHO_PLUGIN.to_string(),
        ],
        transport: TransportKind::Stdio,
        capabilities: PluginCapabilities {
            tools: true,
            observe: false,
            transform: false,
            gate: false,
            publish: false,
        },
        authority: Authority::Reduced,
        risk: Risk::Low,
        call_timeout_ms: None,
        env: None,
    }
}

#[test]
fn mounts_the_hosts_tools_and_unload_withdraws_them() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "process_echo",
    }));
    let plugin = ProcessPluginsPlugin::new(host.clone(), vec![registration]);

    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn KernelPlugin>,
            Arc::new(plugin) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry` and `process-plugins`");
    assert_eq!(mounted.len(), 2);

    let tools = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    assert!(
        Arc::ptr_eq(&tools, &registry),
        "the plugin binds the host to the service's registry"
    );
    assert!(
        registry.contains("process_echo"),
        "the host's tool is dispatchable"
    );
    assert_eq!(registry.tool_count(), 1);
    assert!(
        registry
            .descriptors()
            .iter()
            .any(|tool| tool.name == "process_echo"),
        "the host's tool is advertised to the model"
    );

    drop(mounted);
    assert!(
        !registry.contains("process_echo"),
        "unload must drop the registrations it carried"
    );
    assert_eq!(registry.tool_count(), 0);
}

#[test]
fn missing_tools_leaves_process_plugins_unmounted() {
    let ctx = Context::new();

    let error = mount_all(
        &ctx,
        &[Arc::new(ProcessPluginsPlugin::new(Host::new(), Vec::new()))
            as Arc<dyn KernelPlugin>],
    )
    .expect_err("the plugin cannot activate without `tools`");

    match error {
        MountError::MissingDependency { plugin_id, keys } => {
            assert_eq!(plugin_id, "process-plugins");
            assert!(
                keys.iter().any(|key| key == KEY_TOOLS),
                "the refusal names `tools`: {keys:?}"
            );
        }
        other => panic!("expected MissingDependency, got {other:?}"),
    }
}

#[test]
fn manifest_is_native_and_injects_only_the_registry() {
    let plugin = ProcessPluginsPlugin::new(Host::new(), Vec::new());
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "process-plugins");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert!(
        manifest.provides.is_empty(),
        "the entry consumes `tools`; it provides nothing"
    );
    assert_eq!(manifest.inject, vec![KEY_TOOLS.to_string()]);
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}

#[test]
fn an_installed_remote_plugin_reaches_the_tools_service() {
    let mut host = Host::new();
    let plugin = RemotePlugin::new(remote_config());
    let handle = block_on(plugin.install(&mut host)).expect("install");
    let registrations = handle.registrations().to_vec();
    // The composition in the boot path hands its registrations over;
    // the clone above must be the last one before the mount so the
    // unload below can withdraw the remote tool.
    drop(handle);

    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn KernelPlugin>,
            Arc::new(ProcessPluginsPlugin::new(host, registrations))
                as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount the remote plugin's host");
    assert!(
        registry.contains("echo"),
        "the remote tool is dispatchable through `tools`"
    );

    drop(mounted);
    assert!(
        !registry.contains("echo"),
        "dropping the mount withdraws the remote tool"
    );
    assert_eq!(registry.tool_count(), 0);
}
