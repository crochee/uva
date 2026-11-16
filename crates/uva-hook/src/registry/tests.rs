//! Registry behavior: the revocable registration, the arbitration,
//! and the four registration kinds.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use uva_steering::{HookDecision, HookEvent, RequestParams};

use super::{Arbitration, HookRegistry, RegistryError};
use crate::{
    context::HookContext,
    seam::{LLM_REQUEST, LLM_RESULT, STEP_POST, STEP_PRE, TOOL_POST, TOOL_PRE},
    traits::{Gate, Hook, RequestTransformer, ResultTransformer},
};

// One dispatch's observable run state and cancellation token.
struct Fixture {
    state: uva_context::AgentState,
    cancel: Arc<dyn uva_core::CancelToken>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            state: uva_context::AgentState::with_window(1_000),
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

fn before_tool() -> HookEvent {
    HookEvent::BeforeTool {
        tool_name: "shell".to_owned(),
        call_id: "call-1".to_owned(),
        arguments: serde_json::json!({ "command": "rm -rf /" }),
    }
}

fn before_step() -> HookEvent {
    HookEvent::BeforeStep {
        agent_name: "a".to_owned(),
        run_idx: 0,
        iteration: 1,
    }
}

fn after_step() -> HookEvent {
    HookEvent::AfterStep {
        agent_name: "a".to_owned(),
        run_idx: 0,
        iteration: 1,
    }
}

fn after_tool() -> HookEvent {
    HookEvent::AfterTool {
        tool_name: "shell".to_owned(),
        call_id: "call-1".to_owned(),
        is_error: false,
    }
}

fn after_provider_call() -> HookEvent {
    HookEvent::AfterProviderCall {
        provider: "p".to_owned(),
        model: "m".to_owned(),
        prompt_tokens: 1,
        completion_tokens: 1,
        stop_reason: None,
    }
}

fn before_request() -> HookEvent {
    HookEvent::BeforeRequest {
        provider: "p".to_owned(),
        params: RequestParams::from_request(
            &uva_provider::CompletionRequest::default(),
        ),
    }
}

struct FixedHook {
    decision: HookDecision,
    calls: Arc<AtomicUsize>,
}

impl FixedHook {
    fn new(decision: HookDecision) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                decision,
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.decision.clone()
    }
}

struct FailingGate;

#[async_trait]
impl Gate for FailingGate {
    fn name(&self) -> &str {
        "failing"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Err("could not decide".to_owned())
    }
}

struct ModelRewriter;

#[async_trait]
impl RequestTransformer for ModelRewriter {
    fn name(&self) -> &str {
        "model-rewriter"
    }

    async fn transform(
        &self,
        params: &mut RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        params.model = "rewritten-model".to_owned();
        Ok(())
    }
}

struct Redactor;

#[async_trait]
impl ResultTransformer for Redactor {
    fn name(&self) -> &str {
        "redactor"
    }

    async fn transform(
        &self,
        output: &mut uva_tool::ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        output.content = uva_tool::ToolOutput::text("[redacted]").content;
        Ok(())
    }
}

#[test]
fn an_unknown_key_is_refused() {
    let registry = HookRegistry::new();
    let (hook, _calls) = FixedHook::new(HookDecision::Allow);
    let error = registry
        .register_hook("tool/execute", Arc::new(hook))
        .expect_err("not a mount point");
    assert_eq!(
        error,
        RegistryError::UnknownMountPoint("tool/execute".to_owned())
    );
}

#[test]
fn a_transformer_must_name_its_own_mount_point() {
    let registry = HookRegistry::new();
    let error = registry
        .register_request_transformer(STEP_PRE, Arc::new(ModelRewriter))
        .expect_err("step-pre cannot rewrite a request");
    assert_eq!(
        error,
        RegistryError::WrongMountPoint {
            key: STEP_PRE,
            expected: LLM_REQUEST,
        }
    );
    let error = registry
        .register_result_transformer(TOOL_PRE, Arc::new(Redactor))
        .expect_err("tool-pre cannot rewrite a result");
    assert_eq!(
        error,
        RegistryError::WrongMountPoint {
            key: TOOL_PRE,
            expected: TOOL_POST,
        }
    );
}

#[tokio::test]
async fn the_first_block_short_circuits_the_pass() {
    let registry = HookRegistry::new();
    let (blocker, _blocked_calls) = FixedHook::new(HookDecision::Block {
        reason: "no shell".to_owned(),
    });
    let _block = registry
        .register_hook(TOOL_PRE, Arc::new(blocker))
        .expect("block registers");
    let (late, late_calls) = FixedHook::new(HookDecision::Allow);
    let _late = registry
        .register_hook(TOOL_PRE, Arc::new(late))
        .expect("late registers");

    let fixture = Fixture::new();
    let pass = registry
        .dispatch(TOOL_PRE, &before_tool(), &fixture.ctx())
        .await;
    assert_eq!(
        pass.arbitration.decision,
        HookDecision::Block {
            reason: "no shell".to_owned()
        }
    );
    assert_eq!(pass.denied_by.as_deref(), Some("fixed"));
    assert!(pass.event.is_none(), "a decided pass hands no event on");
    assert_eq!(
        late_calls.load(Ordering::SeqCst),
        0,
        "registrations after the block never run"
    );
}

#[tokio::test]
async fn asks_collect_and_the_pass_continues() {
    let registry = HookRegistry::new();
    let mut handles = Vec::new();
    for reason in ["first ask", "second ask"] {
        let (hook, _calls) = FixedHook::new(HookDecision::Ask {
            reason: reason.into(),
        });
        handles.push(
            registry
                .register_hook(TOOL_PRE, Arc::new(hook))
                .expect("ask registers"),
        );
    }
    let (allow, calls) = FixedHook::new(HookDecision::Allow);
    let _allow = registry
        .register_hook(TOOL_PRE, Arc::new(allow))
        .expect("allow registers");

    let fixture = Fixture::new();
    let pass = registry
        .dispatch(TOOL_PRE, &before_tool(), &fixture.ctx())
        .await;
    assert_eq!(
        pass.arbitration.asks,
        vec!["first ask".to_owned(), "second ask".to_owned()]
    );
    assert!(!pass.arbitration.decided(), "an ask never ends the pass");
    assert!(pass.event.is_some());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the pass continues past every ask"
    );
}

#[tokio::test]
async fn terminate_is_accepted_only_where_the_table_says_so() {
    let registry = HookRegistry::new();
    let (terminal, calls) = FixedHook::new(HookDecision::Terminate {
        reason: "end it".to_owned(),
    });
    let _handle = registry
        .register_hook(STEP_PRE, Arc::new(terminal))
        .expect("terminate registers");

    let fixture = Fixture::new();
    let pass = registry
        .dispatch(STEP_PRE, &before_step(), &fixture.ctx())
        .await;
    assert_eq!(
        pass.arbitration.decision,
        HookDecision::Terminate {
            reason: "end it".to_owned()
        },
        "before-step is one of the five points the table accepts"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let registry = HookRegistry::new();
    let (terminal, calls) = FixedHook::new(HookDecision::Terminate {
        reason: "too late".to_owned(),
    });
    let _handle = registry
        .register_hook(LLM_RESULT, Arc::new(terminal))
        .expect("terminate registers");
    let pass = registry
        .dispatch(LLM_RESULT, &after_provider_call(), &fixture.ctx())
        .await;
    assert_eq!(
        pass.arbitration.decision,
        HookDecision::Allow,
        "after-provider-call is not on the table; the decision is ignored"
    );
    assert!(pass.event.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A gate that cannot answer never reads as "no opinion": at every
/// point that can refuse a pass, the failure synthesizes the decision
/// that point accepts — `Block` before a tool call, `Terminate` at the
/// three that end a run.
#[tokio::test]
async fn a_gate_failure_fails_closed_at_every_point_that_can_refuse() {
    let cases = [
        (TOOL_PRE, before_tool(), true),
        (STEP_PRE, before_step(), false),
        (STEP_POST, after_step(), false),
        (TOOL_POST, after_tool(), false),
    ];
    for (key, event, wants_block) in cases {
        let registry = HookRegistry::new();
        let _handle = registry
            .register_gate(key, Arc::new(FailingGate))
            .expect("gate registers");
        let fixture = Fixture::new();
        let pass = registry.dispatch(key, &event, &fixture.ctx()).await;
        assert!(
            pass.event.is_none(),
            "`{key}`: a gate that cannot answer ends the pass"
        );
        let reason = match &pass.arbitration.decision {
            HookDecision::Block { reason } if wants_block => reason,
            HookDecision::Terminate { reason } if !wants_block => reason,
            other => panic!(
                "`{key}` wants {}: {other:?}",
                if wants_block { "Block" } else { "Terminate" }
            ),
        };
        assert!(reason.contains("could not decide"), "`{key}`: {reason}");
        assert_eq!(pass.denied_by.as_deref(), Some("failing"));
        assert!(
            pass.failures.is_empty(),
            "`{key}`: a carried denial is not a dispatch failure"
        );
    }
}

/// A panicking gate is the same failure by another route: it must not
/// turn into permission either.
#[tokio::test]
async fn a_panicking_gate_terminates_the_step_pass() {
    struct PanickingGate;

    #[async_trait]
    impl Gate for PanickingGate {
        fn name(&self) -> &str {
            "panicking"
        }

        async fn check(
            &self,
            _event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> Result<HookDecision, String> {
            panic!("the gate exploded");
        }
    }

    let registry = HookRegistry::new();
    let _handle = registry
        .register_gate(STEP_PRE, Arc::new(PanickingGate))
        .expect("gate registers");
    let fixture = Fixture::new();
    let pass = registry
        .dispatch(STEP_PRE, &before_step(), &fixture.ctx())
        .await;
    assert_eq!(
        pass.arbitration.decision,
        HookDecision::Terminate {
            reason: "the gate panicked".to_owned()
        }
    );
    assert_eq!(pass.denied_by.as_deref(), Some("panicking"));
}

/// `llm/request` and `llm/result` accept no denial, so a failing gate
/// there cannot refuse the pass. It must not be recorded as permission
/// either: the dispatch reports it and the harness turns that into a
/// `hook` warning.
#[tokio::test]
async fn a_gate_failure_at_an_observational_point_is_reported() {
    let cases = [
        (LLM_REQUEST, before_request()),
        (LLM_RESULT, after_provider_call()),
    ];
    for (key, event) in cases {
        let registry = HookRegistry::new();
        let _handle = registry
            .register_gate(key, Arc::new(FailingGate))
            .expect("gate registers");
        let fixture = Fixture::new();
        let pass = registry.dispatch(key, &event, &fixture.ctx()).await;
        assert_eq!(
            pass.arbitration.decision,
            HookDecision::Allow,
            "`{key}`: the point carries no denial"
        );
        assert!(pass.event.is_some(), "`{key}`: the pass continues");
        assert_eq!(pass.failures.len(), 1, "`{key}`: {:?}", pass.failures);
        assert!(
            pass.failures[0].contains("failing")
                && pass.failures[0].contains("could not decide"),
            "`{key}`: {:?}",
            pass.failures
        );
    }
}

#[tokio::test]
async fn dropping_the_registration_withdraws_it() {
    let registry = HookRegistry::new();
    let (hook, calls) = FixedHook::new(HookDecision::Allow);
    let registration = registry
        .register_hook(TOOL_PRE, Arc::new(hook))
        .expect("hook registers");
    drop(registration);

    let fixture = Fixture::new();
    let pass = registry
        .dispatch(TOOL_PRE, &before_tool(), &fixture.ctx())
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the row was withdrawn");
    assert!(registry.is_empty());
    assert!(pass.event.is_some());
}

#[tokio::test]
async fn a_request_transformer_rewrites_the_effective_event() {
    let registry = HookRegistry::new();
    let _handle = registry
        .register_request_transformer(LLM_REQUEST, Arc::new(ModelRewriter))
        .expect("transformer registers");

    let fixture = Fixture::new();
    let pass = registry
        .dispatch(LLM_REQUEST, &before_request(), &fixture.ctx())
        .await;
    let Some(HookEvent::BeforeRequest { params, .. }) = pass.event else {
        panic!("a continuing pass hands the rewritten event on");
    };
    assert_eq!(params.model, "rewritten-model");
}

#[tokio::test]
async fn a_result_transformer_rewrites_the_output_and_reports_its_name() {
    let registry = HookRegistry::new();
    let _handle = registry
        .register_result_transformer(TOOL_POST, Arc::new(Redactor))
        .expect("transformer registers");

    let fixture = Fixture::new();
    let mut output = uva_tool::ToolOutput::text("secret");
    let names = registry.transform_result(&mut output, &fixture.ctx()).await;
    assert_eq!(names, vec!["redactor".to_owned()]);
    assert_eq!(
        output.content,
        uva_tool::ToolOutput::text("[redacted]").content
    );

    // A no-op transformer reports no names: the loop writes no
    // provenance row for an unchanged result.
    let names = registry.transform_result(&mut output, &fixture.ctx()).await;
    assert!(names.is_empty(), "the second pass changes nothing");
}

#[test]
fn the_deny_only_fold_cannot_be_cleared() {
    let mut arbitration = Arbitration::default();
    arbitration.absorb(HookDecision::Block {
        reason: "denied".to_owned(),
    });
    arbitration.absorb(HookDecision::Allow);
    arbitration.absorb(HookDecision::ReplaceParams(
        RequestParams::from_request(&uva_provider::CompletionRequest::default()),
    ));
    assert!(
        matches!(arbitration.decision, HookDecision::Block { .. }),
        "registration order cannot turn a denial back into permission: \
         {arbitration:?}"
    );
    assert!(arbitration.decided());
}
