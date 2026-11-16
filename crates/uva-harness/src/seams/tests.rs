//! Tests for the seam chain dispatch: foreign middleware, the host
//! frontend, and the observation points.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::channel::mpsc;
use uva_hook::HookPayload;
use uva_kernel::{EventValue, Outcome, WaterfallCtx};
use uva_steering::{HookDecision, HookEvent, RequestParams};

use super::{LLM_REQUEST, STEP_PRE, SeamRuling, Seams, TOOL_PRE};
use crate::agent::{
    EventSink,
    host::{Hook, HookContext, Host},
};

struct Fixture {
    state: uva_context::AgentState,
    sink: EventSink,
    cancel: Arc<dyn uva_core::CancelToken>,
}

impl Fixture {
    fn new() -> Self {
        let (tx, _rx) = mpsc::unbounded();
        Self {
            state: uva_context::AgentState::with_window(1_000),
            sink: EventSink::new(Arc::new(tx), None),
            cancel: uva_core::AtomicCancelToken::shared(),
        }
    }

    fn ctx(&self) -> HookContext<'_> {
        HookContext {
            state: &self.state,
            cancel: &self.cancel,
        }
    }
}

fn short_circuits_with(ruling: SeamRuling) -> uva_kernel::Middleware {
    Arc::new(move |_ctx, _next| {
        let ruling = ruling.clone();
        Box::pin(
            async move { Ok(Outcome::ShortCircuit(EventValue::new(ruling))) },
        )
    })
}

struct CountingHook {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Hook for CountingHook {
    fn name(&self) -> &str {
        "counting"
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        HookDecision::Allow
    }
}

#[tokio::test]
async fn a_foreign_block_on_step_pre_decides_the_pass() {
    let host = Host::new();
    let chain = host.waterfall(STEP_PRE).expect("step-pre chain");
    let _guard = chain
        .register(short_circuits_with(SeamRuling::Block {
            reason: "no model today".to_owned(),
        }))
        .expect("register blocks");
    let seams = Seams::new(host);

    let fixture = Fixture::new();
    let arbitration =
        seams.step_pre(&fixture.ctx(), &fixture.sink, "a", 0).await;
    assert!(
        matches!(arbitration.decision, HookDecision::Block { .. }),
        "a foreign block decides the pass: {arbitration:?}"
    );
}

#[tokio::test]
async fn a_foreign_ask_collects_and_the_host_hooks_still_run() {
    let mut host = Host::new();
    let chain = host.waterfall(TOOL_PRE).expect("tool-pre chain");
    let _guard = chain
        .register(Arc::new(|ctx, next| {
            Box::pin(async move {
                if let Some(pass) = HookPayload::payload(&ctx) {
                    pass.ask("high risk");
                }
                next(ctx).await
            })
        }))
        .expect("register ask");
    let calls = Arc::new(AtomicUsize::new(0));
    let _registration = host.hook(Arc::new(CountingHook {
        calls: Arc::clone(&calls),
    }));
    let seams = Seams::new(host);

    let fixture = Fixture::new();
    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(arbitration.asks, vec!["high risk".to_owned()]);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the host hooks run after the foreign chain: {arbitration:?}"
    );
}

#[tokio::test]
async fn a_foreign_rewrite_on_llm_request_rewrites_the_params() {
    let host = Host::new();
    let chain = host.waterfall(LLM_REQUEST).expect("llm-request chain");
    let _guard = chain
        .register(Arc::new(|ctx, next| {
            Box::pin(async move {
                let rewritten = HookPayload::payload(&ctx).map(|pass| {
                    let event = pass.event().clone();
                    match event {
                        HookEvent::BeforeRequest {
                            provider,
                            mut params,
                        } => {
                            params.model = "rewritten-model".to_owned();
                            HookEvent::BeforeRequest { provider, params }
                        }
                        other => other,
                    }
                });
                match rewritten {
                    Some(event) => {
                        next(HookPayload::rewrite(&ctx, event)).await
                    }
                    None => next(ctx).await,
                }
            })
        }))
        .expect("register rewriter");
    // `_guard` above keeps the middleware registered for the pass.
    let seams = Seams::new(host);

    let fixture = Fixture::new();
    let mut params =
        RequestParams::from_request(&uva_provider::CompletionRequest {
            model: "original-model".to_owned(),
            ..uva_provider::CompletionRequest::default()
        });
    let arbitration = seams
        .llm_request(&fixture.ctx(), &fixture.sink, "p", &mut params)
        .await;
    assert_eq!(params.model, "rewritten-model");
    assert!(
        !arbitration.decided(),
        "a rewrite is not a decision: {arbitration:?}"
    );
}

#[tokio::test]
async fn a_panicking_foreign_middleware_on_tool_pre_is_a_block() {
    let host = Host::new();
    let chain = host.waterfall(TOOL_PRE).expect("tool-pre chain");
    let _guard = chain
        .register(Arc::new(|_ctx, _next| {
            Box::pin(async move { panic!("broken middleware") })
        }))
        .expect("register panic");
    let seams = Seams::new(host);

    let fixture = Fixture::new();
    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert!(
        matches!(arbitration.decision, HookDecision::Block { .. }),
        "a broken foreign middleware fails closed: {arbitration:?}"
    );
}

struct FixedHook {
    decision: HookDecision,
}

#[async_trait]
impl Hook for FixedHook {
    fn name(&self) -> &str {
        "fixed"
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.decision.clone()
    }
}

struct BlockingGate;

#[async_trait]
impl crate::agent::host::Gate for BlockingGate {
    fn name(&self) -> &str {
        "blocking"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Ok(HookDecision::Block {
            reason: "denied".to_owned(),
        })
    }
}

struct RenamingTransformer;

#[async_trait]
impl crate::agent::host::RequestTransformer for RenamingTransformer {
    fn name(&self) -> &str {
        "renamer"
    }

    async fn transform(
        &self,
        params: &mut uva_steering::RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        params.model = "transformed-model".to_owned();
        Ok(())
    }
}

#[tokio::test]
async fn host_gate_registrations_land_on_the_tool_pre_chain() {
    let mut host = Host::new();
    let _registration = host.gate(Arc::new(BlockingGate));
    assert_eq!(
        host.waterfall(TOOL_PRE)
            .map(|chain| chain.len())
            .unwrap_or(0),
        1,
        "a gate registration is chain middleware"
    );
    let seams = Seams::new(host);
    let fixture = Fixture::new();
    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert!(
        matches!(arbitration.decision, HookDecision::Block { .. }),
        "the gate still fails closed through the chain: {arbitration:?}"
    );
}

#[tokio::test]
async fn host_hooks_arbitrate_through_the_chain_in_order() {
    let mut host = Host::new();
    let _ask = host.hook(Arc::new(FixedHook {
        decision: HookDecision::Ask {
            reason: "needs approval".to_owned(),
        },
    }));
    let _block = host.hook(Arc::new(FixedHook {
        decision: HookDecision::Block {
            reason: "denied".to_owned(),
        },
    }));
    assert_eq!(
        host.waterfall(TOOL_PRE)
            .map(|chain| chain.len())
            .unwrap_or(0),
        2,
        "hook registrations are chain middleware"
    );
    let seams = Seams::new(host);
    let fixture = Fixture::new();
    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(
        arbitration.asks,
        vec!["needs approval".to_owned()],
        "the ask is collected: {arbitration:?}"
    );
    assert!(matches!(arbitration.decision, HookDecision::Block { .. }));
}

#[tokio::test]
async fn request_transformers_land_on_the_llm_request_chain() {
    let mut host = Host::new();
    let _registration = host.request_transformer(Arc::new(RenamingTransformer));
    assert_eq!(
        host.waterfall(LLM_REQUEST)
            .map(|chain| chain.len())
            .unwrap_or(0),
        1,
        "a request-transformer registration is chain middleware"
    );
    let seams = Seams::new(host);
    let fixture = Fixture::new();
    let mut params =
        RequestParams::from_request(&uva_provider::CompletionRequest {
            model: "original-model".to_owned(),
            ..uva_provider::CompletionRequest::default()
        });
    seams
        .llm_request(&fixture.ctx(), &fixture.sink, "p", &mut params)
        .await;
    assert_eq!(params.model, "transformed-model");
}

#[tokio::test]
async fn observation_points_still_dispatch_registered_hooks() {
    let mut host = Host::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _registration = host.hook(Arc::new(CountingHook {
        calls: Arc::clone(&calls),
    }));
    let seams = Seams::new(host);
    let fixture = Fixture::new();
    seams
        .run_started(&fixture.ctx(), &fixture.sink, "a", 0)
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the observation points keep their typed dispatch"
    );
}

#[test]
fn a_pass_rewrite_builds_a_new_chain_context() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let pass = Arc::new(HookPayload::new(
        HookEvent::BeforeStep {
            agent_name: "a".to_owned(),
            run_idx: 0,
            iteration: 0,
        },
        &ctx,
    ));
    let ctx = Arc::new(WaterfallCtx::new(
        STEP_PRE,
        EventValue::from_arc(pass),
        uva_kernel::Context::new(),
    ));
    let rewritten = HookPayload::rewrite(
        &ctx,
        HookEvent::BeforeStep {
            agent_name: "b".to_owned(),
            run_idx: 0,
            iteration: 0,
        },
    );
    let payload = HookPayload::payload(&rewritten).expect("carries a pass");
    assert!(matches!(
        payload.event(),
        HookEvent::BeforeStep { agent_name, .. } if agent_name == "b"
    ));
    assert_eq!(rewritten.event(), STEP_PRE);
}

// The registry's registrations run before the host's typed ones, and
// a registry block stops the host pass: one arbitration, first
// decision wins.
#[tokio::test]
async fn the_registry_runs_first_and_its_block_stops_the_host_pass() {
    struct RegistryBlock;

    #[async_trait]
    impl crate::agent::host::Gate for RegistryBlock {
        fn name(&self) -> &str {
            "registry-block"
        }

        async fn check(
            &self,
            _event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> Result<HookDecision, String> {
            Ok(HookDecision::Block {
                reason: "the registry denied the call".to_owned(),
            })
        }
    }

    let mut host = Host::new();
    let host_calls = Arc::new(AtomicUsize::new(0));
    let _host_hook = host.hook(Arc::new(CountingHook {
        calls: Arc::clone(&host_calls),
    }));
    let registry = uva_hook::HookRegistry::new();
    let _external = registry
        .register_gate(TOOL_PRE, Arc::new(RegistryBlock))
        .expect("the external gate registers");
    let seams = Seams::new(host).with_hook_registry(registry);

    let fixture = Fixture::new();
    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(
        arbitration.decision,
        HookDecision::Block {
            reason: "the registry denied the call".to_owned()
        }
    );
    assert_eq!(
        host_calls.load(Ordering::SeqCst),
        0,
        "a registry denial short-circuits the host pass: {arbitration:?}"
    );
}

// Both surfaces fold into one arbitration: the external asks come
// first, the host's asks follow, and neither end the pass.
#[tokio::test]
async fn external_and_host_asks_collect_into_one_arbitration() {
    let mut host = Host::new();
    let _host_ask = host.hook(Arc::new(FixedHook {
        decision: HookDecision::Ask {
            reason: "host ask".to_owned(),
        },
    }));
    let registry = uva_hook::HookRegistry::new();
    let _external_ask = registry
        .register_hook(
            TOOL_PRE,
            Arc::new(FixedHook {
                decision: HookDecision::Ask {
                    reason: "external ask".to_owned(),
                },
            }),
        )
        .expect("the external hook registers");
    let seams = Seams::new(host).with_hook_registry(registry);

    let fixture = Fixture::new();
    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(
        arbitration.asks,
        vec!["external ask".to_owned(), "host ask".to_owned()],
        "registry first, host second, one collected report"
    );
    assert!(!arbitration.decided());
}

// A dropped registration withdraws from the seam layer too.
#[tokio::test]
async fn dropping_a_registration_withdraws_it_from_the_pass() {
    let registry = uva_hook::HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let registration = registry
        .register_hook(
            TOOL_PRE,
            Arc::new(CountingHook {
                calls: Arc::clone(&calls),
            }),
        )
        .expect("the external hook registers");
    let seams = Seams::new(Host::new()).with_hook_registry(registry);

    let fixture = Fixture::new();
    seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(registration);
    seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the withdrawn registration does not run again"
    );
}
