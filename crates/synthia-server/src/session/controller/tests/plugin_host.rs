//! R5: a plugin installed at boot must be visible in the per-run
//! session agent that actually serves chat.
//!
//! The composition is installed into the deployment's one `Host`;
//! `AppState` keeps it and every run gets a clone. This test walks
//! the real per-run path — `RunDependencies` →
//! `SessionController` → `AgentRunStreamFactory` — and asserts the
//! model-facing tool list of that run contains the plugin's tool.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde_json::{Value, json};
use synthia::{
    harness::{Host, Plugin, PluginHandle},
    provider::traits::ModelProvider,
    tool::{Context, Tool, ToolEntry, ToolOutput, ToolRegistry},
};

use super::{
    super::{
        AgentRunStreamFactory,
        RunDependencies,
        SessionController,
        SessionOp,
    },
    support::ToolCapturingProvider,
};
use crate::{
    agent_assembly::AgentAssembly,
    composition::{Composition, StaticPlugins},
    config::{PluginEntry, PluginsConfig},
};

/// The tool a statically linked plugin installs.
struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "plugin_echo"
    }

    fn description(&self) -> &str {
        "the boot plugin's tool"
    }

    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }

    async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
        ToolOutput::text("ok")
    }
}

/// A statically linked plugin: the `StaticPlugins` table maps its
/// name to this factory, and its install puts one tool in the host.
struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn name(&self) -> &str {
        "static-echo"
    }

    fn install(&self, host: &mut Host) -> PluginHandle {
        PluginHandle::new(vec![host.tool(Arc::new(EchoTool))])
    }
}

fn echo_plugin() -> Arc<dyn Plugin> {
    Arc::new(EchoPlugin)
}

/// A plugin tool registered during boot reaches a per-run session
/// agent's tool list.
#[tokio::test]
async fn a_boot_plugin_tool_is_visible_to_the_per_run_agent() {
    // Boot: one registry, one host, the config's static plugin
    // installed into it.
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(ToolEntry::dynamic(
        "core_tool".to_string(),
        "a built-in tool".to_string(),
        json!({"type": "object"}),
    ));
    let config = PluginsConfig {
        plugins: vec![PluginEntry::Static {
            name: "static-echo".to_string(),
        }],
    };
    let table = StaticPlugins::new().register("static-echo", echo_plugin);
    let mut host = Host::new();
    let composition =
        Composition::build(&config, &table, &registry, &mut host).await;
    assert_eq!(composition.resolved().len(), 1, "the plugin installed");

    // The boot agent binds the host to the node's registry, which is
    // where the plugin's tool becomes callable.
    let boot_agent = AgentAssembly {
        provider: Arc::new(synthia::test_support::FakeProvider::new(vec![])),
        tool_registry: Arc::clone(&registry),
        workspace_root: PathBuf::new(),
        descriptor: synthia::harness::default_descriptor(String::new()),
        prompt_context: Arc::new(synthia::harness::PromptContext::default()),
        steering: Arc::new(synthia::steering::Steering::noop()),
        max_iterations: 1,
        delegators: Vec::new(),
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        subagent_writers: None,
        context_manager: None,
        host: Some(host.clone()),
    }
    .build();
    drop(boot_agent);
    assert!(
        registry.contains("plugin_echo"),
        "the boot binding registers the plugin's tool"
    );

    // The per-run path: the same wiring `AppState` installs on every
    // session's `RunDependencies`.
    let provider = Arc::new(ToolCapturingProvider {
        captured: parking_lot::Mutex::new(Vec::new()),
    });
    let deps = RunDependencies::new(
        Arc::clone(&provider) as Arc<dyn ModelProvider>,
        Arc::clone(&registry),
        PathBuf::from("/tmp"),
        synthia::harness::DEFAULT_SYSTEM_PROMPT.to_string(),
    )
    .with_plugin_host(Some(host.clone()));

    let temp = tempfile::TempDir::new().expect("a temp workspace");
    let manager = synthia::session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    );
    manager
        .create_with_user("s1".to_string(), "alice".to_string())
        .await
        .expect("the session is created");
    let controller = SessionController::spawn(
        "alice",
        "s1",
        crate::session::input_queue::InputQueue::default(),
        manager.sink("alice", "s1"),
        manager.writer("alice", "s1"),
        deps,
        Duration::from_secs(30),
        Arc::new(AgentRunStreamFactory),
    );
    controller
        .submit(SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .expect("the prompt is accepted");

    // The captured request is the run agent's model-facing tool list.
    tokio::time::timeout(Duration::from_secs(10), async {
        while provider.captured.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run asked the provider for a completion");

    let captured = provider.captured.lock();
    let names: Vec<&str> =
        captured[0].iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"plugin_echo"),
        "a per-run session agent must advertise the boot plugin's tool: \
         {names:?}"
    );
    assert!(
        names.contains(&"core_tool"),
        "the run keeps the built-in surface: {names:?}"
    );
    // The composition owns the handle for the deployment's lifetime;
    // dropping it here would withdraw the registration.
    assert_eq!(composition.registration_count(), 1);
}
