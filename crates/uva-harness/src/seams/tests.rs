//! Tests for the seam layer: one registry dispatch for the six passes
//! and the five observation points, and the facts it turns into the
//! run's warnings.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::channel::mpsc;
use uva_hook::{MountPoint, RegistrationOrigin};
use uva_steering::{HookDecision, HookEvent, RequestParams};

use super::Seams;
use crate::agent::{
    EventSink,
    host::{Hook, HookContext, Host},
};

struct Fixture {
    state: uva_context::AgentState,
    sink: EventSink,
    cancel: Arc<dyn uva_core::CancelToken>,
    events: mpsc::UnboundedReceiver<crate::AgentEvent>,
}

impl Fixture {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self {
            state: uva_context::AgentState::with_window(1_000),
            sink: EventSink::new(Arc::new(tx), None),
            cancel: uva_core::AtomicCancelToken::shared(),
            events: rx,
        }
    }

    fn ctx(&self) -> HookContext<'_> {
        HookContext {
            state: &self.state,
            cancel: &self.cancel,
        }
    }

    // The hook warnings the run published.
    fn hook_warnings(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            if let crate::AgentEvent::System(uva_events::SystemEvent::Warning {
                kind,
                message,
                ..
            }) = event
                && kind == uva_events::WarningKind::Hook
            {
                warnings.push(message);
            }
        }
        warnings
    }
}

// A host over an empty registry the test can register into.
fn host() -> Host {
    Host::new()
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

struct FixedHook {
    name: &'static str,
    decision: HookDecision,
}

#[async_trait]
impl Hook for FixedHook {
    fn name(&self) -> &str {
        self.name
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.decision.clone()
    }
}

// An observer that records the wire spelling of every event it sees.
struct EventRecorder {
    name: &'static str,
    seen: Arc<std::sync::Mutex<Vec<&'static str>>>,
    calls: Arc<AtomicUsize>,
    panic: bool,
}

#[async_trait]
impl Hook for EventRecorder {
    fn name(&self) -> &str {
        self.name
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.panic {
            panic!("intentional observer panic for test");
        }
        self.seen
            .lock()
            .expect("the log is not poisoned")
            .push(uva_steering::event_name(event));
        HookDecision::Allow
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
async fn a_registered_terminate_on_step_pre_decides_the_pass() {
    let host = host();
    let _registration = host.hook_registry().register_hook_at(
        MountPoint::StepPre,
        Arc::new(FixedHook {
            name: "no-model-today",
            decision: HookDecision::Terminate {
                reason: "no model today".to_owned(),
            },
        }),
        RegistrationOrigin::Plugin,
    );
    let seams = Seams::new(host);

    let fixture = Fixture::new();
    let arbitration =
        seams.step_pre(&fixture.ctx(), &fixture.sink, "a", 0).await;
    assert!(
        matches!(arbitration.decision, HookDecision::Terminate { .. }),
        "`before_step` accepts terminate, so the registration ends the \
         step: {arbitration:?}"
    );
}

// Both lanes fold into one report: the plugin lane's asks come first,
// the in-process lane's follow, and neither ends the pass.
#[tokio::test]
async fn asks_collect_from_both_lanes() {
    let host = host();
    let _external = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(FixedHook {
            name: "external-asker",
            decision: HookDecision::Ask {
                reason: "external ask".to_owned(),
            },
        }),
        RegistrationOrigin::Plugin,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let _in_process = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(CountingHook {
            calls: Arc::clone(&calls),
        }),
        RegistrationOrigin::InProcess,
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
    assert_eq!(arbitration.asks, vec!["external ask".to_owned()]);
    assert!(!arbitration.decided());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the in-process lane runs after the plugin lane: {arbitration:?}"
    );
}

#[tokio::test]
async fn a_gate_registration_fails_closed_at_tool_pre() {
    let host = host();
    let _registration = host.hook_registry().register_gate_at(
        MountPoint::ToolPre,
        Arc::new(BlockingGate),
        RegistrationOrigin::InProcess,
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
        "the gate still fails closed: {arbitration:?}"
    );
}

#[tokio::test]
async fn a_request_transformer_rewrites_the_parameters() {
    let host = host();
    let _registration = host
        .hook_registry()
        .register_request_transformer_at(
            MountPoint::LlmRequest,
            Arc::new(RenamingTransformer),
            RegistrationOrigin::Plugin,
        )
        .expect("llm/request carries a request transformer");
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

// The contract this ticket lands: an out-of-tree registration reaches
// all five observation points through the one table.
#[tokio::test]
async fn the_observation_points_dispatch_the_plugin_lane() {
    let host = host();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let record = |host: &Host, mount: MountPoint| {
        host.hook_registry().register_hook_at(
            mount,
            Arc::new(EventRecorder {
                name: "recorder",
                seen: Arc::clone(&seen),
                calls: Arc::clone(&calls),
                panic: false,
            }),
            RegistrationOrigin::Plugin,
        )
    };
    let registration = record(&host, MountPoint::BeforeRun);
    let seams = Seams::new(host);
    let fixture = Fixture::new();

    seams
        .run_started(&fixture.ctx(), &fixture.sink, "a", 0)
        .await;
    assert_eq!(
        seen.lock().expect("the log").as_slice(),
        ["before_run"],
        "an out-of-tree registration sees before_run"
    );

    // The same registration is one row per point: a second point needs
    // its own, exactly like every other mount point.
    drop(registration);
    let mut registrations = Vec::new();
    for mount in [
        MountPoint::AfterRun,
        MountPoint::BeforeProviderCall,
        MountPoint::OnError,
        MountPoint::OnCompaction,
    ] {
        registrations.push(record(seams.host(), mount));
    }

    seams
        .run_finished(&fixture.ctx(), &fixture.sink, "a", 0, "done")
        .await;
    seams
        .provider_started(&fixture.ctx(), &fixture.sink, "p", "m", 12)
        .await;
    seams
        .provider_failed(&fixture.ctx(), &fixture.sink, "a", "boom")
        .await;
    seams
        .compacted(&fixture.ctx(), &fixture.sink, 0, 5, "summary")
        .await;

    assert_eq!(
        seen.lock().expect("the log").as_slice(),
        [
            "before_run",
            "after_run",
            "before_provider_call",
            "on_error",
            "on_compaction",
        ],
        "every observation point dispatches through the same table"
    );
}

// A dropped registration withdraws from the seam layer too.
#[tokio::test]
async fn dropping_a_registration_withdraws_it_from_the_pass() {
    let host = host();
    let calls = Arc::new(AtomicUsize::new(0));
    let registration = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(CountingHook {
            calls: Arc::clone(&calls),
        }),
        RegistrationOrigin::Plugin,
    );
    let seams = Seams::new(host);

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

// A decision the point does not accept does not apply — and it is not
// silently dropped: the run's durable record says which registration
// decided it.
#[tokio::test]
async fn unsupported_decision_at_a_point_is_a_noop_and_is_reported() {
    let host = host();
    let _registration = host.hook_registry().register_hook_at(
        MountPoint::BeforeRun,
        Arc::new(FixedHook {
            name: "blocker",
            decision: HookDecision::Block {
                reason: "no".to_owned(),
            },
        }),
        RegistrationOrigin::InProcess,
    );
    let mut fixture = Fixture::new();
    let seams = Seams::new(host);
    let arbitration = seams
        .run_started(&fixture.ctx(), &fixture.sink, "default", 0)
        .await;

    assert_eq!(arbitration.decision, HookDecision::Allow);
    let warnings = fixture.hook_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("blocker"), "{}", warnings[0]);
}

// The two panic policies are facts on the registration, not on a
// dispatch path: an in-process registration is withdrawn after its
// panic, a plugin registration stays — and both report through the
// run's durable record.
#[tokio::test]
async fn a_panicking_observer_follows_its_origin() {
    let host = host();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let in_process_calls = Arc::new(AtomicUsize::new(0));
    let plugin_calls = Arc::new(AtomicUsize::new(0));
    let later_calls = Arc::new(AtomicUsize::new(0));
    // The in-process row is withdrawn after its panic; the plugin row
    // is kept and skipped.
    let _in_process = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(EventRecorder {
            name: "broken-in-process",
            seen: Arc::clone(&seen),
            calls: Arc::clone(&in_process_calls),
            panic: true,
        }),
        RegistrationOrigin::InProcess,
    );
    let _plugin = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(EventRecorder {
            name: "broken-plugin",
            seen: Arc::clone(&seen),
            calls: Arc::clone(&plugin_calls),
            panic: true,
        }),
        RegistrationOrigin::Plugin,
    );
    let _counting = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(CountingHook {
            calls: Arc::clone(&later_calls),
        }),
        RegistrationOrigin::Plugin,
    );
    let mut fixture = Fixture::new();
    let seams = Seams::new(host.clone());

    for _ in 0..2 {
        seams
            .tool_pre(
                &fixture.ctx(),
                &fixture.sink,
                "shell",
                "c1",
                &serde_json::json!({}),
            )
            .await;
    }

    assert_eq!(
        in_process_calls.load(Ordering::SeqCst),
        1,
        "an in-process registration that panics is withdrawn"
    );
    assert_eq!(
        plugin_calls.load(Ordering::SeqCst),
        2,
        "a plugin registration that panics is kept and skipped"
    );
    assert_eq!(
        later_calls.load(Ordering::SeqCst),
        2,
        "the pass continues past a panicking registration either way"
    );
    let warnings = fixture.hook_warnings();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(
        warnings.iter().any(|w| w.contains("broken-in-process"))
            && warnings.iter().any(|w| w.contains("broken-plugin")),
        "the facts name the registrations that broke: {warnings:?}"
    );
}

// A gate that cannot answer refuses the pass where the point can
// refuse: `Terminate` at the points that end a run.
struct FailingGate;

#[async_trait]
impl uva_hook::Gate for FailingGate {
    fn name(&self) -> &str {
        "failing"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Err("the policy store is down".to_owned())
    }
}

#[tokio::test]
async fn a_gate_that_cannot_answer_ends_the_run_where_the_point_can_refuse() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let host = host();
    let _registration = host.hook_registry().register_gate_at(
        MountPoint::StepPre,
        Arc::new(FailingGate),
        RegistrationOrigin::InProcess,
    );

    let seams = Seams::new(host);
    let arbitration = seams.step_pre(&ctx, &fixture.sink, "default", 0).await;

    // Fail closed: the step does not begin under a gate that has no
    // answer.
    match &arbitration.decision {
        HookDecision::Terminate { reason } => {
            assert!(reason.contains("failing"), "{reason}");
        }
        other => panic!("expected a termination, got {other:?}"),
    }
}

// The default policy's `LoggingHook` sits at every event point,
// `tool/post-execute` included. It observes; it rewrites nothing, so
// a default deployment must keep the loop's no-rewrite fast path —
// otherwise every tool result is serialized, hashed and cloned for
// nothing.
#[test]
fn a_default_policy_reports_no_result_transformers() {
    let host = Host::default_policy(
        std::env::temp_dir(),
        uva_hook::HookRegistry::new(),
    );
    assert!(
        host.hook_registry().has_entries_at(MountPoint::ToolPost),
        "the policy observes tool/post-execute"
    );
    let seams = Seams::new(host);
    assert!(
        !seams.has_tool_result_transformers(),
        "a Hook at tool/post-execute is not a result transformer"
    );
}
