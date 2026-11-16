//! One tool call's dispatch route: registry lookup, stream drain.

use std::sync::Arc;

use futures::StreamExt;
use tracing::{debug, warn};
use uva_events::SystemEvent;
use uva_provider::ToolUse;
use uva_tool::{Context, DispatchPlan, StreamOutput, ToolOutput};

use super::super::ReActLoop;

/// How many streamed chunks a call may produce between log flushes.
///
/// A chunk row is buffered by the session writer and only becomes
/// durable at a flush; without a cadence the only flushes bracket the
/// whole tool call, so a process that dies mid-tool loses every chunk
/// — exactly when the row exists to survive the process. Flushing
/// every `TOOL_OUTPUT_FLUSH_CHUNKS` chunks bounds what a crash can
/// lose without making the fsync rate follow the pipe's rate.
pub(crate) const TOOL_OUTPUT_FLUSH_CHUNKS: usize = 32;

/// Dispatch one call: the registry owns every tool name, delegation
/// included — the `task` tool is an ordinary registry entry.
pub(crate) async fn dispatch_tool_call(
    this: &ReActLoop,
    call: &ToolUse,
    tool_name: &str,
    call_id: &str,
) -> ToolOutput {
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
    let mut unflushed_chunks = 0usize;
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
                // The live event is the running client's copy; the
                // row is the reload's. Both carry the same chunk, so
                // a tool that streams is not silent in either view.
                let chunk = output
                    .content
                    .iter()
                    .filter_map(|part| part.text())
                    .collect::<Vec<_>>()
                    .join("");
                if !chunk.is_empty() {
                    append_streamed_chunk(
                        this,
                        tool_name,
                        call_id,
                        &chunk,
                        &mut unflushed_chunks,
                    )
                    .await;
                }
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

// Append one streamed chunk and keep the log's durability cadence.
//
// The chunk is durable from the next flush: a cadence here keeps a
// crash from losing a long tool's whole progress. A failed flush is
// not fatal — the rows stay owed and the commit flush retries them —
// so it is reported, not propagated.
async fn append_streamed_chunk(
    this: &ReActLoop,
    tool_name: &str,
    call_id: &str,
    chunk: &str,
    unflushed_chunks: &mut usize,
) {
    this.sink.append_tool_output(tool_name, call_id, chunk);
    *unflushed_chunks += 1;
    if *unflushed_chunks < TOOL_OUTPUT_FLUSH_CHUNKS {
        return;
    }
    *unflushed_chunks = 0;
    if let Err(error) = this.sink.flush_log().await {
        debug!(
            tool_name = %tool_name,
            tool_use_id = %call_id,
            %error,
            "dispatch_tool_call: flushing the streamed output failed; \
             the rows stay owed to the next flush"
        );
    }
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
