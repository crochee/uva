//! The `approval` service and the gate it bridges.
//!
//! The plugin publishes the deployment's [`ApprovalPolicy`]; the
//! [`ApprovalGate`] reads that policy at the `tool/pre-execute` seam and
//! turns a held call into `HookDecision::Ask`. The gate never decides
//! the approval itself: the durable path is the controller's (write
//! `approval_asked`, suspend the turn, resume through
//! `ResumeToken`), which
//! `agent::re_act::tests::lifecycle::approval_resume` already pins.
//! What is new here is the plugin's service and the gate's Ask/Allow
//! boundary; the timeout path is exercised with the public
//! `ResumeStatus::Rejected` semantics, and the controller test is cited
//! by name below.

use std::sync::Arc;

use serde_json::json;
use uva_context::AgentState;
use uva_core::{AtomicCancelToken, CancelToken};
use uva_harness::{
    ApprovalGate,
    ApprovalPlugin,
    ApprovalPolicy,
    ApprovalRequest,
    EventSink,
    HookContext,
    Host,
    KEY_APPROVAL,
    ResumeDecision,
    ResumeStatus,
    ResumeToken,
    Suspension,
    seams::Seams,
};
use uva_kernel::{
    Context as KernelContext,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use uva_steering::HookDecision;
use uva_tool::Risk;

/// Classifies a call for the gate. The real host derives this from the
/// `tools` registry (`Tool::risk`); the test fixes it so the policy,
/// not the classifier, is what the assertions measure.
type RiskFn = Arc<dyn Fn(&str, &serde_json::Value) -> Risk + Send + Sync>;

fn risk_by_tool() -> RiskFn {
    Arc::new(|tool, _arguments| match tool {
        "shell" => Risk::High,
        _ => Risk::Low,
    })
}

/// The per-dispatch context the seam reads: a state, an event sink and
/// a cancellation token of the test's own.
struct Fixture {
    state: AgentState,
    sink: EventSink,
    cancel: Arc<dyn CancelToken>,
}

impl Fixture {
    fn new() -> Self {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        Self {
            state: AgentState::with_window(1_000),
            sink: EventSink::new(Arc::new(tx), None),
            cancel: AtomicCancelToken::shared(),
        }
    }

    fn ctx(&self) -> HookContext<'_> {
        HookContext {
            state: &self.state,
            cancel: &self.cancel,
        }
    }
}

#[tokio::test]
async fn approval_denies_on_timeout() {
    let policy = ApprovalPolicy::default();
    let ctx = KernelContext::new();
    let plugin = ApprovalPlugin::new(policy.clone());

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `approval`");
    let service = ctx
        .require::<ApprovalPolicy>(KEY_APPROVAL)
        .expect("`approval` resolves");
    assert_eq!(*service, policy);

    // The gate reads the provided policy; the host installs it at the
    // `tool/pre-execute` seam.
    let mut host = Host::new();
    let _registration =
        host.gate(Arc::new(ApprovalGate::new(service, "ws", risk_by_tool())));
    let seams = Seams::new(host);
    let fixture = Fixture::new();

    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "shell",
            "c1",
            &json!({"command": "rm -rf /"}),
        )
        .await;
    assert!(
        matches!(arbitration.decision, HookDecision::Allow),
        "Ask is collected, not decided: {:?}",
        arbitration.decision
    );
    assert_eq!(
        arbitration.asks.len(),
        1,
        "a high-risk call must ask for approval"
    );
    assert!(
        arbitration.asks[0].contains("shell"),
        "the ask names the tool: {}",
        arbitration.asks[0]
    );
    assert!(
        arbitration.asks[0].contains("high"),
        "the ask names the risk: {}",
        arbitration.asks[0]
    );

    // What the loop records for the held call: the interrupt and the
    // log-derived token that resumes the turn.
    let request = ApprovalRequest::new(
        "approval-1",
        "c1",
        "shell",
        "sha256:args",
        Risk::High,
        "default",
    );
    let suspension = Suspension::AwaitingApproval {
        interrupts: vec![request.clone()],
        resume: ResumeToken::new(7, vec!["c1".to_string()], "sha256:snapshot"),
    };
    let Suspension::AwaitingApproval { interrupts, resume } = suspension;
    assert_eq!(interrupts.len(), 1);
    assert_eq!(resume.pending, vec!["c1".to_string()]);

    // The operator never answers: the controller records a rejection.
    // The durable half lives in
    // `agent::re_act::tests::lifecycle::approval_resume::
    //  a_rejected_interrupt_writes_an_is_error_result`, which asserts
    // the denied `approval_decided` row, no dispatch, and the
    // `is_error` tool result; this test pins the same public
    // `ResumeStatus` semantics without restating the controller.
    let timed_out = ResumeDecision {
        id: request.id.clone(),
        status: ResumeStatus::Rejected,
        payload: Some(json!({"reason": "approval timed out"})),
    };
    assert!(
        !timed_out.executes(),
        "a timed-out approval never runs the call"
    );
    assert_eq!(timed_out.durable_status(), "denied");
    let message = timed_out.denial_message();
    assert!(
        message.contains("timed out"),
        "the model sees the reason: {message}"
    );
    assert!(
        message.contains("did not run"),
        "the model knows the call never executed: {message}"
    );

    drop(mounted);
    assert!(
        ctx.require::<ApprovalPolicy>(KEY_APPROVAL).is_err(),
        "unload must withdraw `approval`"
    );
}

#[tokio::test]
async fn approval_allows_low_risk() {
    let ctx = KernelContext::new();
    let plugin = ApprovalPlugin::new(ApprovalPolicy::default());
    let _mounted =
        mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
            .expect("mount `approval`");
    let service = ctx
        .require::<ApprovalPolicy>(KEY_APPROVAL)
        .expect("`approval` resolves");

    let mut host = Host::new();
    let _registration =
        host.gate(Arc::new(ApprovalGate::new(service, "ws", risk_by_tool())));
    let seams = Seams::new(host);
    let fixture = Fixture::new();

    let arbitration = seams
        .tool_pre(
            &fixture.ctx(),
            &fixture.sink,
            "read",
            "c2",
            &json!({"path": "a.txt"}),
        )
        .await;
    assert!(
        arbitration.asks.is_empty(),
        "a low-risk read is never asked about: {:?}",
        arbitration.asks
    );
    assert_eq!(arbitration.decision, HookDecision::Allow);
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = ApprovalPlugin::new(ApprovalPolicy::default());
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "approval");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_APPROVAL.to_string()]);
    assert_eq!(KEY_APPROVAL, "approval");
    assert!(
        manifest.inject.is_empty(),
        "the policy arrives with the plugin value; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}
