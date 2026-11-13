//! `provider_embedder` — round-trip an embedding through a
//! `ModelProviderEmbedder` wired to a stub provider.
//!
//! Requires `cargo run --features provider`.

fn main() {
    #[cfg(feature = "provider")]
    {
        use std::sync::Arc;

        use uva_search::{Embedder, ModelProviderEmbedder};

        struct StubProvider;
        #[async_trait::async_trait]
        impl uva_provider::ModelProvider for StubProvider {
            fn name(&self) -> &str {
                "stub"
            }

            fn model_config(&self) -> uva_provider::ModelConfig {
                uva_provider::ModelConfig {
                    name: "stub".into(),
                    provider: "stub".into(),
                    context_window: 8192,
                    max_output_tokens: 1024,
                    supports_tools: false,
                    supports_streaming: false,
                    supports_reasoning: false,
                }
            }

            async fn initialize(
                &mut self,
                _: uva_provider::ProviderConfig,
            ) -> uva_core::Result<()> {
                Ok(())
            }

            async fn complete(
                &self,
                _: uva_provider::CompletionRequest,
            ) -> uva_core::Result<uva_provider::CompletionResponse>
            {
                unreachable!("complete not exercised in this example")
            }

            async fn embed(
                &self,
                _texts: Vec<String>,
            ) -> uva_core::Result<Vec<Vec<f64>>> {
                Ok(vec![vec![0.1; 16]])
            }
        }

        let provider: Arc<dyn uva_provider::ModelProvider> =
            Arc::new(StubProvider);
        let emb = ModelProviderEmbedder::new(provider, 16);
        let v = emb.embed("hello");
        assert_eq!(v.len(), 16);
        println!("provider_embedder OK; dim={}", emb.dim());
    }

    #[cfg(not(feature = "provider"))]
    {
        eprintln!("this example requires --features provider");
    }
}
