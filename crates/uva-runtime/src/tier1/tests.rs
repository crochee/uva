use std::sync::Arc;

use uva_core::sleeper::FixedSleeper;
use uva_provider::{config::WorkspaceConfig, traits::ModelProvider};
use uva_test_support::FakeProvider;

use super::{ModelAdapterConfig, wrap_tier1_providers};

fn sleeper() -> Arc<dyn uva_core::sleeper::Sleeper> {
    Arc::new(FixedSleeper)
}

fn config(cache: bool, resilience: bool, budget: bool) -> ModelAdapterConfig {
    ModelAdapterConfig {
        workspace: WorkspaceConfig::default(),
        cache: cache.then(|| super::super::model_adapter::CacheSection {
            enabled: true,
            ..super::super::model_adapter::CacheSection::default()
        }),
        resilience: resilience.then(|| {
            super::super::model_adapter::ResilienceSection {
                enabled: true,
                ..super::super::model_adapter::ResilienceSection::default()
            }
        }),
        budget: budget.then(|| super::super::model_adapter::BudgetSection {
            enabled: true,
            ..super::super::model_adapter::BudgetSection::default()
        }),
    }
}

#[test]
fn no_sections_enabled_returns_the_inner_handle_unchanged() {
    let inner: Arc<dyn ModelProvider> = Arc::new(FakeProvider::text("raw"));
    let wrapped = wrap_tier1_providers(
        Arc::clone(&inner),
        &config(false, false, false),
        uva_core::SharedClock::system(),
        sleeper(),
    );
    assert!(
        Arc::ptr_eq(&wrapped, &inner),
        "with every section off the chain must be a no-op"
    );
}

#[test]
fn any_section_enabled_wraps_the_inner_handle() {
    for (cache, resilience, budget) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let inner: Arc<dyn ModelProvider> = Arc::new(FakeProvider::text("raw"));
        let wrapped = wrap_tier1_providers(
            Arc::clone(&inner),
            &config(cache, resilience, budget),
            uva_core::SharedClock::system(),
            sleeper(),
        );
        assert!(
            !Arc::ptr_eq(&wrapped, &inner),
            "cache={cache} resilience={resilience} budget={budget} \
             must wrap"
        );
    }
}

#[cfg(test)]
mod behavior_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use uva_core::SharedClock;
    use uva_core_tokio::TokioSleeper;
    use uva_provider::{
        CompletionRequest,
        CompletionResponse,
        Content,
        ContentPart,
        ModelConfig,
        ModelProvider,
        ProviderConfig,
        TextContent,
        TokenUsage,
        config::WorkspaceConfig,
    };

    use super::wrap_tier1_providers;
    use crate::model_adapter::{
        BudgetSection,
        CacheSection,
        ModelAdapterConfig,
        ResilienceSection,
    };

    /// In-process fake that counts `complete` calls.
    struct CountingProvider {
        calls: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for CountingProvider {
        async fn initialize(
            &mut self,
            _config: ProviderConfig,
        ) -> Result<(), uva_core::Error> {
            Ok(())
        }

        fn name(&self) -> &str {
            "counting"
        }

        fn model_config(&self) -> ModelConfig {
            ModelConfig {
                name: "counting".into(),
                provider: "counting".into(),
                context_window: 8_192,
                max_output_tokens: 1_024,
                supports_tools: false,
                supports_streaming: false,
                supports_reasoning: false,
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, uva_core::Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionResponse {
                id: "r".into(),
                model: "counting".into(),
                content: Content::Single(ContentPart::Text(TextContent {
                    text: "ok".into(),
                    cache_control: None,
                })),
                usage: TokenUsage::default(),
                cached: false,
                stop_reason: None,
                replay_state: None,
            })
        }
    }

    fn request() -> CompletionRequest {
        use uva_provider::{Message, Role};
        CompletionRequest {
            model: "counting".into(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Content::Single(ContentPart::Text(TextContent {
                    text: "hello".into(),
                    cache_control: None,
                })),
                tool_call_id: None,
                name: None,
                tool_result_cleared_at: None,
            }]),
            tools: Arc::new(Vec::new()),
            ..CompletionRequest::default()
        }
    }

    fn config(
        cache: Option<CacheSection>,
        resilience: Option<ResilienceSection>,
        budget: Option<BudgetSection>,
    ) -> ModelAdapterConfig {
        ModelAdapterConfig {
            workspace: WorkspaceConfig::default(),
            cache,
            resilience,
            budget,
        }
    }

    fn tokio_sleeper() -> Arc<dyn uva_core::sleeper::Sleeper> {
        Arc::new(TokioSleeper)
    }

    /// All wrappers off ⇒ transparent pass-through: same `name()`,
    /// every call goes through.
    #[tokio::test]
    async fn tier1_all_off_is_passthrough() {
        let calls = Arc::new(AtomicU64::new(0));
        let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider {
            calls: calls.clone(),
        });
        let wrapped = wrap_tier1_providers(
            inner,
            &config(None, None, None),
            SharedClock::system(),
            tokio_sleeper(),
        );
        assert_eq!(wrapped.name(), "counting");
        let _ = wrapped.complete(request()).await.unwrap();
        let _ = wrapped.complete(request()).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "no wrapper installed ⇒ both calls reach the inner provider"
        );
    }

    /// Cache on ⇒ second identical call is served from the cache and
    /// the inner provider is only called once.
    #[tokio::test]
    async fn tier1_cache_serves_second_identical_call() {
        let calls = Arc::new(AtomicU64::new(0));
        let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider {
            calls: calls.clone(),
        });
        let cache = CacheSection {
            enabled: true,
            threshold: 0.99,
            ..CacheSection::default()
        };
        let wrapped = wrap_tier1_providers(
            inner,
            &config(Some(cache), None, None),
            SharedClock::system(),
            tokio_sleeper(),
        );
        let first = wrapped.complete(request()).await.unwrap();
        let second = wrapped.complete(request()).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "cache must short-circuit the second identical request"
        );
        assert!(!first.cached, "first response is a fresh inner call");
        assert!(second.cached, "second response is cache-served");
    }

    /// Budget on ⇒ the first call trips the cap.
    #[tokio::test]
    async fn tier1_budget_trips() {
        let calls = Arc::new(AtomicU64::new(0));
        let inner: Arc<dyn ModelProvider> = Arc::new(CountingProvider {
            calls: calls.clone(),
        });
        let budget = BudgetSection {
            enabled: true,
            tokens_limit: 0, // exceeded from the start
            ..BudgetSection::default()
        };
        let wrapped = wrap_tier1_providers(
            inner,
            &config(None, None, Some(budget)),
            SharedClock::system(),
            tokio_sleeper(),
        );
        let r = wrapped.complete(request()).await;
        assert!(r.is_err(), "budget must reject the call");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "budget rejects before consulting the inner provider"
        );
    }

    /// Always fails with a retryable error, so the resilience wrapper
    /// retries and waits through its backoff.
    struct RateLimitedProvider {
        calls: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for RateLimitedProvider {
        async fn initialize(
            &mut self,
            _config: ProviderConfig,
        ) -> Result<(), uva_core::Error> {
            Ok(())
        }

        fn name(&self) -> &str {
            "rate-limited"
        }

        fn model_config(&self) -> ModelConfig {
            ModelConfig {
                name: "rate-limited".into(),
                provider: "rate-limited".into(),
                context_window: 8_192,
                max_output_tokens: 1_024,
                supports_tools: false,
                supports_streaming: false,
                supports_reasoning: false,
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, uva_core::Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(uva_core::Error::RateLimited { retry_after: None })
        }
    }

    /// Resilience on ⇒ the retry backoff waits on the injected
    /// sleeper's timer, not on an OS thread.
    ///
    /// The proof is the *virtual* clock: under `start_paused = true`
    /// the tokio clock only moves when a wait is registered on it —
    /// a backoff awaited through `TokioSleeper` moves it, a
    /// std-thread wait leaves it still.
    #[tokio::test(start_paused = true)]
    async fn tier1_resilience_backoff_waits_on_the_tokio_timer() {
        let calls = Arc::new(AtomicU64::new(0));
        let inner: Arc<dyn ModelProvider> = Arc::new(RateLimitedProvider {
            calls: calls.clone(),
        });
        let resilience = ResilienceSection {
            enabled: true,
            max_attempts: 2, // one retry
            ..ResilienceSection::default()
        };
        let wrapped = wrap_tier1_providers(
            inner,
            &config(None, Some(resilience), None),
            SharedClock::system(),
            tokio_sleeper(),
        );

        let virtual_before = tokio::time::Instant::now();
        let real_before = std::time::Instant::now();
        let r = wrapped.complete(request()).await;
        let virtual_elapsed = virtual_before.elapsed();
        let real_elapsed = real_before.elapsed();

        assert!(r.is_err(), "the inner provider always fails");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the wrapper made the initial call plus one retry"
        );
        assert!(
            virtual_elapsed > std::time::Duration::ZERO,
            "the backoff must be awaited through the injected sleeper, \
         but the tokio clock did not move (virtual elapsed \
         {virtual_elapsed:?}, real elapsed {real_elapsed:?})"
        );
    }
}
