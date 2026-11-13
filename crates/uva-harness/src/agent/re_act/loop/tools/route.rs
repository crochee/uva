//! One tool call's dispatch route: delegator lookup, registry
//! lookup, stream drain.

use std::{
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use futures::{StreamExt, future::Either, pin_mut, task::noop_waker_ref};
use tracing::{debug, warn};
use uva_provider::ToolUse;
use uva_tool::{Context, DispatchPlan, StreamOutput, ToolOutput};

use super::super::ReActLoop;
use crate::{agent::re_act::delegation::DelegationCall, events::SystemEvent};

/// Dispatch one call: a delegator first, then the registry.
pub(crate) async fn dispatch_tool_call(
    this: &ReActLoop,
    call: &ToolUse,
    tool_name: &str,
    call_id: &str,
) -> ToolOutput {
    // Delegators first.
    if let Some(output) = try_delegation(this, call, tool_name).await {
        return output;
    }

    let (tool, input) = match resolve_registry_tool(
        this,
        tool_name,
        call_id,
        call.input.clone(),
    ) {
        Ok(pair) => pair,
        Err(out) => return out,
    };

    // Typed `SpanKind::Tool` span over the dispatch. Use the
    // underlying `tracing::info_span!` macro so the dynamic
    // `tool.name` / `tool.call_id` fields can be declared and
    // populated at the same site. The `uva.span.kind` /
    // `otel.kind` markers are kept identical to the helper so
    // dashboards filter consistently.
    let span = tracing::info_span!(
        "tool_execute",
        uva.span.name = "tool_execute",
        uva.span.kind = uva_telemetry::SpanKind::Tool.as_str(),
        otel.kind = uva_telemetry::SpanKind::Tool.otel_kind(),
        tool.name = %tool_name,
        tool.call_id = %call_id,
        exception.type = tracing::field::Empty,
        exception.message = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    );
    let _entered = span.enter();

    // A panic during a tool call would unwind this task and leave the
    // run unreported; see `catching_panics` for why that matters.
    // `this.ctx` is built once per run, so the dispatch pays no
    // workspace clone here.
    let ran = crate::agent::re_act::panic::catching_panics(drain_one_tool(
        this, tool, input, &this.ctx, tool_name, call_id,
    ))
    .await;

    let outcome_label = match &ran {
        Ok(out) if !out.is_error.unwrap_or(false) => "ok",
        Ok(_) | Err(_) => "error",
    };

    match ran {
        Ok(output) => {
            span.record("otel.status_code", "OK");
            uva_telemetry::record_tool_call(tool_name, outcome_label);
            output
        }
        Err(message) => {
            span.record("exception.type", "panic");
            span.record("exception.message", message.as_str());
            span.record("otel.status_code", "ERROR");
            uva_telemetry::record_tool_call(tool_name, "panicked");
            warn!(
                tool_name = %tool_name,
                tool_use_id = %call_id,
                panic = %message,
                "dispatch_tool_call: tool panicked; reporting it as a \
                 tool error so the run can still end normally"
            );
            panicked_tool_output(tool_name, "", &message)
        }
    }
}

async fn drain_one_tool(
    this: &ReActLoop,
    tool: Arc<dyn uva_tool::Tool>,
    input: serde_json::Value,
    ctx: &Context,
    tool_name: &str,
    call_id: &str,
) -> ToolOutput {
    let mut stream = tool.stream(input, ctx);
    drain_tool_stream(this, &mut stream, tool_name, call_id).await
}

// A delegator gets the cancel token, depth, tool context, and event
// emitter — internals the registry path never sees.
async fn try_delegation(
    this: &ReActLoop,
    call: &ToolUse,
    tool_name: &str,
) -> Option<ToolOutput> {
    let delegator = this.delegators.get(tool_name)?;
    let cancel = Arc::clone(&this.run.cancel);
    let depth = this.run.subagent_depth;
    let (emit_tx, mut emit_rx) = futures::channel::mpsc::unbounded();
    // Forward events from the plugin's `UnboundedSender` into
    // the run's event sink while the executor runs.
    let sink = this.sink.clone();
    let call_input = DelegationCall {
        call,
        cancel,
        depth,
        context: &this.ctx,
        emit: emit_tx,
        subagent_writers: this.caps.env.subagent_writers.clone(),
    };
    // A delegator is a plugin (it spawns whole subagents), so it is
    // exactly as capable of panicking as a tool is.
    //
    // The executor races the channel, never joins it: a plugin may
    // leak its `emit` sender into a task that outlives `execute`
    // (a delegator spawning subagents does exactly that), and
    // awaiting the channel's close would then hang the dispatch
    // forever. Events win the race while `execute` runs; once it
    // resolves, whatever it already buffered is drained without
    // blocking and the output returns.
    let executor = crate::agent::re_act::panic::catching_panics(
        delegator.execute(call_input),
    );
    pin_mut!(executor);
    let output = loop {
        let next_event = emit_rx.next();
        pin_mut!(next_event);
        match futures::future::select(&mut executor, next_event).await {
            Either::Left((output, _)) => break output,
            Either::Right((event, _)) => match event {
                Some(event) => sink.emit(event),
                // Channel closed: every sender dropped, so the
                // executor is the only thing left to wait on.
                None => break executor.await,
            },
        }
    };
    // Buffered-but-unread events still reach the sink; the receiver
    // (and anything a leaked sender emits afterwards) is then
    // dropped with it.
    let poll_cx = &mut TaskContext::from_waker(noop_waker_ref());
    while let Poll::Ready(Some(event)) = emit_rx.poll_next_unpin(poll_cx) {
        sink.emit(event);
    }
    let name = tool_name.to_string();
    Some(match output {
        Ok(out) => out,
        Err(message) => {
            warn!(
                tool_name = %name,
                panic = %message,
                "dispatch_tool_call: delegator panicked; reporting it \
                 as a tool error so the run can still end normally"
            );
            panicked_tool_output(&name, "delegation: ", &message)
        }
    })
}

// Allow `result_large_err`: the `Err` arm is the model-visible
// `ToolOutput` the caller hands straight back — boxing it would
// add an allocation to every miss for no gain.
#[allow(clippy::result_large_err)]
fn resolve_registry_tool(
    this: &ReActLoop,
    tool_name: &str,
    call_id: &str,
    input: serde_json::Value,
) -> Result<(Arc<dyn uva_tool::Tool>, serde_json::Value), ToolOutput> {
    // The hidden-refusal + optional argument-validation decision
    // is the registry's single `resolve_dispatch`; the loop adds
    // only the AgentEvent-side semantics on top. The inherent
    // method avoids the `Registry` trait's boxed future on this
    // per-tool-call path.
    match this.caps.tools.registry.resolve_dispatch(tool_name, input) {
        DispatchPlan::Execute(tool, input) => Ok((tool, input)),
        DispatchPlan::Refused { tool_name, reason } => {
            warn!(
                tool_name = %tool_name,
                tool_use_id = %call_id,
                reason = ?reason,
                "dispatch_tool_call: refusing a call before dispatch"
            );
            Err(reason.tool_output(&tool_name))
        }
    }
}

async fn drain_tool_stream(
    this: &ReActLoop,
    stream: &mut std::pin::Pin<
        Box<dyn futures::Stream<Item = StreamOutput> + Send + '_>,
    >,
    tool_name: &str,
    call_id: &str,
) -> ToolOutput {
    let mut final_output: Option<ToolOutput> = None;
    loop {
        // Silent mid-stream abort: the loop's cancel handling
        // reports the interruption once the pass unwinds; an
        // event here would fire per streamed tool.
        if this.run.cancel.is_cancelled() {
            debug!(
                tool_name = %tool_name,
                tool_use_id = %call_id,
                "dispatch_tool_call: cancelled mid-stream"
            );
            break;
        }
        match StreamExt::next(stream).await {
            Some(StreamOutput::Progress(output)) => {
                this.sink.system(SystemEvent::ToolProgress {
                    tool_name: tool_name.to_string(),
                    call_id: call_id.to_string(),
                    output,
                });
            }
            Some(StreamOutput::Result(output)) => {
                final_output = Some(output);
                break;
            }
            None => break,
        }
    }
    let result = final_output.unwrap_or_else(|| {
        ToolOutput::error("tool stream closed without producing a Result")
    });
    debug!(
        tool_name = %tool_name,
        tool_use_id = %call_id,
        is_error = result.is_error.unwrap_or(false),
        "dispatch_tool_call: tool stream completed"
    );
    result
}

fn panicked_tool_output(
    tool_name: &str,
    who: &str,
    message: &str,
) -> ToolOutput {
    ToolOutput::error(format!(
        "tool `{tool_name}` panicked during execution: {who}{message}"
    ))
}
