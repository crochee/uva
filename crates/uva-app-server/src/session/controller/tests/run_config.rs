//! R34 / R50 / R58 + factory wiring.
//!
//! Each `with_optional_*` knob on `RunDependencies` must reach
//! the `AgentRunConfig` the controller hands to the factory —
//! without that hop, a deployment's `max_visible` / `strategy`
//! / `tool_restriction` would never leave the config file. The
//! factory itself then installs each knob on the agent it
//! builds, so the wire tools / strategy / deny-list honour the
//! active configuration.
//!
//! Cover, in order: deps → config forwarding (tool_surface,
//! strategy, tool_restriction); factory → wire tools and
//! strategy (the `ToolCapturingProvider` records the request
//! tool list); R58 deny-list reaches the wire.

use std::{path::PathBuf, sync::Arc, time::Duration};

use futures::StreamExt;
use parking_lot::Mutex;
use uva::{
    core::AtomicCancelToken,
    harness::{AgentEvent, AgentInput, PromptContext},
};

use super::{
    super::{AgentRunStreamFactory, RunStreamFactory},
    support::{
        RecordedCall,
        ToolCapturingProvider,
        VecFactory,
        test_deps,
        wait_for_runs,
    },
};
use crate::agent_run_config::{AgentRunConfig, RunOverrides};

#[tokio::test]
async fn run_config_carries_the_dependencies_tool_surface() {
    let policy = uva::tool::ToolSurfacePolicy {
        max_visible: Some(4),
        ..uva::tool::ToolSurfacePolicy::default()
    };
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls)));
    let temp = tempfile::TempDir::new().unwrap();
    let manager =
        uva::session::manager::SessionRegistry::new(temp.path().to_path_buf());
    manager
        .create_with_user("s1".to_string(), "alice".to_string())
        .await
        .unwrap();
    let deps = test_deps().with_optional_tool_surface(Some(policy.clone()));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        manager.writer("alice", "s1"),
        deps,
        Duration::from_secs(60),
        factory,
    );

    controller
        .submit(super::super::SessionOp::Prompt {
            content: "go".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 1).await;

    let configs = calls.lock();
    assert_eq!(
        configs[0].config.tool_surface.as_ref(),
        Some(&policy),
        "the controller must forward the deps' surface policy"
    );
    assert!(
        test_deps().tool_surface.is_none(),
        "no [tools] section means `None`, the R33 path"
    );
}

/// R34: the real [`AgentRunStreamFactory`] installs the config's
/// tool-surface policy on the agent it builds, so the tools on the
/// wire honour the active group and the cap — while the registry
/// still holds every tool.
#[tokio::test]
async fn run_factory_narrows_the_wire_tools_to_the_tool_surface() {
    let provider = Arc::new(ToolCapturingProvider {
        captured: parking_lot::Mutex::new(Vec::new()),
    });
    let registry = Arc::new(uva::tool::registry::ToolRegistry::new());
    for name in ["gamma", "beta", "alpha"] {
        registry.register_entry(uva::tool::ToolEntry::dynamic(
            name.to_string(),
            format!("the {name} tool"),
            serde_json::json!({"type": "object", "properties": {}}),
        ));
    }
    let policy = uva::tool::ToolSurfacePolicy {
        max_visible: Some(1),
        groups: [(
            "core".to_string(),
            vec!["alpha".to_string(), "beta".to_string()],
        )]
        .into_iter()
        .collect(),
        active_groups: vec!["core".to_string()],
    };
    let config = AgentRunConfig {
        provider: Arc::clone(&provider)
            as Arc<dyn uva::provider::traits::ModelProvider>,
        tool_registry: Arc::clone(&registry),
        workspace_root: PathBuf::from("/tmp"),
        system_prompt: "be brief".to_string(),
        prompt_context: Arc::new(PromptContext::default()),
        agent_name: None,
        max_iterations: Some(1),
        agent_registry: None,
        log_writer: None,
        host: None,
        agent_loop: None,
        tool_surface: None,
    };
    let overrides = RunOverrides {
        context_manager: None,
        tool_surface: Some(policy),
        strategy: None,
        tool_restriction: None,
        compactor: None,
    };

    let stream = AgentRunStreamFactory.run_stream(
        config,
        overrides,
        AgentInput::text("go"),
        Arc::new(AtomicCancelToken::new()),
    );
    let _events: Vec<AgentEvent> = stream.collect().await;

    let captured = provider.captured.lock();
    assert_eq!(captured.len(), 1);
    let names: Vec<&str> =
        captured[0].iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha"],
        "the wire list must be the active group capped at one"
    );
    assert!(
        registry.contains("gamma"),
        "the run factory must not unregister tools it did not \
         advertise"
    );
}
