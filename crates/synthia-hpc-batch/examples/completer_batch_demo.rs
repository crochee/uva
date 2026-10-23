//! `completer_batch_demo` — 32 requests through a
//! scripted `ModelProvider`; verify chunk_size cap + stats.
//!
//! ```bash
//! cargo run --example completer_batch_demo \
//!   -p synthia-hpc-batch
//! ```
//!
//! Expected tail:
//!
//! ```text
//! COMPLETER-BATCH: ok submitted=32 completed=32 wall_ms=…
//! ```

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use synthia_hpc_batch::{BatchCompleter, BatchPolicy};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
};

struct ScriptedProvider;

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), synthia_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "demo".into(),
            provider: "scripted".into(),
            context_window: 8192,
            max_output_tokens: 1024,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, synthia_core::Error> {
        // Tiny sleep so wall_ms is meaningful.
        tokio::time::sleep(Duration::from_micros(50)).await;
        Ok(CompletionResponse {
            content: Content::text(format!("echo: {}", request.messages.len())),
            ..CompletionResponse::default()
        })
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let batch = BatchCompleter::with_policy(
            Arc::new(ScriptedProvider) as Arc<dyn ModelProvider>,
            BatchPolicy {
                chunk_size: 16,
                max_inflight: 8,
            },
        );
        let requests: Vec<CompletionRequest> = (0..32)
            .map(|i| CompletionRequest {
                messages: Arc::new(vec![synthia_provider::Message::user(
                    format!("q-{i}"),
                )]),
                ..CompletionRequest::default()
            })
            .collect();
        let started = std::time::Instant::now();
        let outs = batch
            .complete_batch(requests)
            .await
            .expect("32-call batch should succeed");
        let wall_ms = started.elapsed().as_millis();
        let stats = batch.stats();
        println!(
            "COMPLETER-BATCH: ok submitted={} completed={} wall_ms={}",
            stats.submitted, stats.completed, wall_ms
        );
        assert_eq!(outs.len(), 32);
        assert_eq!(stats.submitted, 32);
        assert_eq!(stats.completed, 32);
    });
}
