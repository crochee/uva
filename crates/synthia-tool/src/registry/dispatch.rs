//! Dispatching a batch of tool uses as one stream, and the
//! per-tool drain that turns a `Tool::stream` into exactly one
//! result. Method continuation of
//! [`ToolRegistry`](super::ToolRegistry).
//!
//! Dispatch is spawn-free: the merged stream returned by
//! [`ToolRegistry::run_stream`] is itself the executor. Polling it
//! drives every in-flight tool drain; dropping it cancels them
//! where they stand. No per-call task, channel, or runtime is
//! involved, so the crate stays runtime-free.

use std::{
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use futures::{FutureExt, Stream, StreamExt};
use parking_lot::Mutex;

use super::{DispatchPlan, ToolRegistry};
use crate::{
    traits::{StreamOutput, Tool},
    types::{Context, ToolOutput},
};

/// The slot a draining tool call hands its items to the consumer
/// through, one at a time.
type Slot = Arc<Mutex<Option<StreamOutput>>>;

impl ToolRegistry {
    /// Dispatch a batch of tool uses as a stream of `(call_id,
    /// StreamOutput)` items. Each input `ToolUse` yields zero-or-more
    /// `Progress` items then exactly one `Result`; if the tool's
    /// underlying stream closes without a `Result`, this method
    /// synthesizes an error `Result`. Concurrency is bounded by
    /// `self.max_concurrent`. Cancellation is implicit: dropping the
    /// returned stream aborts every in-flight tool call.
    pub fn run_stream(
        &self,
        tool_uses: Vec<synthia_provider::ToolUse>,
        context: Context,
    ) -> impl Stream<Item = (String, StreamOutput)> + Send + 'static {
        let max_concurrent = self.max_concurrent.max(1);
        // Phase 1: resolve every call under one read-lock span, so
        // the whole batch pays for the lock once and no await point
        // ever holds it.
        let resolved: Vec<(String, DispatchPlan)> = {
            let inner = self.inner.read();
            tool_uses
                .into_iter()
                .map(|tu| {
                    let plan =
                        self.resolve_plan_locked(&inner, &tu.name, tu.input);
                    (tu.id, plan)
                })
                .collect()
        };
        // Phase 2: drive the per-call streams from the merged
        // stream. `flatten_unordered` keeps at most
        // `max_concurrent` drains in flight (the old semaphore's
        // bound) and yields items in arrival order.
        futures::stream::iter(resolved)
            .map(move |(call_id, plan)| {
                let stream = execute_dispatch_plan(plan, context.clone());
                stream.map(move |item| (call_id.clone(), item))
            })
            .flatten_unordered(max_concurrent)
    }

    /// Dispatch a single `ToolUse` and stream its `StreamOutput` items.
    /// Per-call sibling of [`Self::run_stream`]: same hidden-refusal +
    /// JSON-schema-validation contract, same panic-recovery +
    /// missing-`Result` synthesis, without the batch plumbing.
    ///
    /// Stream yields zero-or-more `Progress` + exactly one `Result`
    /// (synthesised on contract violation / panic). Dropping the
    /// stream aborts the in-flight call. NotFound / Invalid / Hidden
    /// cases synthesise a single `is_error` `Result` and close.
    pub fn dispatch_one(
        &self,
        call: synthia_provider::ToolUse,
        context: &Context,
    ) -> Pin<Box<dyn Stream<Item = StreamOutput> + Send + 'static>> {
        let plan = self.resolve_dispatch(&call.name, call.input);
        execute_dispatch_plan(plan, context.clone())
    }
}

/// Turn one resolved [`DispatchPlan`] into a stream: an executable
/// call becomes a panic-isolated drain, a refusal becomes a single
/// `is_error` `Result`.
fn execute_dispatch_plan(
    plan: DispatchPlan,
    ctx: Context,
) -> Pin<Box<dyn Stream<Item = StreamOutput> + Send + 'static>> {
    match plan {
        DispatchPlan::Execute(tool, input) => {
            tool_call_stream(tool, input, ctx)
        }
        DispatchPlan::Refused { tool_name, reason } => {
            Box::pin(futures::stream::iter([StreamOutput::Result(
                reason.tool_output(&tool_name),
            )]))
        }
    }
}

/// One tool call draining as a [`Stream`]: the drain logic (span +
/// metrics recording, panic recovery, missing-`Result` synthesis)
/// runs inside a boxed future that hands items to the consumer one
/// at a time through a slot. The consumer's `poll_next` is what
/// drives the future, so there is no spawn and no channel —
/// dropping the stream stops the call wherever it is.
struct ToolCallStream {
    /// The drain future; never polled after `Ready` (see
    /// `finished`).
    producer: Pin<Box<dyn Future<Output = ()> + Send>>,
    slot: Slot,
    /// Set once `producer` has completed — a boxed future must not
    /// be polled again after `Ready`.
    finished: bool,
}

impl Stream for ToolCallStream {
    type Item = StreamOutput;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        // Drive the drain with the consumer's waker: the underlying
        // tool stream registers it, so its wakeups reach this poll.
        if self.producer.as_mut().poll(cx).is_ready() {
            self.finished = true;
        }
        // Whatever the producer did, surface an item if it placed
        // one in the slot — a `Pending` producer with a full slot is
        // the "item published, waiting to be taken" state.
        match self.slot.lock().take() {
            item @ Some(_) => Poll::Ready(item),
            None if self.finished => Poll::Ready(None),
            None => Poll::Pending,
        }
    }
}

/// Build the panic-isolated drain stream for one tool call.
fn tool_call_stream(
    tool: Arc<dyn Tool>,
    input: serde_json::Value,
    ctx: Context,
) -> Pin<Box<dyn Stream<Item = StreamOutput> + Send + 'static>> {
    let slot: Slot = Arc::new(Mutex::new(None));
    // R-OPT-1 Step 5.3: layer `synthia.span.kind` + `otel.kind`
    // onto the `tool.execute` span so the harness-side wrap (see
    // `route.rs::dispatch_tool_call`) and the registry-side span
    // share the same typed-kind fields. The harness side also
    // emits the OTel metric (`record_tool_call`)—the registry side
    // doesn't, to keep synthia-tool runtime-feature-clean.
    let span = tracing::info_span!(
        "tool.execute",
        synthia.span.name = "tool.execute",
        synthia.span.kind =
            synthia_telemetry::SpanKind::Tool.as_str(),
        otel.kind = synthia_telemetry::SpanKind::Tool.otel_kind(),
        tool.name = %tool.name(),
        exception.type = tracing::field::Empty,
        exception.message = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    );
    let producer =
        drive_tool_call(Arc::clone(&tool), input, ctx, Arc::clone(&slot), span);
    Box::pin(ToolCallStream {
        producer: Box::pin(producer),
        slot,
        finished: false,
    })
}

/// Hand `item` to the consumer and wait until it has been taken
/// before the drain pulls the next one — the backpressure that
/// keeps a runaway tool from out-pacing its consumer.
async fn publish(slot: &Slot, item: StreamOutput) {
    {
        let mut guard = slot.lock();
        debug_assert!(
            guard.is_none(),
            "dispatch slot must be empty before publish"
        );
        *guard = Some(item);
    }
    // The consumer is the driver: it takes the item in the same
    // poll that observed this `Pending` and re-polls this future on
    // its next `poll_next`, so no waker registration is needed —
    // waking here would only spin.
    futures::future::poll_fn(|_| {
        if slot.lock().is_some() {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
}

/// Terminal bookkeeping (span fields + counters) for one tool call,
/// paid BEFORE the item is handed over: a consumer that stops at
/// the `Result` — the common shape, one Result per call — may drop
/// the stream, and the recording must not depend on that.
fn record_terminal(
    span: &tracing::Span,
    tool_name: &str,
    out: &ToolOutput,
    started: std::time::Instant,
) {
    record_tool_outcome(span, out);
    synthia_telemetry::record_tool_outcome_metric(
        tool_name,
        out.is_error.unwrap_or(false),
    );
    synthia_telemetry::record_tool_duration(
        tool_name,
        started.elapsed().as_secs_f64(),
    );
}

/// Drain a tool's `Tool::stream` into the slot, recording the
/// outcome on `span`, and synthesise a `Result` if the stream
/// closes without one OR the underlying future panics.
async fn drive_tool_call(
    tool: Arc<dyn Tool>,
    input: serde_json::Value,
    ctx: Context,
    slot: Slot,
    span: tracing::Span,
) {
    let started = std::time::Instant::now();
    let stream = tool.stream(input, &ctx);
    let mut stream = std::pin::pin!(stream);
    let mut saw_result = false;

    let drain = AssertUnwindSafe(async {
        while let Some(item) = stream.next().await {
            if let StreamOutput::Result(out) = &item {
                saw_result = true;
                record_terminal(&span, tool.name(), out, started);
            }
            publish(&slot, item).await;
        }
    })
    .catch_unwind()
    .await;

    match drain {
        Ok(()) => {
            // Tool stream closed without a Result — synthesise the
            // contract-violation error (with a Result, everything
            // was already recorded above).
            if !saw_result {
                let out = ToolOutput::error(format!(
                    "tool `{}` stream yielded no Result—contract violation",
                    tool.name()
                ));
                record_terminal(&span, tool.name(), &out, started);
                publish(&slot, StreamOutput::Result(out)).await;
            }
        }
        Err(payload) => {
            // Tool panicked. Record on the span, synthesise an error
            // Result, and hand it to the consumer.
            let msg = synthia_core::panic_message(payload);
            let out = ToolOutput::error(format!(
                "tool `{}` panicked during execution: {}",
                tool.name(),
                msg
            ));
            record_tool_outcome(&span, &out);
            synthia_telemetry::record_tool_panic(tool.name());
            synthia_telemetry::record_tool_duration(
                tool.name(),
                started.elapsed().as_secs_f64(),
            );
            publish(&slot, StreamOutput::Result(out)).await;
        }
    }
}

/// Record the tool outcome into the `tool.execute` span.
///
/// Mirrors the OTel semantic conventions expected by
/// `crates/synthia-tool/tests/tool_span.rs`:
/// - On success: leaves `exception.*` and `otel.status_code` empty.
/// - On error: classifies by output message:
/// - `"timed out"` → `exception.type = "TimeoutError"`
/// - anything else → `exception.type = "ToolError"`
///
/// and records `otel.status_code = "ERROR"`.
fn record_tool_outcome(span: &tracing::Span, out: &ToolOutput) {
    let is_err = out.is_error.unwrap_or(false);
    if !is_err {
        return;
    }
    let message = out
        .content
        .iter()
        .find_map(|c| match c {
            synthia_provider::types::ContentPart::Text(t) => {
                Some(t.text.clone())
            }
            _ => None,
        })
        .unwrap_or_default();
    let exception_type = if message.to_lowercase().contains("timed out") {
        "TimeoutError"
    } else {
        "ToolError"
    };
    span.record("exception.type", exception_type);
    span.record("exception.message", message.as_str());
    span.record("otel.status_code", "ERROR");
}
