//! The host's dispatch snapshot: the registrations one dispatch runs
//! are collected once and reused until a registration changes.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::channel::mpsc;
use uva_context::AgentState;
use uva_core::CancelToken;
use uva_events::AgentEvent;
use uva_steering::{HookDecision, HookEvent, RequestParams};
use uva_tool::ToolOutput;

use crate::agent::{
    host::{
        Gate,
        Hook,
        HookContext,
        Host,
        RequestTransformer,
        ResultTransformer,
    },
    sink::EventSink,
};

// An observer that counts its own calls.
struct CountingObserver {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    panic: bool,
}

#[async_trait]
impl Hook for CountingObserver {
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
        HookDecision::Allow
    }
}

// A gate that counts its own calls.
struct CountingGate {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Gate for CountingGate {
    fn name(&self) -> &str {
        "counting-gate"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(HookDecision::Allow)
    }
}

// A request transformer that counts its own calls.
struct CountingRequestTransformer {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl RequestTransformer for CountingRequestTransformer {
    fn name(&self) -> &str {
        "counting-request-transformer"
    }

    async fn transform(
        &self,
        _params: &mut RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// A result transformer that counts its own calls.
struct CountingResultTransformer {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ResultTransformer for CountingResultTransformer {
    fn name(&self) -> &str {
        "counting-result-transformer"
    }

    async fn transform(
        &self,
        _output: &mut ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn before_tool_event() -> HookEvent {
    HookEvent::BeforeTool {
        tool_name: "shell".to_string(),
        call_id: "c1".to_string(),
        arguments: serde_json::json!({}),
    }
}

// The run-level context a dispatch reads: a state, an event sink and
// a cancellation token of its own.
struct Fixture {
    state: AgentState,
    sink: EventSink,
    cancel: Arc<dyn CancelToken>,
}

impl Fixture {
    fn new() -> Self {
        let (tx, _rx) = mpsc::unbounded::<AgentEvent>();
        Self {
            state: AgentState::with_window(1000),
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

// The snapshot a dispatch runs is the one the host hands out, and it
// is reused until a registration changes: two snapshots over an
// unchanged table are one allocation, and the registration that
// follows invalidates it.
#[tokio::test]
async fn the_observer_snapshot_is_reused_until_a_registration_changes() {
    let mut host = Host::new();
    let mut registrations = Vec::new();
    for _ in 0..3 {
        registrations.push(host.hook(Arc::new(CountingObserver {
            name: "observer",
            calls: Arc::new(AtomicUsize::new(0)),
            panic: false,
        })));
    }

    let first = host.snapshot();
    let second = host.snapshot();
    assert!(
        Arc::ptr_eq(&first, &second),
        "two snapshots over an unchanged table must be one allocation"
    );
    assert_eq!(first.hooks.len(), 3);

    // The fourth observer invalidates the cached snapshot.
    registrations.push(host.hook(Arc::new(CountingObserver {
        name: "observer",
        calls: Arc::new(AtomicUsize::new(0)),
        panic: false,
    })));
    let third = host.snapshot();
    assert!(
        !Arc::ptr_eq(&first, &third),
        "a new registration must invalidate the cached snapshot"
    );
    assert_eq!(third.hooks.len(), 4);
}

// One snapshot carries every dispatch table, and it is reused the
// same way whichever table a registration lands in.
#[tokio::test]
async fn the_gate_and_transformer_tables_ride_the_same_snapshot() {
    let mut host = Host::new();
    let _gate = host.gate(Arc::new(CountingGate {
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let _request =
        host.request_transformer(Arc::new(CountingRequestTransformer {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
    let _result =
        host.result_transformer(Arc::new(CountingResultTransformer {
            calls: Arc::new(AtomicUsize::new(0)),
        }));

    let snapshot = host.snapshot();
    let again = host.snapshot();
    assert!(
        Arc::ptr_eq(&snapshot, &again),
        "the snapshot must be reused between dispatches"
    );
    assert_eq!(
        host.waterfall(crate::seams::TOOL_PRE)
            .map(|chain| chain.len())
            .unwrap_or(0),
        1
    );
    // The llm/request chain carries the gate's entry and the
    // transformer's.
    assert_eq!(
        host.waterfall(crate::seams::LLM_REQUEST)
            .map(|chain| chain.len())
            .unwrap_or(0),
        2
    );
    assert_eq!(snapshot.result_transformers.len(), 1);

    // A registration in any table invalidates the published snapshot.
    let _second_gate = host.gate(Arc::new(CountingGate {
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let rebuilt = host.snapshot();
    assert!(
        !Arc::ptr_eq(&snapshot, &rebuilt),
        "a new gate must invalidate the cached snapshot"
    );

    // The rebuilt snapshot holds the new registration too, not just a
    // different allocation.
    assert_eq!(
        host.waterfall(crate::seams::TOOL_PRE)
            .map(|chain| chain.len())
            .unwrap_or(0),
        2
    );
    assert_eq!(
        host.waterfall(crate::seams::LLM_REQUEST)
            .map(|chain| chain.len())
            .unwrap_or(0),
        3
    );
    assert_eq!(rebuilt.result_transformers.len(), 1);

    let _second_request =
        host.request_transformer(Arc::new(CountingRequestTransformer {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
    let _second_result =
        host.result_transformer(Arc::new(CountingResultTransformer {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
    assert!(
        !Arc::ptr_eq(&rebuilt, &host.snapshot()),
        "a new transformer must invalidate the cached snapshot"
    );
    assert_eq!(
        host.waterfall(crate::seams::LLM_REQUEST)
            .map(|chain| chain.len())
            .unwrap_or(0),
        4
    );
    assert_eq!(host.snapshot().result_transformers.len(), 2);
}

// The dispatch really runs the snapshot: every enabled observer is
// consulted.
#[tokio::test]
async fn a_dispatch_calls_every_enabled_observer() {
    let fixture = Fixture::new();
    let mut host = Host::new();
    let first_calls = Arc::new(AtomicUsize::new(0));
    let second_calls = Arc::new(AtomicUsize::new(0));
    let _first = host.hook(Arc::new(CountingObserver {
        name: "first",
        calls: Arc::clone(&first_calls),
        panic: false,
    }));
    let _second = host.hook(Arc::new(CountingObserver {
        name: "second",
        calls: Arc::clone(&second_calls),
        panic: false,
    }));

    let arbitration = host
        .dispatch_hooks(&before_tool_event(), &fixture.ctx(), &fixture.sink)
        .await;
    assert_eq!(arbitration.decision, HookDecision::Allow);
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
}

// A registration that lands after a dispatch is seen by the next one:
// the cache is invalidated by the registration, not by the dispatch.
#[tokio::test]
async fn an_observer_registered_after_a_dispatch_is_seen_by_the_next_one() {
    let fixture = Fixture::new();
    let mut host = Host::new();
    let late_calls = Arc::new(AtomicUsize::new(0));
    host.dispatch_hooks(&before_tool_event(), &fixture.ctx(), &fixture.sink)
        .await;
    let _late = host.hook(Arc::new(CountingObserver {
        name: "late",
        calls: Arc::clone(&late_calls),
        panic: false,
    }));
    host.dispatch_hooks(&before_tool_event(), &fixture.ctx(), &fixture.sink)
        .await;
    assert_eq!(late_calls.load(Ordering::SeqCst), 1);
}

// An observer that reports when it is released.
struct DropObserver {
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl Hook for DropObserver {
    fn name(&self) -> &str {
        "drop-observer"
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        HookDecision::Allow
    }
}

impl Drop for DropObserver {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

// A withdrawn observer is released: the host's cached snapshot must
// not keep it alive after the host gave it up.
#[tokio::test]
async fn a_withdrawn_observer_is_released() {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut host = Host::new();
    let registration = host.hook(Arc::new(DropObserver {
        dropped: Arc::clone(&dropped),
    }));
    // The snapshot holds the observer, the way a dispatch would. The
    // host's own cache is the only holder left once this scope ends.
    {
        let _snapshot = host.snapshot();
        assert!(
            !dropped.load(Ordering::SeqCst),
            "the observer is still registered"
        );
    }

    drop(registration);
    assert!(
        dropped.load(Ordering::SeqCst),
        "a withdrawn observer must not stay alive through the cached \
         snapshot"
    );
}

// A panicking observer is disabled: the dispatch that hit the panic
// still runs to the end on the old snapshot, and the next dispatch
// skips the observer.
#[tokio::test]
async fn a_panicking_observer_is_disabled_after_the_dispatch_that_hit_it() {
    let fixture = Fixture::new();
    let mut host = Host::new();
    let broken_calls = Arc::new(AtomicUsize::new(0));
    let after_calls = Arc::new(AtomicUsize::new(0));
    let _broken = host.hook(Arc::new(CountingObserver {
        name: "broken",
        calls: Arc::clone(&broken_calls),
        panic: true,
    }));
    let _after = host.hook(Arc::new(CountingObserver {
        name: "after",
        calls: Arc::clone(&after_calls),
        panic: false,
    }));

    let first = host
        .dispatch_hooks(&before_tool_event(), &fixture.ctx(), &fixture.sink)
        .await;
    // The dispatch in flight ran to the end: the observer behind the
    // panicking one was still consulted.
    assert_eq!(first.decision, HookDecision::Allow);
    assert_eq!(after_calls.load(Ordering::SeqCst), 1);

    // The next dispatch skips the disabled observer.
    host.dispatch_hooks(&before_tool_event(), &fixture.ctx(), &fixture.sink)
        .await;
    assert_eq!(broken_calls.load(Ordering::SeqCst), 1);
    assert_eq!(after_calls.load(Ordering::SeqCst), 2);
}
