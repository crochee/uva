use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use elysper_steering::{HookDecision, HookEvent, RequestParams};
use elysper_tool::{Tool, ToolOutput, ToolRegistry};

use super::*;

// The run-level context every dispatch reads: a state, an event sink
// and a cancellation token of the test's own.
struct Fixture {
    state: elysper_context::AgentState,
    sink: crate::agent::sink::EventSink,
    cancel: Arc<dyn elysper_core::CancelToken>,
}

impl Fixture {
    fn new() -> Self {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        Self {
            state: elysper_context::AgentState::with_window(1_000),
            sink: crate::agent::sink::EventSink::new(Arc::new(tx), None),
            cancel: elysper_core::AtomicCancelToken::shared(),
        }
    }

    fn ctx(&self) -> HookContext<'_> {
        HookContext {
            state: &self.state,
            events: &self.sink,
            cancel: &self.cancel,
        }
    }
}

// A gate whose answer the test decides, counting its own calls.
struct CountingGate {
    name: &'static str,
    answer: Answer,
    calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
enum Answer {
    Allow,
    Decision(HookDecision),
    Failure(String),
    Panic,
}

#[async_trait]
impl Gate for CountingGate {
    fn name(&self) -> &str {
        self.name
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.answer {
            Answer::Allow => Ok(HookDecision::Allow),
            Answer::Decision(decision) => Ok(decision.clone()),
            Answer::Failure(reason) => Err(reason.clone()),
            Answer::Panic => panic!("intentional gate panic for test"),
        }
    }
}

// An observer with a fixed decision.
struct FixedObserver {
    name: &'static str,
    decision: HookDecision,
    panic: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Hook for FixedObserver {
    fn name(&self) -> &str {
        self.name
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.panic {
            panic!("intentional observer panic for test");
        }
        self.decision.clone()
    }
}

// A transformer that stamps the parameters it was given.
struct StampingTransformer {
    name: &'static str,
    model: String,
    panic: bool,
}

#[async_trait]
impl RequestTransformer for StampingTransformer {
    fn name(&self) -> &str {
        self.name
    }

    async fn transform(
        &self,
        params: &mut RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        if self.panic {
            panic!("intentional transformer panic for test");
        }
        params.model = self.model.clone();
        Ok(())
    }
}

// A transformer that rewrites a tool result.
struct RewritingTransformer {
    name: &'static str,
    content: String,
    panic: bool,
    // Whether the transform writes; `false` leaves the output alone.
    rewrite: bool,
}

#[async_trait]
impl ResultTransformer for RewritingTransformer {
    fn name(&self) -> &str {
        self.name
    }

    async fn transform(
        &self,
        output: &mut ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        if self.panic {
            panic!("intentional transformer panic for test");
        }
        if !self.rewrite {
            return Ok(());
        }
        output.content = vec![elysper_provider::types::ContentPart::Text(
            elysper_provider::types::TextContent {
                text: self.content.clone(),
                cache_control: None,
            },
        )];
        Ok(())
    }
}

// A plugin that installs a fixed number of observers and gates.
struct TestPlugin {
    observers: usize,
    gates: usize,
    calls: Arc<AtomicUsize>,
}

impl Plugin for TestPlugin {
    fn name(&self) -> &str {
        "test-plugin"
    }

    fn install(&self, host: &mut Host) -> PluginHandle {
        let mut registrations = Vec::new();
        for _ in 0..self.observers {
            registrations.push(host.hook(Arc::new(FixedObserver {
                name: "observer",
                decision: HookDecision::Allow,
                panic: false,
                calls: Arc::clone(&self.calls),
            })));
        }
        for _ in 0..self.gates {
            registrations.push(host.gate(Arc::new(CountingGate {
                name: "gate",
                answer: Answer::Allow,
                calls: Arc::clone(&self.calls),
            })));
        }
        PluginHandle::new(registrations)
    }
}

fn before_tool_event() -> HookEvent {
    HookEvent::BeforeTool {
        tool_name: "shell".to_string(),
        call_id: "c1".to_string(),
        arguments: serde_json::json!({}),
    }
}

fn before_run_event() -> HookEvent {
    HookEvent::BeforeRun {
        agent_name: "default".to_string(),
        run_idx: 0,
    }
}

#[tokio::test]
async fn block_wins_over_ask_and_later_gates_do_not_run() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut host = Host::new();
    let _reg1 = host.gate(Arc::new(CountingGate {
        name: "asker",
        answer: Answer::Decision(HookDecision::Ask {
            reason: "needs approval".to_string(),
        }),
        calls: Arc::clone(&calls),
    }));
    let _reg2 = host.gate(Arc::new(CountingGate {
        name: "blocker",
        answer: Answer::Decision(HookDecision::Block {
            reason: "denied".to_string(),
        }),
        calls: Arc::clone(&calls),
    }));
    let _reg3 = host.gate(Arc::new(CountingGate {
        name: "later",
        answer: Answer::Allow,
        calls: Arc::clone(&calls),
    }));

    let arbitration = host.dispatch_gates(&before_tool_event(), &ctx).await;

    // The block wins over the ask collected before it.
    assert_eq!(
        arbitration.decision,
        HookDecision::Block {
            reason: "denied".to_string()
        }
    );
    // The ask was collected, not dropped.
    assert_eq!(arbitration.asks, vec!["needs approval".to_string()]);
    // The gate behind the block never ran.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_panicking_gate_is_a_block() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.gate(Arc::new(CountingGate {
        name: "broken",
        answer: Answer::Panic,
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    let arbitration = host.dispatch_gates(&before_tool_event(), &ctx).await;

    // Fail closed: a gate that could not answer does not get to
    // approve the call.
    assert!(matches!(arbitration.decision, HookDecision::Block { .. }));
    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, DiagnosticKind::GateFailed);
    assert_eq!(diagnostics[0].source, "broken");
}

#[tokio::test]
async fn a_failing_gate_is_a_block() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.gate(Arc::new(CountingGate {
        name: "broken",
        answer: Answer::Failure("the policy store is down".to_string()),
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    let arbitration = host.dispatch_gates(&before_tool_event(), &ctx).await;

    assert!(matches!(arbitration.decision, HookDecision::Block { .. }));
    assert_eq!(host.diagnostics()[0].kind, DiagnosticKind::GateFailed);
}

#[tokio::test]
async fn dropping_the_plugin_handle_unregisters_every_hook() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut host = Host::new();
    let handle = host.install(&TestPlugin {
        observers: 2,
        gates: 1,
        calls: Arc::clone(&calls),
    });
    assert_eq!(handle.registrations().len(), 3);
    assert_eq!(host.registration_count(), 3);

    // The installed registrations run.
    host.dispatch_hooks(&before_tool_event(), &ctx).await;
    host.dispatch_gates(&before_tool_event(), &ctx).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    drop(handle);

    assert_eq!(host.registration_count(), 0);
    // The withdrawn observer's effect stops: the counter does not
    // move after the uninstall.
    host.dispatch_hooks(&before_tool_event(), &ctx).await;
    host.dispatch_gates(&before_tool_event(), &ctx).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn unsupported_decision_at_a_point_is_a_noop_with_a_diagnostic() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.hook(Arc::new(FixedObserver {
        name: "blocker",
        decision: HookDecision::Block {
            reason: "no".to_string(),
        },
        panic: false,
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    // `before_run` cannot block a tool call: there is no call yet.
    let arbitration = host.dispatch_hooks(&before_run_event(), &ctx).await;

    assert_eq!(arbitration.decision, HookDecision::Allow);
    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, DiagnosticKind::UnsupportedDecision);
    assert_eq!(diagnostics[0].source, "blocker");
}

#[tokio::test]
async fn a_panicking_observer_is_disabled_with_a_diagnostic() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut host = Host::new();
    let _reg1 = host.hook(Arc::new(FixedObserver {
        name: "broken",
        decision: HookDecision::Allow,
        panic: true,
        calls: Arc::clone(&calls),
    }));

    let first = host.dispatch_hooks(&before_tool_event(), &ctx).await;
    assert_eq!(first.decision, HookDecision::Allow);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The second pass skips the disabled observer.
    let second = host.dispatch_hooks(&before_tool_event(), &ctx).await;
    assert_eq!(second.decision, HookDecision::Allow);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, DiagnosticKind::Disabled);
}

#[tokio::test]
async fn the_observers_run_in_registration_order() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut host = Host::new();
    let mut registrations = Vec::new();
    for name in ["first", "second", "third"] {
        let order = Arc::clone(&order);
        registrations.push(host.hook(Arc::new(RecordingObserver {
            name,
            order: Arc::clone(&order),
        })));
    }

    host.dispatch_hooks(&before_tool_event(), &ctx).await;

    assert_eq!(
        *order.lock(),
        vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string()
        ]
    );
}

// The production policy is installed, not just built: every
// registration has to survive the call that made it, or a deployment
// that opts into `default_policy` runs with no guards at all.
#[test]
fn default_policy_installs_the_production_policy() {
    let host = Host::default_policy(std::env::temp_dir());
    assert_eq!(
        host.registration_count(),
        10,
        "5 guards + 3 hints + 1 tracker + 1 hook"
    );
    assert!(
        host.has_event_registrations(),
        "the installed hook must be dispatchable"
    );
    assert!(
        host.diagnostics().is_empty(),
        "the production policy installs without refusals"
    );
}

struct RecordingObserver {
    name: &'static str,
    order: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for RecordingObserver {
    fn name(&self) -> &str {
        self.name
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.order.lock().push(self.name.to_string());
        HookDecision::Allow
    }
}

#[tokio::test]
async fn the_request_transformers_chain() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.request_transformer(Arc::new(StampingTransformer {
        name: "first",
        model: "first-model".to_string(),
        panic: false,
    }));
    let _reg2 = host.request_transformer(Arc::new(StampingTransformer {
        name: "second",
        model: "second-model".to_string(),
        panic: false,
    }));

    let mut params = RequestParams::from_request(
        &elysper_provider::CompletionRequest::default(),
    );
    host.apply_request_transformers(&mut params, &ctx).await;

    // The last transformer in the chain owns the value.
    assert_eq!(params.model, "second-model");
}

#[tokio::test]
async fn a_panicking_transformer_is_disabled_and_the_chain_continues() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.request_transformer(Arc::new(StampingTransformer {
        name: "broken",
        model: "broken-model".to_string(),
        panic: true,
    }));
    let _reg2 = host.request_transformer(Arc::new(StampingTransformer {
        name: "healthy",
        model: "healthy-model".to_string(),
        panic: false,
    }));

    let mut params = RequestParams::from_request(
        &elysper_provider::CompletionRequest::default(),
    );
    host.apply_request_transformers(&mut params, &ctx).await;

    // The broken transformer is skipped, the healthy one still ran.
    assert_eq!(params.model, "healthy-model");
    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, DiagnosticKind::Disabled);
}

#[tokio::test]
async fn the_result_transformers_rewrite_the_output() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.result_transformer(Arc::new(RewritingTransformer {
        name: "rewriter",
        content: "rewritten".to_string(),
        panic: false,
        rewrite: true,
    }));

    let mut output = ToolOutput::text("original");
    let names = host.apply_result_transformers(&mut output, &ctx).await;

    assert_eq!(output.content.len(), 1);
    let rewritten = output.content[0].text().unwrap_or_default();
    assert_eq!(rewritten, "rewritten");
    // The host measures the change itself and names what ran: the
    // caller records provenance from this.
    assert_eq!(names, vec!["rewriter".to_string()]);
}

#[tokio::test]
async fn a_result_transformer_that_changed_nothing_is_not_reported() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.result_transformer(Arc::new(RewritingTransformer {
        name: "leaver",
        content: "untouched".to_string(),
        panic: false,
        rewrite: false,
    }));

    let mut output = ToolOutput::text("untouched");
    let names = host.apply_result_transformers(&mut output, &ctx).await;

    assert!(names.is_empty(), "an unchanged output is nobody's rewrite");
    assert_eq!(output.content[0].text(), Some("untouched"));
}

#[tokio::test]
async fn a_host_without_result_transformers_pays_nothing() {
    let host = Host::new();
    assert!(
        !host.has_result_rewriters(),
        "the loop must be able to skip the whole pass"
    );
}

#[tokio::test]
async fn the_prompt_sections_keep_their_registration_order() {
    let mut host = Host::new();
    let _reg1 = host.prompt_section(PromptSection {
        title: "first".to_string(),
        body: "one".to_string(),
    });
    let _reg2 = host.prompt_section(PromptSection {
        title: "second".to_string(),
        body: "two".to_string(),
    });

    let titles: Vec<String> = host
        .prompt_sections()
        .into_iter()
        .map(|section| section.title)
        .collect();
    assert_eq!(titles, vec!["first".to_string(), "second".to_string()]);
}

// A tool the host can register, distinguishable by its description.
struct NamedTool {
    name: &'static str,
    description: &'static str,
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &elysper_tool::Context,
    ) -> ToolOutput {
        ToolOutput::text("ok")
    }
}

#[test]
fn a_withdrawn_tool_leaves_the_registry_and_is_no_longer_callable() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    assert!(
        registry.contains("hosted"),
        "the bound host's tool is callable"
    );
    assert!(
        registry.get("hosted").is_some(),
        "the registry resolves the host's tool"
    );

    drop(registration);

    assert!(
        !registry.contains("hosted"),
        "a withdrawn tool must not stay in the registry"
    );
    assert!(
        registry.get("hosted").is_none(),
        "a withdrawn tool must not stay callable"
    );
}

#[test]
fn a_tool_added_to_a_bound_host_reaches_the_registry() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    let registration = host.tool(Arc::new(NamedTool {
        name: "late",
        description: "a late tool",
    }));

    assert!(
        registry.contains("late"),
        "a tool registered after the binding is callable too"
    );

    drop(registration);

    assert!(!registry.contains("late"));
}

#[test]
fn a_withdrawn_tool_leaves_the_other_entry_under_the_name_alone() {
    let registry = Arc::new(ToolRegistry::new());
    // Another party already filed a tool under the name.
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        NamedTool {
            name: "shared",
            description: "the other party's tool",
        },
    )));
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "shared",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    drop(registration);

    // The other party's entry survives: the withdrawal names the
    // host's own registration, not the name.
    let entry = registry
        .get("shared")
        .expect("the other party's tool must survive the withdrawal");
    assert_eq!(
        entry.tool_instance().description(),
        "the other party's tool",
        "the surviving entry must be the other party's"
    );
}

#[tokio::test]
async fn a_registration_dropped_inside_the_install_is_dead() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let calls = Arc::new(AtomicUsize::new(0));
    struct DroppingPlugin {
        calls: Arc<AtomicUsize>,
    }
    impl Plugin for DroppingPlugin {
        fn name(&self) -> &str {
            "dropping-plugin"
        }

        fn install(&self, host: &mut Host) -> PluginHandle {
            let observer = host.hook(Arc::new(FixedObserver {
                name: "dropped",
                decision: HookDecision::Allow,
                panic: false,
                calls: Arc::clone(&self.calls),
            }));
            // The plugin drops the registration: it must not be
            // resurrected by the install's bookkeeping.
            drop(observer);
            PluginHandle::new(Vec::new())
        }
    }

    let mut host = Host::new();
    let handle = host.install(&DroppingPlugin {
        calls: Arc::clone(&calls),
    });
    assert_eq!(host.registration_count(), 0);
    assert_eq!(handle.registrations().len(), 0);

    host.dispatch_hooks(&before_tool_event(), &ctx).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_ask_is_collected_and_reported_together() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    let mut host = Host::new();
    let _reg1 = host.gate(Arc::new(CountingGate {
        name: "first",
        answer: Answer::Decision(HookDecision::Ask {
            reason: "first reason".to_string(),
        }),
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let _reg2 = host.gate(Arc::new(CountingGate {
        name: "second",
        answer: Answer::Decision(HookDecision::Ask {
            reason: "second reason".to_string(),
        }),
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    let arbitration = host.dispatch_gates(&before_tool_event(), &ctx).await;

    assert_eq!(arbitration.decision, HookDecision::Allow);
    assert_eq!(
        arbitration.asks,
        vec!["first reason".to_string(), "second reason".to_string()]
    );
}

// A delegator the host can register, distinguishable by its tool name.
struct NamedDelegator {
    name: &'static str,
}

impl DelegatingTool for NamedDelegator {
    fn definitions(&self) -> Vec<elysper_provider::ToolDefinition> {
        vec![elysper_provider::ToolDefinition::new(
            self.name,
            "delegates",
            serde_json::json!({"type": "object"}),
        )]
    }

    fn execute(
        &self,
        _call: crate::agent::re_act::delegation::DelegationCall<'_>,
    ) -> futures::future::BoxFuture<'static, ToolOutput> {
        Box::pin(async { ToolOutput::text("delegated") })
    }
}

#[test]
fn a_withdrawn_delegator_leaves_the_host() {
    let mut host = Host::new();
    let registration =
        host.delegator(Arc::new(NamedDelegator { name: "task" }));
    assert_eq!(host.delegators().len(), 1);
    assert_eq!(host.registration_count(), 1);

    drop(registration);

    assert!(host.delegators().is_empty());
    assert_eq!(host.registration_count(), 0);
}

#[test]
fn the_hosts_delegators_keep_their_registration_order() {
    let mut host = Host::new();
    let _first = host.delegator(Arc::new(NamedDelegator { name: "one" }));
    let _second = host.delegator(Arc::new(NamedDelegator { name: "two" }));

    let names: Vec<String> = host
        .delegators()
        .iter()
        .map(|delegator| delegator.definitions()[0].name.clone())
        .collect();
    assert_eq!(names, vec!["one".to_string(), "two".to_string()]);
}

// R4: one host is one policy. Every agent over a deployment binds
// the same registry, so the repeated bind is a no-op — and a second,
// *different* registry is refused rather than silently stranding the
// tokens the first one holds.
#[test]
fn rebinding_the_same_registry_is_a_no_op() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));

    assert!(host.bind(Arc::clone(&registry)).is_empty());
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    assert!(
        host.diagnostics().is_empty(),
        "the second bind of the same registry is not a diagnostic"
    );
    assert!(registry.contains("hosted"));
    drop(registration);
    assert!(
        !registry.contains("hosted"),
        "the single binding is the one the withdrawal targets"
    );
}

#[test]
fn rebinding_a_different_registry_keeps_the_original_binding() {
    let first = Arc::new(ToolRegistry::new());
    let second = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&first)).is_empty());

    let refusals = host.bind(Arc::clone(&second));

    assert_eq!(refusals.len(), 1, "a second, different registry is refused");
    assert_eq!(host.diagnostics().len(), 1, "and recorded once on the host");
    assert_eq!(
        host.diagnostics()[0].kind,
        DiagnosticKind::RegistryAlreadyBound
    );
    assert!(
        second.descriptors().is_empty(),
        "the refused registry receives nothing"
    );

    // A tool registered after the refusal joins the original
    // registry, because that is still the binding the host holds.
    let late = host.tool(Arc::new(NamedTool {
        name: "late",
        description: "a late tool",
    }));
    assert!(first.contains("late"));
    assert!(!second.contains("late"));

    drop(late);
    assert!(!first.contains("late"));
    drop(registration);
    assert!(!first.contains("hosted"));
}

// The refusals a bind reports are that call's own, not a slice of the
// host's diagnostic history. A consumer that folds the returned list
// therefore cannot re-report an earlier bind's refusal, and the
// history's cap cannot drop a refusal while a caller still needs it.
#[test]
fn bind_reports_only_the_refusals_of_that_call() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let held = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&registry)).is_empty());
    assert!(
        host.bind(Arc::clone(&registry)).is_empty(),
        "a repeated bind of the same registry refuses nothing"
    );

    let elsewhere = Arc::new(ToolRegistry::new());
    let refused = host.bind(Arc::clone(&elsewhere));
    assert_eq!(refused.len(), 1, "one refusal for the one bind");
    assert_eq!(refused[0].kind, DiagnosticKind::RegistryAlreadyBound);
    assert_eq!(
        host.diagnostics().len(),
        1,
        "the history holds the same single refusal"
    );
    drop(held);
}

// A pending tool the registry will not take is reported by the bind
// itself, so the caller learns it without reading the history.
#[test]
fn bind_reports_a_tool_the_registry_refuses() {
    let registry = Arc::new(ToolRegistry::new());
    assert!(registry.register_core_entry(elysper_tool::ToolEntry::new(
        Arc::new(NamedTool {
            name: "builtin",
            description: "the core tool",
        })
    )));
    let mut host = Host::new();
    let held = host.tool(Arc::new(NamedTool {
        name: "builtin",
        description: "the host's tool",
    }));

    let refusals = host.bind(Arc::clone(&registry));

    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].kind, DiagnosticKind::ToolNameConflict);
    assert_eq!(refusals[0].source, "builtin");
    assert!(refusals[0].detail.contains("core tool"));
    assert_eq!(host.diagnostics(), refusals, "the history agrees");
    drop(held);
}
