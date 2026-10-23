//! [`SyntheticTool`] - the harness's plugin point for synthetic tools that need loop internals (event sink, cancel, depth bookkeeping).
use std::sync::Arc;

use futures::{channel::mpsc, future::BoxFuture};
use synthia_core::CancelToken;
use synthia_provider::{ToolDefinition, ToolUse};
use synthia_tool::ToolOutput;

use crate::events::AgentEvent;

pub struct SyntheticCall<'a> {
    pub call: &'a ToolUse,
    pub cancel: Arc<dyn CancelToken>,
    pub depth: usize,
    /// Forward an event into the parent run's stream — it reaches the caller like a loop-owned event. The sender owns its
    /// channel: a plugin that spawns child tasks and forgets the closure can still emit until the stream is dropped.
    pub emit: mpsc::UnboundedSender<AgentEvent>,
}

/// A synthetic tool the loop dispatches ahead of the registry. Install with
/// [`ReActAgent::with_synthetic_tool`](crate::agent::ReActAgent::with_synthetic_tool);
/// its definitions are advertised beside the registry's (subject to restriction), and a [`claimed`](SyntheticTool::claims) name routes to [`execute`](SyntheticTool::execute).
pub trait SyntheticTool: Send + Sync {
    /// The wire definitions to advertise to the model.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// Whether this tool claims `name`.
    fn claims(&self, name: &str) -> bool;

    /// Execute the claimed call and produce the tool result.
    fn execute(
        &self,
        call: SyntheticCall<'_>,
    ) -> BoxFuture<'static, ToolOutput>;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct EchoInterceptor {
        pub name: &'static str,
    }

    impl SyntheticTool for EchoInterceptor {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: self.name.to_string(),
                description: "echo the input back".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
                cache_control: None,
                annotations: None,
            }]
        }

        fn claims(&self, name: &str) -> bool {
            name == self.name
        }

        fn execute(
            &self,
            call: SyntheticCall<'_>,
        ) -> BoxFuture<'static, ToolOutput> {
            let name = self.name;
            let input = call.call.input.clone();
            let _ = call.depth;
            Box::pin(async move {
                ToolOutput::text(format!("intercepted:{}:{}", name, input))
            })
        }
    }

    #[test]
    fn claims_matches_only_its_own_name() {
        let i = EchoInterceptor { name: "echo_me" };
        assert!(i.claims("echo_me"));
        assert!(!i.claims("shell"));
        assert_eq!(i.definitions().len(), 1);
        assert_eq!(i.definitions()[0].name, "echo_me");
    }

    #[test]
    fn with_synthetic_tool_rejects_duplicate_name() {
        use std::sync::Arc;
        let provider: Arc<dyn synthia_provider::ModelProvider> =
            Arc::new(synthia_test_support::FakeProvider::new(Vec::new()));
        let result = crate::agent::re_act::ReActAgent::new(
            provider,
            Arc::new(synthia_tool::ToolRegistry::new()),
        )
        .with_synthetic_tool(Arc::new(EchoInterceptor { name: "echo_me" }))
        .and_then(|a| {
            a.with_synthetic_tool(Arc::new(EchoInterceptor { name: "echo_me" }))
        });
        assert!(result.is_err());
    }

    #[test]
    fn with_synthetic_tool_accepts_distinct_names() {
        use std::sync::Arc;
        let provider: Arc<dyn synthia_provider::ModelProvider> =
            Arc::new(synthia_test_support::FakeProvider::new(Vec::new()));
        let result = crate::agent::re_act::ReActAgent::new(
            provider,
            Arc::new(synthia_tool::ToolRegistry::new()),
        )
        .with_synthetic_tool(Arc::new(EchoInterceptor { name: "echo_me" }))
        .and_then(|a| {
            a.with_synthetic_tool(Arc::new(EchoInterceptor {
                name: "echo_other",
            }))
        });
        assert!(result.is_ok());
    }
}
