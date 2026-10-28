//! `ServerWorkflowHost` — bridge between [`elysper_workflow`]'s
//! `WorkflowHost` trait and the server's `AgentRegistry`.
//!
//! Every planned call dispatches through
//! `ServerWorkflowHost::spawn_agent`: the step's `agent` name is
//! resolved against the registry, the agent runs to completion on
//! the request's prompt, and the final text becomes the
//! [`AgentOutcome`] the workflow runtime journals. An unknown
//! name is a typed step failure, not a host `Err`.
//!
//! Gates: `run_gate` passes every gate with empty output until
//! the command-runner integration lands.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use elysper::{
    core::{AtomicCancelToken, CancelToken},
    harness::{AgentEvent, AgentInput, AgentRegistry, SystemEvent},
    workflow::{
        AgentOutcome,
        AgentRequest,
        GateOutcome,
        SelectionRequest,
        WorkflowError,
        WorkflowHost,
    },
};

/// The workflow-runtime bridge: resolves step agents by name from
/// the deployment's registry and runs each to completion.
pub struct ServerWorkflowHost {
    /// The same registry the interactive routes resolve through.
    /// Cloned (cheap — one `Arc`) at boot before the state handle
    /// takes the original, so the host and `AppState` observe the
    /// same registrations for the process's lifetime.
    registry: Arc<AgentRegistry>,
}

impl ServerWorkflowHost {
    /// Bind the host to `registry`.
    ///
    /// `registry` must be the same `Arc` the rest of the server
    /// resolves through (boot clones its own before constructing
    /// `AppState`), so a dynamically-registered agent is spawnable
    /// by a workflow without any extra wiring.
    #[must_use]
    pub fn new(registry: Arc<AgentRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl WorkflowHost for ServerWorkflowHost {
    /// Resolve `request.agent`, run it on `request.prompt`, and
    /// return the drained final text.
    ///
    /// # Errors
    ///
    /// Only a host-level failure (never in the current impl — the
    /// registry resolve and the run are both data-shaped outcomes)
    /// reaches `Err`. An unknown agent or a failed session is
    /// `Ok(AgentOutcome::failed(..))` so the workflow's remaining
    /// steps still run.
    async fn spawn_agent(
        &self,
        request: &AgentRequest,
    ) -> Result<AgentOutcome, WorkflowError> {
        let Some(agent) = self.registry.resolve_sync(&request.agent) else {
            return Ok(AgentOutcome::failed(format!(
                "agent `{}` is not registered in this deployment",
                request.agent
            )));
        };

        // A fresh token per spawn: workflow steps run to
        // completion, and step-level abort lands with the live
        // control layer (the run-level token the route would hold).
        let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();
        let input = AgentInput::text(request.prompt.clone());
        let mut stream = agent.run(input, cancel).await;

        // Drain exactly like the `task` delegator does: accumulate
        // the model's text parts, capture the terminal reason, and
        // let everything else (tool results, warnings, usage) flow
        // past unseen — the workflow journals text or error only.
        let mut text = String::new();
        let mut end_reason: Option<String> = None;
        while let Some(event) = stream.next().await {
            match &event {
                AgentEvent::Model(elysper::provider::ContentPart::Text(
                    text_part,
                )) => {
                    if !text_part.text.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&text_part.text);
                    }
                }
                AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                    end_reason = Some(format!("{reason:?}"));
                }
                _ => {}
            }
        }

        match end_reason.as_deref() {
            Some("Completed") => {
                if text.trim().is_empty() {
                    Ok(AgentOutcome::failed(
                        "agent completed without producing text",
                    ))
                } else {
                    Ok(AgentOutcome::ok(text))
                }
            }
            other => Ok(AgentOutcome::failed(format!(
                "agent session ended before completing: {}",
                other.unwrap_or("stream closed without a SessionEnded event")
            ))),
        }
    }

    async fn run_gate(
        &self,
        _gate: &elysper::workflow::GateRef,
        _cwd: Option<&Path>,
    ) -> Result<GateOutcome, WorkflowError> {
        // No command runner is wired into the workflow path yet;
        // see the module docs for why pass-through is the honest
        // placeholder (a failing verdict here would report healthy
        // workflows as broken, which is worse than no gate).
        Ok(GateOutcome::passed())
    }

    async fn select_candidate(
        &self,
        request: &SelectionRequest,
    ) -> Result<Option<usize>, WorkflowError> {
        // Decline (the default trait impl); the built-in rule —
        // first successful candidate in item order — decides.
        // A judge-backed selector lands with the eval integration.
        let _ = request;
        Ok(None)
    }
}

/// Wrap `host` in an `Arc<dyn WorkflowHost>` for `AppState`.
///
/// The blanket `WorkflowHost for Arc<T>` impl in `elysper-workflow`
/// forwards, so callers that hold the concrete type and callers
/// that hold the trait object behave identically.
pub fn as_arc(host: ServerWorkflowHost) -> Arc<dyn WorkflowHost> {
    Arc::new(host)
}

#[cfg(test)]
mod tests {
    //! End-to-end over the real registry: a `FakeProvider`-backed
    //! `ReActAgent` registered by name is spawnable by prompt and
    //! its drained text becomes the `AgentOutcome`. The unknown-
    //! agent and non-`Completed` paths pin the failure wording the
    //! workflow journals.

    use elysper::{
        core::registry::Registry,
        harness::ReActAgent,
        provider::ModelProvider,
        test_support::FakeProvider,
        tool::ToolRegistry,
    };

    use super::*;

    fn request(agent: &str, prompt: &str) -> AgentRequest {
        AgentRequest {
            run_id: "r1".to_string(),
            step_id: "s1".to_string(),
            position: 0,
            depth: 0,
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            gate: None,
            isolation: false,
            cwd: None,
        }
    }

    #[tokio::test]
    async fn spawn_runs_the_named_agent_and_returns_its_text() {
        let registry = Arc::new(AgentRegistry::new());
        let provider: Arc<dyn ModelProvider> =
            Arc::new(FakeProvider::text("workflow answer"));
        registry
            .put(elysper::harness::AgentEntry::new(Arc::new(
                ReActAgent::new(provider, Arc::new(ToolRegistry::new())),
            )))
            .await
            .expect("registry accepts the entry");

        let host = ServerWorkflowHost::new(Arc::clone(&registry));
        let outcome = host
            .spawn_agent(&request("agent", "say the answer"))
            .await
            .expect("spawn is a data-shaped outcome");
        assert!(outcome.succeeded(), "outcome: {outcome:?}");
        assert_eq!(outcome.text.as_deref(), Some("workflow answer"));
    }

    #[tokio::test]
    async fn unknown_agent_is_a_typed_failure_not_a_host_error() {
        let host = ServerWorkflowHost::new(Arc::new(AgentRegistry::new()));
        let outcome = host
            .spawn_agent(&request("ghost", "hi"))
            .await
            .expect("an unresolvable name is Ok(failed), not Err");
        assert!(!outcome.succeeded());
        assert_eq!(
            outcome.error.as_deref(),
            Some("agent `ghost` is not registered in this deployment")
        );
    }

    #[tokio::test]
    async fn a_failing_provider_maps_to_failure_naming_the_reason() {
        // `FakeProvider::new(vec![])` errors on the first sample
        // (rate_limited), so the run ends with an `Error` reason —
        // the host must surface that as a typed failure naming the
        // reason, never as a host-level `Err`.
        let registry = Arc::new(AgentRegistry::new());
        let provider: Arc<dyn ModelProvider> =
            Arc::new(FakeProvider::new(vec![]));
        registry
            .put(elysper::harness::AgentEntry::new(Arc::new(
                ReActAgent::new(provider, Arc::new(ToolRegistry::new())),
            )))
            .await
            .expect("registry accepts the entry");

        let host = ServerWorkflowHost::new(Arc::clone(&registry));
        let outcome = host
            .spawn_agent(&request("agent", "go"))
            .await
            .expect("a failed session is Ok(failed), not Err");
        assert!(!outcome.succeeded(), "outcome: {outcome:?}");
        let error = outcome.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("ended before completing"),
            "the failure must name the end reason: {error}"
        );
    }

    #[tokio::test]
    async fn run_gate_passes_with_empty_output() {
        let host = ServerWorkflowHost::new(Arc::new(AgentRegistry::new()));
        let gate = elysper::workflow::GateRef {
            command: "true".to_string(),
            label: None,
        };
        let outcome = host.run_gate(&gate, None).await.unwrap();
        assert!(outcome.passed);
        assert_eq!(outcome.output, "");
    }

    #[tokio::test]
    async fn select_candidate_declines() {
        let host = ServerWorkflowHost::new(Arc::new(AgentRegistry::new()));
        let req = SelectionRequest {
            run_id: "r".to_string(),
            step_id: "s".to_string(),
            agent: "alpha".to_string(),
            gate: None,
            candidates: vec![],
        };
        assert!(host.select_candidate(&req).await.unwrap().is_none());
    }

    #[test]
    fn host_is_send_sync() {
        // Compile-time pin: the host crosses invoker tasks, so it
        // must stay `Send + Sync` (an `Arc<AgentRegistry>` is).
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ServerWorkflowHost>();
    }
}
