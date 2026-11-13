//! # A harness consumer with no runtime in its dependency tree
//!
//! The claim under test is narrow and mechanical: `uva-harness`
//! (and everything it pulls in) is usable from a program whose
//! dependency tree contains **zero** `tokio`. Not "the framework does
//! not require you to call `#[tokio::main]`" — that weaker claim holds
//! for a crate that merely links tokio without using it — but that no
//! part of the closure brings the runtime along at all.
//!
//! Run it:
//!
//! ```bash
//! cargo run --manifest-path docs/examples/harness-only-consumer/Cargo.toml
//! ```
//!
//! Expected tail: `HARNESS-ONLY: OK`
//!
//! The mechanical half of the proof lives outside the program:
//!
//! ```bash
//! cargo tree --manifest-path docs/examples/harness-only-consumer/Cargo.toml \
//!   | grep -c tokio     # must print 0
//! ```
//!
//! `make examples` runs both. A non-zero count means the harness
//! closure grew a runtime dependency and this fixture is the tripwire
//! — the fix is to remove it from the library, never to add tokio
//! here.
//!
//! What makes the run possible without a runtime:
//!
//! - [`ThreadSpawner`] is the whole executor contract — [`Spawner`] is
//!   ten lines of `std::thread::spawn` + `futures::executor::block_on`;
//! - `main` is a plain `fn main()`; every await happens inside a
//!   `block_on`;
//! - the provider is scripted, so no HTTP adapter (and its tokio SSE
//!   pump) is in the tree — `uva-provider` is taken with
//!   `default-features = false`;
//! - the only tool is this file's own [`EchoTool`]; the builtin
//!   `read` / `write` / `shell` plugins are tokio-bound and are not
//!   registered.
//!
//! `uva-core`, `uva-provider` and `uva-tool` are direct
//! dependencies because their traits are what a consumer implements
//! and the harness re-exports neither. They are the same libraries the
//! harness itself uses; the tree they produce carries no runtime.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use futures::StreamExt;
use uva_core::{
    AtomicCancelToken,
    CancelToken,
    Error,
    spawn::{BoxFuture, Spawner},
};
use uva_harness::{Agent, AgentEvent, AgentInput, ReActAgent};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    ToolUse,
};
use uva_tool::{Tool, ToolOutput, ToolRegistry, registry::ToolEntry};

/// Where detached work goes. This is the only piece a non-tokio
/// consumer has to supply, and it is the same ten lines as
/// `uva-harness/examples/runtime_agnostic.rs`.
struct ThreadSpawner;

impl Spawner for ThreadSpawner {
    fn spawn(&self, task: BoxFuture<()>) {
        thread::spawn(move || futures::executor::block_on(task));
    }

    fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        thread::spawn(f);
    }
}

/// Two scripted calls: ask for `echo`, then answer.
struct ScriptedProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let content = if call == 0 {
            Content::parts(vec![ContentPart::ToolUse(ToolUse {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                input: serde_json::json!({ "text": "no runtime" }),
            })])
        } else {
            Content::text("the harness ran with no runtime in its tree")
        };
        Ok(CompletionResponse {
            content,
            ..CompletionResponse::default()
        })
    }
}

/// A consumer-owned tool — no filesystem, no process, no timer. Nothing
/// in the harness requires one to be tokio-backed.
struct EchoTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo the `text` argument back."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &uva_tool::Context,
    ) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = input
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(missing)");
        ToolOutput::text(format!("echo: {text}"))
    }
}

fn main() {
    println!("=== uva-harness: a consumer with no tokio ===\n");

    let model_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let spawner: Arc<dyn Spawner> = Arc::new(ThreadSpawner);

    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider {
        calls: Arc::clone(&model_calls),
    });

    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(EchoTool {
        calls: Arc::clone(&tool_calls),
    })));

    let agent = ReActAgent::new(provider, Arc::new(registry))
        .with_workspace(".")
        .with_spawner(Arc::clone(&spawner))
        .with_max_iterations(4)
        .with_name("harness-only")
        .with_instructions("Answer in one short sentence.");

    let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();

    let mut stream = futures::executor::block_on(
        agent.run(AgentInput::text("Use the echo tool, then answer."), cancel),
    );

    let events = futures::executor::block_on(async move {
        let mut collected = Vec::new();
        while let Some(event) = stream.next().await {
            collected.push(event);
        }
        collected
    });

    let mut answer = String::new();
    for event in &events {
        if let AgentEvent::ModelDone(done) = event {
            answer.clone_from(&done.text);
        }
    }

    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        1,
        "the tool must run on the injected executor"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert!(
        answer.contains("no runtime in its tree"),
        "the provider's final answer must reach the caller, got {answer:?}"
    );

    println!("spawner          : std::thread + futures::executor::block_on");
    println!(
        "model crate      : uva-harness (no runtime crate in the tree)"
    );
    println!("events observed  : {}", events.len());
    println!("model calls      : {}", model_calls.load(Ordering::SeqCst));
    println!("tool calls       : {}", tool_calls.load(Ordering::SeqCst));
    println!("answer           : {answer}");
    println!("\nHARNESS-ONLY: OK");
    println!(
        "(dependency-tree check: `cargo tree | grep -c tokio` must print 0)"
    );
}
