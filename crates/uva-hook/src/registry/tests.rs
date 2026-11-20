//! Registry behavior: the revocable registration, the arbitration,
//! and the four registration kinds.

use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use uva_steering::{HookDecision, HookEvent, RequestParams};

use super::{Arbitration, HookRegistry, RegistryError};
use crate::{
    FactKind,
    MountPoint,
    PromptSection,
    RegistrationOrigin,
    context::HookContext,
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

fn before_run() -> HookEvent {
    HookEvent::BeforeRun {
        agent_name: "a".to_owned(),
        run_idx: 0,
    }
}

fn after_run() -> HookEvent {
    HookEvent::AfterRun {
        agent_name: "a".to_owned(),
        run_idx: 0,
        reason: "completed".to_owned(),
    }
}

fn before_provider_call() -> HookEvent {
    HookEvent::BeforeProviderCall {
        provider: "p".to_owned(),
        model: "m".to_owned(),
        estimated_tokens: 12,
    }
}

fn on_error() -> HookEvent {
    HookEvent::OnError {
        agent_name: "a".to_owned(),
        reason: "boom".to_owned(),
    }
}

fn on_compaction() -> HookEvent {
    HookEvent::OnCompaction {
        start: 0,
        end: 5,
        summary: "short summary".to_owned(),
    }
}

fn section(title: &str, body: &str) -> PromptSection {
    PromptSection {
        title: title.to_owned(),
        body: body.to_owned(),
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

/// An observer that records its name in a shared log and allows.
struct NamedHook {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}

impl NamedHook {
    fn new(name: &'static str, log: &Arc<Mutex<Vec<String>>>) -> Arc<Self> {
        Arc::new(Self {
            name,
            log: Arc::clone(log),
        })
    }
}

#[async_trait]
impl Hook for NamedHook {
    fn name(&self) -> &str {
        self.name
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.log
            .lock()
            .expect("the log is not poisoned")
            .push(self.name.to_owned());
        HookDecision::Allow
    }
}

/// An observer that panics on every pass and counts its calls.
struct PanickingHook {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

impl PanickingHook {
    fn new(name: &'static str, calls: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            name,
            calls: Arc::clone(calls),
        })
    }
}

#[async_trait]
impl Hook for PanickingHook {
    fn name(&self) -> &str {
        self.name
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("the hook exploded");
    }
}

/// A request transformer that panics on every pass and counts its
/// calls.
struct PanickingRequestTransformer {
    calls: Arc<AtomicUsize>,
}

impl PanickingRequestTransformer {
    fn new(calls: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            calls: Arc::clone(calls),
        })
    }
}

#[async_trait]
impl RequestTransformer for PanickingRequestTransformer {
    fn name(&self) -> &str {
        "panicking-request"
    }

    async fn transform(
        &self,
        _params: &mut RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("the request transformer exploded");
    }
}

/// A result transformer that panics on every pass and counts its
/// calls.
struct PanickingResultTransformer {
    calls: Arc<AtomicUsize>,
}

impl PanickingResultTransformer {
    fn new(calls: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            calls: Arc::clone(calls),
        })
    }
}

#[async_trait]
impl ResultTransformer for PanickingResultTransformer {
    fn name(&self) -> &str {
        "panicking-result"
    }

    async fn transform(
        &self,
        _output: &mut uva_tool::ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("the result transformer exploded");
    }
}

#[test]
fn a_transformer_must_name_its_own_mount_point() {
    let registry = HookRegistry::new();
    // The error names the wire spelling of both points: the entry takes
    // a `MountPoint`, the report speaks the one spelling the enum
    // declares.
    let error = registry
        .register_request_transformer_at(
            MountPoint::StepPre,
            Arc::new(ModelRewriter),
            RegistrationOrigin::Plugin,
        )
        .expect_err("step-pre cannot rewrite a request");
    assert_eq!(
        error,
        RegistryError::WrongMountPoint {
            key: "agent/step-pre",
            expected: "llm/request",
        }
    );
    let error = registry
        .register_result_transformer_at(
            MountPoint::ToolPre,
            Arc::new(Redactor),
            RegistrationOrigin::Plugin,
        )
        .expect_err("tool-pre cannot rewrite a result");
    assert_eq!(
        error,
        RegistryError::WrongMountPoint {
            key: "tool/pre-execute",
            expected: "tool/post-execute",
        }
    );
}

#[tokio::test]
async fn the_first_block_short_circuits_the_pass() {
    let registry = HookRegistry::new();
    let (blocker, _blocked_calls) = FixedHook::new(HookDecision::Block {
        reason: "no shell".to_owned(),
    });
    let _block = registry.register_hook_at(
        MountPoint::ToolPre,
        Arc::new(blocker),
        RegistrationOrigin::Plugin,
    );
    let (late, late_calls) = FixedHook::new(HookDecision::Allow);
    let _late = registry.register_hook_at(
        MountPoint::ToolPre,
        Arc::new(late),
        RegistrationOrigin::Plugin,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::ToolPre, &before_tool(), &fixture.ctx())
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
        handles.push(registry.register_hook_at(
            MountPoint::ToolPre,
            Arc::new(hook),
            RegistrationOrigin::Plugin,
        ));
    }
    let (allow, calls) = FixedHook::new(HookDecision::Allow);
    let _allow = registry.register_hook_at(
        MountPoint::ToolPre,
        Arc::new(allow),
        RegistrationOrigin::Plugin,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::ToolPre, &before_tool(), &fixture.ctx())
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
    let _handle = registry.register_hook_at(
        MountPoint::StepPre,
        Arc::new(terminal),
        RegistrationOrigin::Plugin,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::StepPre, &before_step(), &fixture.ctx())
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
    let _handle = registry.register_hook_at(
        MountPoint::LlmResult,
        Arc::new(terminal),
        RegistrationOrigin::Plugin,
    );
    let pass = registry
        .dispatch_at(
            MountPoint::LlmResult,
            &after_provider_call(),
            &fixture.ctx(),
        )
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
        (MountPoint::ToolPre, before_tool(), true),
        (MountPoint::StepPre, before_step(), false),
        (MountPoint::StepPost, after_step(), false),
        (MountPoint::ToolPost, after_tool(), false),
    ];
    for (mount, event, wants_block) in cases {
        let registry = HookRegistry::new();
        let _handle = registry.register_gate_at(
            mount,
            Arc::new(FailingGate),
            RegistrationOrigin::Plugin,
        );
        let fixture = Fixture::new();
        let pass = registry.dispatch_at(mount, &event, &fixture.ctx()).await;
        assert!(
            pass.event.is_none(),
            "`{mount}`: a gate that cannot answer ends the pass"
        );
        let reason = match &pass.arbitration.decision {
            HookDecision::Block { reason } if wants_block => reason,
            HookDecision::Terminate { reason } if !wants_block => reason,
            other => panic!(
                "`{mount}` wants {}: {other:?}",
                if wants_block { "Block" } else { "Terminate" }
            ),
        };
        assert!(reason.contains("could not decide"), "`{mount}`: {reason}");
        assert_eq!(pass.denied_by.as_deref(), Some("failing"));
        assert!(
            pass.facts.is_empty(),
            "`{mount}`: a carried denial is not a dispatch failure"
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
    let _handle = registry.register_gate_at(
        MountPoint::StepPre,
        Arc::new(PanickingGate),
        RegistrationOrigin::Plugin,
    );
    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::StepPre, &before_step(), &fixture.ctx())
        .await;
    assert_eq!(
        pass.arbitration.decision,
        HookDecision::Terminate {
            reason: "gate `panicking` could not answer: the gate panicked"
                .to_owned()
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
        (MountPoint::LlmRequest, before_request()),
        (MountPoint::LlmResult, after_provider_call()),
    ];
    for (mount, event) in cases {
        let registry = HookRegistry::new();
        let _handle = registry.register_gate_at(
            mount,
            Arc::new(FailingGate),
            RegistrationOrigin::Plugin,
        );
        let fixture = Fixture::new();
        let pass = registry.dispatch_at(mount, &event, &fixture.ctx()).await;
        assert_eq!(
            pass.arbitration.decision,
            HookDecision::Allow,
            "`{mount}`: the point carries no denial"
        );
        assert!(pass.event.is_some(), "`{mount}`: the pass continues");
        assert_eq!(pass.facts.len(), 1, "`{mount}`: {:?}", pass.facts);
        let detail = pass.facts[0].message();
        assert!(
            detail.contains("failing") && detail.contains("could not decide"),
            "`{mount}`: {detail}"
        );
    }
}

#[tokio::test]
async fn dropping_the_registration_withdraws_it() {
    let registry = HookRegistry::new();
    let (hook, calls) = FixedHook::new(HookDecision::Allow);
    let registration = registry.register_hook_at(
        MountPoint::ToolPre,
        Arc::new(hook),
        RegistrationOrigin::Plugin,
    );
    drop(registration);

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::ToolPre, &before_tool(), &fixture.ctx())
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the row was withdrawn");
    assert!(registry.is_empty());
    assert!(pass.event.is_some());
}

#[tokio::test]
async fn a_request_transformer_rewrites_the_effective_event() {
    let registry = HookRegistry::new();
    let _handle = registry
        .register_request_transformer_at(
            MountPoint::LlmRequest,
            Arc::new(ModelRewriter),
            RegistrationOrigin::Plugin,
        )
        .expect("transformer registers");

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::LlmRequest, &before_request(), &fixture.ctx())
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
        .register_result_transformer_at(
            MountPoint::ToolPost,
            Arc::new(Redactor),
            RegistrationOrigin::Plugin,
        )
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

/// A `Hook` or `Gate` at `tool/post-execute` observes the event; it
/// is not a rewrite surface. The loop's no-rewrite fast path keys on
/// this predicate, so a row that cannot transform must not report
/// one.
#[tokio::test]
async fn an_observer_at_tool_post_is_not_a_result_transformer() {
    let registry = HookRegistry::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let _observer = registry.register_hook_at(
        MountPoint::ToolPost,
        NamedHook::new("observer", &log),
        RegistrationOrigin::InProcess,
    );
    let _gate = registry.register_gate_at(
        MountPoint::ToolPost,
        Arc::new(FailingGate),
        RegistrationOrigin::InProcess,
    );
    assert!(
        registry.has_entries_at(MountPoint::ToolPost),
        "the point holds rows"
    );
    assert!(
        !registry.has_result_transformers(),
        "a Hook or Gate at tool/post-execute transforms nothing"
    );

    // The transform pass agrees: it returns before copying the output.
    let fixture = Fixture::new();
    let mut output = uva_tool::ToolOutput::text("kept");
    let names = registry.transform_result(&mut output, &fixture.ctx()).await;
    assert!(names.is_empty(), "no transformer ran");
    assert_eq!(output.content, uva_tool::ToolOutput::text("kept").content);

    let _transformer = registry
        .register_result_transformer_at(
            MountPoint::ToolPost,
            Arc::new(Redactor),
            RegistrationOrigin::Plugin,
        )
        .expect("a result transformer belongs on tool/post-execute");
    assert!(
        registry.has_result_transformers(),
        "a real Result registration makes the fast path take the slow branch"
    );
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

#[tokio::test]
async fn every_mount_point_carries_its_registration() {
    let cases = [
        (MountPoint::StepPre, before_step()),
        (MountPoint::StepPost, after_step()),
        (MountPoint::ToolPre, before_tool()),
        (MountPoint::ToolPost, after_tool()),
        (MountPoint::LlmRequest, before_request()),
        (MountPoint::LlmResult, after_provider_call()),
        (MountPoint::BeforeRun, before_run()),
        (MountPoint::AfterRun, after_run()),
        (MountPoint::BeforeProviderCall, before_provider_call()),
        (MountPoint::OnError, on_error()),
        (MountPoint::OnCompaction, on_compaction()),
    ];
    assert_eq!(cases.len(), 11, "every event point is covered");
    for (mount, event) in cases {
        let registry = HookRegistry::new();
        let (hook, calls) = FixedHook::new(HookDecision::Allow);
        let _handle = registry.register_hook_at(
            mount,
            Arc::new(hook),
            RegistrationOrigin::Plugin,
        );
        assert!(registry.has_entries_at(mount), "{mount} holds its row");
        let fixture = Fixture::new();
        let pass = registry.dispatch_at(mount, &event, &fixture.ctx()).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{mount} dispatched");
        assert!(pass.event.is_some(), "{mount} hands the event on");
        assert!(pass.facts.is_empty(), "{mount}: {:?}", pass.facts);
    }

    let registry = HookRegistry::new();
    let _section = registry
        .register_prompt_section(
            MountPoint::PromptSections,
            section("audit", "the audit body"),
            RegistrationOrigin::Plugin,
        )
        .expect("the collection point carries a section");
    assert!(registry.has_entries_at(MountPoint::PromptSections));
    assert_eq!(
        registry.prompt_sections(),
        vec![section("audit", "the audit body")]
    );
}

#[tokio::test]
async fn plugin_registrations_run_before_in_process_ones() {
    let registry = HookRegistry::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let _in_process_first = registry.register_hook_at(
        MountPoint::BeforeRun,
        NamedHook::new("in-process-first", &log),
        RegistrationOrigin::InProcess,
    );
    let _plugin_second = registry.register_hook_at(
        MountPoint::BeforeRun,
        NamedHook::new("plugin-second", &log),
        RegistrationOrigin::Plugin,
    );
    let _in_process_third = registry.register_hook_at(
        MountPoint::BeforeRun,
        NamedHook::new("in-process-third", &log),
        RegistrationOrigin::InProcess,
    );
    let _plugin_fourth = registry.register_hook_at(
        MountPoint::BeforeRun,
        NamedHook::new("plugin-fourth", &log),
        RegistrationOrigin::Plugin,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::BeforeRun, &before_run(), &fixture.ctx())
        .await;
    assert!(pass.event.is_some());
    assert_eq!(
        log.lock().expect("the log is not poisoned").clone(),
        vec![
            "plugin-second".to_owned(),
            "plugin-fourth".to_owned(),
            "in-process-first".to_owned(),
            "in-process-third".to_owned(),
        ],
        "the plugin lane runs first; registration order decides inside it"
    );
}

#[tokio::test]
async fn a_plugin_panic_is_skipped_and_the_registration_stays() {
    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _handle = registry.register_hook_at(
        MountPoint::ToolPre,
        PanickingHook::new("panicking-plugin", &calls),
        RegistrationOrigin::Plugin,
    );
    let (later, later_calls) = FixedHook::new(HookDecision::Allow);
    let _later = registry.register_hook_at(
        MountPoint::ToolPre,
        Arc::new(later),
        RegistrationOrigin::Plugin,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::ToolPre, &before_tool(), &fixture.ctx())
        .await;
    assert!(
        pass.event.is_some(),
        "a plugin panic does not fail the pass"
    );
    assert_eq!(
        later_calls.load(Ordering::SeqCst),
        1,
        "later registrations still run"
    );
    assert_eq!(pass.facts.len(), 1, "{:?}", pass.facts);
    assert_eq!(pass.facts[0].kind, FactKind::Panicked);
    assert!(
        pass.facts[0].detail.contains("stays registered"),
        "{:?}",
        pass.facts[0]
    );

    let pass = registry
        .dispatch_at(MountPoint::ToolPre, &before_tool(), &fixture.ctx())
        .await;
    assert!(pass.event.is_some());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the plugin gets another chance on the next pass"
    );
    assert!(registry.has_entries_at(MountPoint::ToolPre));
}

#[tokio::test]
async fn an_in_process_panic_withdraws_the_registration() {
    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = registry.register_hook_at(
        MountPoint::StepPre,
        PanickingHook::new("panicking-policy", &calls),
        RegistrationOrigin::InProcess,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::StepPre, &before_step(), &fixture.ctx())
        .await;
    assert!(pass.event.is_some(), "withdrawal does not fail the pass");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(pass.facts.len(), 1, "{:?}", pass.facts);
    assert_eq!(pass.facts[0].kind, FactKind::Panicked);
    assert!(
        pass.facts[0].detail.contains("withdrawn"),
        "{:?}",
        pass.facts[0]
    );
    assert!(!registry.has_entries_at(MountPoint::StepPre));
    assert!(registry.is_empty());

    let pass = registry
        .dispatch_at(MountPoint::StepPre, &before_step(), &fixture.ctx())
        .await;
    assert!(pass.event.is_some());
    assert!(
        pass.facts.is_empty(),
        "the withdrawn row does not run again"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    drop(handle);
    assert!(registry.is_empty());
}

#[tokio::test]
async fn a_request_transformer_panic_follows_its_origin() {
    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _handle = registry
        .register_request_transformer_at(
            MountPoint::LlmRequest,
            PanickingRequestTransformer::new(&calls),
            RegistrationOrigin::Plugin,
        )
        .expect("the plugin transformer registers");
    let fixture = Fixture::new();
    for _ in 0..2 {
        let pass = registry
            .dispatch_at(
                MountPoint::LlmRequest,
                &before_request(),
                &fixture.ctx(),
            )
            .await;
        assert!(pass.event.is_some(), "a failure never rewrites the pass");
        assert_eq!(pass.facts.len(), 1);
        assert_eq!(pass.facts[0].kind, FactKind::Panicked);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the plugin transformer is kept"
    );
    assert!(registry.has_entries_at(MountPoint::LlmRequest));

    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _handle = registry
        .register_request_transformer_at(
            MountPoint::LlmRequest,
            PanickingRequestTransformer::new(&calls),
            RegistrationOrigin::InProcess,
        )
        .expect("the in-process transformer registers");
    let pass = registry
        .dispatch_at(MountPoint::LlmRequest, &before_request(), &fixture.ctx())
        .await;
    assert!(pass.event.is_some());
    assert_eq!(pass.facts.len(), 1);
    assert!(
        pass.facts[0].detail.contains("withdrawn"),
        "{:?}",
        pass.facts[0]
    );
    assert!(!registry.has_entries_at(MountPoint::LlmRequest));

    let pass = registry
        .dispatch_at(MountPoint::LlmRequest, &before_request(), &fixture.ctx())
        .await;
    assert!(pass.event.is_some());
    assert!(pass.facts.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_result_transformer_panic_follows_its_origin() {
    let fixture = Fixture::new();
    let mut output = uva_tool::ToolOutput::text("payload");

    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _handle = registry
        .register_result_transformer_at(
            MountPoint::ToolPost,
            PanickingResultTransformer::new(&calls),
            RegistrationOrigin::Plugin,
        )
        .expect("the plugin transformer registers");
    for _ in 0..2 {
        let names =
            registry.transform_result(&mut output, &fixture.ctx()).await;
        assert!(names.is_empty(), "a panic rewrites nothing");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the plugin transformer is kept"
    );
    assert!(registry.has_entries_at(MountPoint::ToolPost));

    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _handle = registry
        .register_result_transformer_at(
            MountPoint::ToolPost,
            PanickingResultTransformer::new(&calls),
            RegistrationOrigin::InProcess,
        )
        .expect("the in-process transformer registers");
    let _ = registry.transform_result(&mut output, &fixture.ctx()).await;
    assert!(!registry.has_entries_at(MountPoint::ToolPost));
    let _ = registry.transform_result(&mut output, &fixture.ctx()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the row was withdrawn");
}

#[tokio::test]
async fn an_observation_point_returns_arbitration_for_the_decisions_it_accepts()
{
    let registry = HookRegistry::new();
    let (hook, calls) = FixedHook::new(HookDecision::Block {
        reason: "not here".to_owned(),
    });
    let _handle = registry.register_hook_at(
        MountPoint::OnError,
        Arc::new(hook),
        RegistrationOrigin::Plugin,
    );

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::OnError, &on_error(), &fixture.ctx())
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the observer saw it");
    assert_eq!(
        pass.arbitration.decision,
        HookDecision::Allow,
        "on_error carries no block"
    );
    assert!(pass.event.is_some(), "the observation hands the event on");
    assert_eq!(pass.facts.len(), 1, "{:?}", pass.facts);
    assert_eq!(pass.facts[0].kind, FactKind::Unsupported);
}

#[test]
fn prompt_sections_keep_lane_then_registration_order() {
    let registry = HookRegistry::new();
    let withdrawn = registry
        .register_prompt_section(
            MountPoint::PromptSections,
            section("in-process-first", "a"),
            RegistrationOrigin::InProcess,
        )
        .expect("a section registers");
    let _plugin = registry
        .register_prompt_section(
            MountPoint::PromptSections,
            section("plugin-second", "b"),
            RegistrationOrigin::Plugin,
        )
        .expect("a section registers");
    let _in_process = registry
        .register_prompt_section(
            MountPoint::PromptSections,
            section("in-process-third", "c"),
            RegistrationOrigin::InProcess,
        )
        .expect("a section registers");

    assert_eq!(
        registry.prompt_sections(),
        vec![
            section("plugin-second", "b"),
            section("in-process-first", "a"),
            section("in-process-third", "c"),
        ]
    );

    drop(withdrawn);
    assert_eq!(
        registry.prompt_sections(),
        vec![
            section("plugin-second", "b"),
            section("in-process-third", "c"),
        ],
        "dropping the handle withdraws the section"
    );
}

#[test]
fn a_section_registers_only_at_the_collection_point() {
    let registry = HookRegistry::new();
    let error = registry
        .register_prompt_section(
            MountPoint::ToolPre,
            section("audit", "body"),
            RegistrationOrigin::Plugin,
        )
        .expect_err("tool-pre is not the collection point");
    assert_eq!(
        error,
        RegistryError::WrongMountPoint {
            key: "tool/pre-execute",
            expected: "prompt/sections",
        }
    );
}

#[tokio::test]
async fn the_collection_point_never_dispatches_an_event() {
    let registry = HookRegistry::new();
    let (hook, calls) = FixedHook::new(HookDecision::Allow);
    let _hook = registry.register_hook_at(
        MountPoint::PromptSections,
        Arc::new(hook),
        RegistrationOrigin::Plugin,
    );
    let _section = registry
        .register_prompt_section(
            MountPoint::PromptSections,
            section("audit", "body"),
            RegistrationOrigin::Plugin,
        )
        .expect("a section registers");

    let fixture = Fixture::new();
    let pass = registry
        .dispatch_at(MountPoint::PromptSections, &before_run(), &fixture.ctx())
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a collection point manufactures no event"
    );
    assert!(pass.event.is_some(), "the caller's event is handed on");
    assert!(pass.facts.is_empty());
    assert_eq!(
        registry.prompt_sections(),
        vec![section("audit", "body")],
        "a hook on the collection point is not a section"
    );
}
