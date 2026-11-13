//! `pending_provider` and a resume run.
//!
//! A provider selection parked on the controller belongs to the turn
//! that submitted it. A resume continues an *earlier* turn, whose
//! request envelope was recorded under the session's default provider;
//! sampling a newly parked selection there would report `ConfigDrift`
//! against the resume token. The selection therefore stays parked for
//! the next ordinary turn, exactly as a multimodal payload does.

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use uva::{
    harness::ResumeToken,
    provider::{
        CompletionRequest,
        CompletionResponse,
        ModelConfig,
        ProviderConfig,
        traits::ModelProvider,
    },
};

use super::{
    super::{RunStreamFactory, SessionOp},
    support::{
        RecordedCall,
        VecFactory,
        make_manager_and_controller_with_deps,
        test_deps,
        wait_for_runs,
    },
};

/// A provider whose name is the only thing a test needs to observe.
#[derive(Debug)]
struct NamedProvider(&'static str);

#[async_trait::async_trait]
impl ModelProvider for NamedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), uva::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        self.0
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: format!("{}-model", self.0),
            provider: self.0.to_string(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, uva::core::Error> {
        Ok(CompletionResponse::default())
    }

    async fn embed(
        &self,
        _texts: Vec<String>,
    ) -> Result<Vec<Vec<f64>>, uva::core::Error> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn a_resume_leaves_a_parked_provider_for_the_next_turn() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> =
        Arc::new(VecFactory::new(Vec::new(), Arc::clone(&calls)));
    let mut deps = test_deps();
    let default: Arc<dyn ModelProvider> = Arc::new(NamedProvider("default"));
    deps.provider = Arc::clone(&default);
    let (controller, _manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        factory,
        deps,
    )
    .await;
    let inner = controller.inner.as_ref().expect("a local controller");

    // A selection parked while the session is paused: it belongs to
    // the next turn, not to the resume.
    let parked: Arc<dyn ModelProvider> = Arc::new(NamedProvider("next-turn"));
    *inner.pending_provider.lock() =
        Some(super::super::PinnedProvider::new(Arc::clone(&parked)));

    controller
        .submit(SessionOp::Resume {
            token: ResumeToken::new(7, Vec::new(), "sha256:test"),
            decisions: Vec::new(),
            allow_drift: false,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 1).await;

    {
        let recorded = calls.lock();
        assert_eq!(
            recorded[0].config.provider.name(),
            "default",
            "the resume must sample the provider the suspension was \
             recorded under"
        );
    }
    assert!(
        inner.pending_provider.lock().is_some(),
        "the parked selection must survive the resume"
    );

    // ...and it still governs the next ordinary turn.
    controller
        .submit(SessionOp::Prompt {
            content: "next turn".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 2).await;
    assert_eq!(
        calls.lock()[1].config.provider.name(),
        "next-turn",
        "the parked selection belongs to the turn after the resume"
    );
}

/// A resolver that answers one recorded pair.
struct FixedResolver;

impl super::super::ProviderResolver for FixedResolver {
    fn resolve(
        &self,
        provider: &str,
        _model: &str,
    ) -> Option<Arc<dyn ModelProvider>> {
        (provider == "pinned").then(|| {
            Arc::new(NamedProvider("pinned")) as Arc<dyn ModelProvider>
        })
    }
}

#[tokio::test]
async fn a_resume_samples_the_provider_the_suspension_recorded() {
    // A suspended turn continues under the provider its request
    // snapshot names. Sampling the deployment default instead would
    // change the request envelope, and the resume would be refused as
    // a configuration drift — the approval could not be answered.
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> =
        Arc::new(VecFactory::new(Vec::new(), Arc::clone(&calls)));
    let mut deps = test_deps();
    deps.provider = Arc::new(NamedProvider("default"));
    deps.provider_resolver = Some(Arc::new(FixedResolver));
    let (controller, manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        factory,
        deps,
    )
    .await;

    // The envelope the suspension was recorded under.
    manager.writer("alice", "s1").append_event_nowait(
        &uva::session::SessionEvent::RequestSnapshot {
            seq: 0,
            ts: String::new(),
            provider: "pinned".to_string(),
            model: "pinned-model".to_string(),
            params_hash: "sha256:params".to_string(),
            tools_hash: "sha256:tools".to_string(),
            system_hash: "sha256:system".to_string(),
            sections: Vec::new(),
            policy_hash: "sha256:policy".to_string(),
        },
    );
    manager
        .writer("alice", "s1")
        .flush()
        .await
        .expect("the snapshot row is durable");

    controller
        .submit(SessionOp::Resume {
            token: ResumeToken::new(7, Vec::new(), "sha256:test"),
            decisions: Vec::new(),
            allow_drift: false,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 1).await;

    assert_eq!(
        calls.lock()[0].config.provider.name(),
        "pinned",
        "the resume samples the provider the suspension recorded"
    );
}
