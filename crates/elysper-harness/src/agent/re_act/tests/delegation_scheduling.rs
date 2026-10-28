//! Batch scheduling: a delegator's `mode()` decides which bucket its call
//! lands in, so a delegated name is not scheduled by the registry's
//! missing-tool default.

use std::sync::Arc;

use futures::channel::mpsc;
use elysper_context::AgentState;
use elysper_core::AtomicCancelToken;
use elysper_provider::{ModelProvider, ToolUse};
use elysper_tool::{ExecutionMode, ToolEntry, ToolRegistry};

use super::state::RunState;
use crate::agent::{
    AgentCaps,
    DelegatingTool,
    EventSink,
    re_act::{
        delegation::tests::EchoInterceptor,
        r#loop::{ReActLoop, tools::bucket_by_execution_mode},
    },
};

fn call(name: &str) -> ToolUse {
    ToolUse {
        id: format!("c_{name}"),
        name: name.to_string(),
        input: serde_json::json!({}),
    }
}

// A loop whose registry holds one parallel tool (`echo`) and whose
// delegators are the ones under test.
fn loop_with(delegators: Vec<Arc<dyn DelegatingTool>>) -> ReActLoop {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(elysper_test_support::FakeProvider::new(Vec::new()));
    let mut caps = AgentCaps::for_provider(provider);
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(ToolEntry::new(Arc::new(
        elysper_test_support::FakeTool::new("echo", "echoed"),
    )));
    caps.tools.registry = registry;
    caps.tools.delegators = delegators;

    let cfg = caps.model.provider.model_config();
    let run = RunState::seed(
        Arc::new(AtomicCancelToken::shared()),
        0,
        Arc::new(Vec::new()),
        AgentState::from_config(&cfg),
    );
    let (tx, _rx) = mpsc::unbounded();
    let prompt_context = caps.run_prompt_context(None);
    ReActLoop::from_caps(
        Arc::new(caps),
        cfg,
        run,
        EventSink::new(Arc::new(tx), None),
        prompt_context,
    )
}

#[test]
fn a_delegator_lands_in_the_sequential_bucket_unless_it_declares_parallel() {
    let loop_ = loop_with(vec![
        Arc::new(EchoInterceptor::new("delegated")),
        Arc::new(EchoInterceptor {
            mode: ExecutionMode::Parallel,
            ..EchoInterceptor::new("concurrent")
        }),
    ]);
    let calls = vec![call("echo"), call("delegated"), call("concurrent")];
    let guarded: Vec<Option<ToolUse>> = calls.into_iter().map(Some).collect();
    let (parallel, sequential, _) =
        bucket_by_execution_mode(&loop_, &guarded, &loop_.run.agent_state);

    assert_eq!(
        parallel,
        vec![0, 2],
        "the registry's parallel tool and the delegator that asks for \
         Parallel share the parallel bucket"
    );
    assert_eq!(
        sequential,
        vec![1],
        "a delegator defaults to Sequential, so it never shares the parallel \
         bucket with a registry tool"
    );
}
