//! Shared fixtures for the control surface's tests: a peer agent that
//! answers with a fixed text, and a child handle built around it.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use uva_agent::{Agent, AgentInput, ChannelInbox};
use uva_core::{
    AtomicCancelToken,
    CancelToken,
    Clock,
    SharedClock,
    agent::AgentDescriptor,
    sleeper::Sleeper,
};
use uva_events::AgentEvent;
use uva_provider::{ContentPart, TextContent};

use crate::control::{registry::ChildHandle, state::ChildState};

/// A clock a test can advance.
///
/// The lifecycle's TTL is measured on the injected [`Clock`], so a
/// test parks a child by moving time, not by sleeping.
#[derive(Clone)]
pub struct TestClock {
    now: Arc<parking_lot::Mutex<DateTime<Utc>>>,
}

impl TestClock {
    /// A clock pinned to 2026-01-01T00:00:00Z.
    pub fn new() -> Self {
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("the fixture instant parses")
            .with_timezone(&Utc);
        Self {
            now: Arc::new(parking_lot::Mutex::new(start)),
        }
    }

    /// Move the clock forward by `elapsed`.
    pub fn advance(&self, elapsed: std::time::Duration) {
        let seconds = i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX);
        *self.now.lock() += chrono::Duration::seconds(seconds);
    }

    /// The clock as the surface takes it.
    pub fn shared(&self) -> SharedClock {
        SharedClock::new(self.clone())
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock()
    }
}

/// A sleeper that wakes immediately a fixed number of times, then
/// never again.
///
/// The parking loop's first wake is all a test needs: `on_wake` runs
/// before the sleep resolves, so a test can move its clock forward
/// exactly when the loop looks at it — without a race.
pub struct StepSleeper {
    remaining: AtomicUsize,
    on_wake: Box<dyn Fn() + Send + Sync>,
}

impl StepSleeper {
    /// A sleeper that wakes `steps` times, calling `on_wake` first.
    pub fn new(
        steps: usize,
        on_wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            remaining: AtomicUsize::new(steps),
            on_wake: Box::new(on_wake),
        }
    }
}

impl Sleeper for StepSleeper {
    fn sleep(&self, _duration: std::time::Duration) -> BoxFuture<'static, ()> {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) == 0 {
            // Woken out: the loop is left waiting, which is exactly
            // what a finished test wants (the future is dropped).
            return Box::pin(futures::future::pending());
        }
        (self.on_wake)();
        Box::pin(std::future::ready(()))
    }
}

/// A peer that answers with `text` and ends its round.
pub struct EchoAgent {
    descriptor: AgentDescriptor,
    text: String,
}

impl EchoAgent {
    /// A peer named `name` that answers with `text`.
    pub fn new(name: &str, text: &str) -> Self {
        let mut descriptor = uva_harness::default_descriptor(String::new());
        descriptor.name = name.to_string();
        Self {
            descriptor,
            text: text.to_string(),
        }
    }

    /// The peer as the registry stores it.
    pub fn shared(name: &str, text: &str) -> Arc<dyn Agent> {
        Arc::new(Self::new(name, text))
    }
}

#[async_trait::async_trait]
impl Agent for EchoAgent {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        let text = self.text.clone();
        Box::pin(futures::stream::once(async move {
            AgentEvent::Model(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            }))
        }))
    }
}

impl uva_core::registry::RegistryItem for EchoAgent {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// A handle for a child that is running: it has a steer channel, a
/// cancellation token and a peer.
///
/// The inbox the channel belongs to is handed back as well: a steer
/// only lands while the round that drains it is alive.
pub fn running_handle(
    name: &str,
) -> (
    ChildHandle,
    futures::channel::mpsc::UnboundedSender<uva_provider::Message>,
    Arc<AtomicCancelToken>,
    Arc<ChannelInbox>,
) {
    let (inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    let handle = ChildHandle::new(
        ChildState::spawn("child-1", name),
        Arc::clone(&cancel) as Arc<dyn CancelToken>,
    )
    .with_steer(steer.clone())
    .with_peer(EchoAgent::shared(name, "done"))
    .with_depth(1);
    (handle, steer, cancel, inbox)
}
