//! Manual compaction wiring: build a
//! `SummarizingContextManager` directly and pin it through
//! `with_context_manager`. The harness-side `with_compaction_settings`
//! setter moved out to the server-side factory
//! (`synthia_server::agent_run_config::context_manager_for_compaction_with_emitters`),
//! so this file is the regression test for the canonical caller
//! path: a hand-built `SummarizingContextManager` installed through
//! `with_context_manager` is what the loop uses for the run.

use std::sync::{Arc, atomic::AtomicUsize};

use async_trait::async_trait;
use futures::StreamExt;
use synthia_context::{AgentState, ContextManager, SummarizingContextManager};
use synthia_provider::{
    ContentPart,
    SamplingResult,
    StreamChunk,
    TextContent,
    traits::ModelProvider,
};
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};

struct SpyManager {
    prepares: Arc<AtomicUsize>,
}

#[async_trait]
impl ContextManager for SpyManager {
    async fn prepare(
        &self,
        _messages: &mut Vec<Message>,
        _state: &mut AgentState,
    ) {
        self.prepares
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn answer_script() -> ScriptedStreamProvider {
    ScriptedStreamProvider::new(vec![vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: "done".to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "done".to_string(),
                ..SamplingResult::default()
            }),
        },
    ]])
}

async fn run_to_completion(agent: &ReActAgent) -> Vec<SessionEndReason> {
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut reasons = Vec::new();
    while let Some(event) = stream.next().await {
        if let AgentEvent::System(SystemEvent::SessionEnded { reason }) = event
        {
            reasons.push(reason);
        }
    }
    reasons
}

#[tokio::test]
async fn caller_supplied_manager_runs_when_set_via_with_context_manager() {
    let prepares = Arc::new(AtomicUsize::new(0));
    let spy: Arc<dyn ContextManager> = Arc::new(SpyManager {
        prepares: Arc::clone(&prepares),
    });
    let provider: Arc<dyn ModelProvider> = Arc::new(answer_script());

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_context_manager(spy);

    let reasons = run_to_completion(&agent).await;
    assert_eq!(
        reasons,
        vec![SessionEndReason::Completed],
        "the run must complete"
    );
    assert_eq!(
        prepares.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the caller-supplied manager must be the one used by the loop"
    );
}

#[tokio::test]
async fn caller_supplied_summarising_manager_runs_when_set_via_with_context_manager()
 {
    let summarise: synthia_context::SummariseFn =
        Arc::new(|_batch| Box::pin(async { None }));
    let summarising: Arc<dyn ContextManager> =
        Arc::new(SummarizingContextManager::new(summarise));
    let provider: Arc<dyn ModelProvider> = Arc::new(answer_script());

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_context_manager(summarising);

    let reasons = run_to_completion(&agent).await;
    assert_eq!(
        reasons,
        vec![SessionEndReason::Completed],
        "the run must complete with a hand-built summarising manager"
    );
}
