//! Tool-result commit: fold the batch's outputs back into the
//! live transcript and publish them.

use std::sync::Arc;

use elysper_provider::{
    Content,
    ContentPart,
    Message,
    TextContent,
    ToolResult,
    ToolUse,
};
use elysper_tool::ToolOutput;
use tracing::info;

use super::{super::ReActLoop, text_parts};
use crate::{agent::re_act::state::RunState, events::AgentEvent};

pub(crate) async fn commit_all_results(
    this: &ReActLoop,
    session: &mut RunState,
    calls: &[ToolUse],
    mut outputs: Vec<Option<ToolOutput>>,
) {
    // A held call has no result to commit: its slot stays `None`
    // until a resume approves it. Every other loop-exit path fills
    // its unanswered slots with a cause-tagged error, so a `None`
    // here is a held call (an unfilled ordinary call would be a bug,
    // but inventing an error row for it would make repair believe
    // the call was answered).
    let held = outputs.iter().filter(|slot| slot.is_none()).count();
    let mut error_count = 0usize;
    for (idx, call) in calls.iter().enumerate() {
        let Some(mut output) = outputs[idx].take() else {
            continue;
        };
        let is_error = output.is_error.unwrap_or(false);
        if is_error {
            error_count += 1;
        }
        // Steering: rewrite the textual projection of successful
        // outputs, then append the tool-result hints. Error outputs
        // pass through verbatim; non-text parts are preserved.
        if !is_error {
            rewrite_output(this, session, call, &mut output).await;
        }
        commit_tool_result(this, session, call, output).await;
    }
    info!(
        tool_call_count = calls.len(),
        error_count,
        held,
        history_len = session.messages.len(),
        "step 4 execute_tools: all ToolResults committed to history"
    );
}

// Push the user turns an inbox delivered while the tool batch was
// open. A request may not interleave a user message between a
// `tool_use` and its `tool_result`, so they waited here.
fn release_deferred_user(session: &mut RunState) {
    if !session.pending_tool_calls.is_empty()
        || session.deferred_user.is_empty()
    {
        return;
    }
    let deferred = std::mem::take(&mut session.deferred_user);
    for message in deferred {
        Arc::make_mut(&mut session.messages).push(message);
        // The host wrote the row; this loop does not know its ordinal.
        session.surface_seqs.push(0);
    }
}

// Rewrite one successful tool output: the host's output
// transformers chain over the text, then the `AppendToToolResult`
// hints that name this tool are appended.
async fn rewrite_output(
    this: &ReActLoop,
    session: &RunState,
    call: &ToolUse,
    output: &mut ToolOutput,
) {
    let text = text_parts(&output.content);
    if text.is_empty() {
        return;
    }
    let mut text = this
        .caps
        .control
        .host
        .transform_output(text, &call.name, &session.agent_state)
        .await;
    for reminder in this
        .caps
        .control
        .host
        .tool_result_hints(&call.name, &session.agent_state)
    {
        text.push_str("\n\n[reminder] ");
        text.push_str(&reminder);
    }
    replace_text_parts(&mut output.content, text);
}

fn replace_text_parts(content: &mut Vec<ContentPart>, text: String) {
    let insert_at = content
        .iter()
        .position(|part| matches!(part, ContentPart::Text(_)))
        .unwrap_or(content.len());
    content.retain(|part| !matches!(part, ContentPart::Text(_)));
    content.insert(
        insert_at.min(content.len()),
        ContentPart::Text(TextContent {
            text,
            cache_control: None,
        }),
    );
}

pub(crate) async fn commit_tool_result(
    this: &ReActLoop,
    session: &mut RunState,
    call: &ToolUse,
    output: ToolOutput,
) {
    let is_error = output.is_error.unwrap_or(false);
    let truncated_by = output
        .truncated_by
        .as_ref()
        .and_then(|t| serde_json::to_value(t).ok());
    // `structured_output` validates before returning and marks a
    // violation `is_error`, so a non-error result is exactly a
    // schema-valid submission. This is the one funnel every tool result
    // passes through, so it is the one place the fact can be recorded
    // for the run's finalize to read.
    if call.name == elysper_tool::STRUCTURED_OUTPUT_TOOL_NAME && !is_error {
        session.structured_output = true;
    }
    // `output` is owned here; move its `content` / `metadata` out
    // and clone `content` once for the message — the event takes
    // the original. The row serializes from a borrow of the
    // message before it moves into history, so the commit costs
    // exactly one `Vec<ContentPart>` copy.
    let elysper_tool::ToolOutput {
        content, metadata, ..
    } = output;
    let message =
        Message::tool(Content::parts(content.clone()), call.id.clone());
    let row = this.sink.has_writer().then(|| {
        serde_json::to_value(&message).expect("tool result serializes")
    });
    // The tool result is a harness-owned log fact: write it here,
    // on the loop's own task, and land it BEFORE the event goes
    // out — a subscriber that sees the tool result reads a log
    // that already holds its row. A failed flush does not drop
    // the row (it waits in the writer's spill); the run's
    // fail-fast rule then surfaces the failure at the next step
    // boundary.
    let tool_result_seq = row.map(|data| {
        this.sink
            .append_typed(elysper_session::SessionEvent::ToolResult {
                seq: 0,
                ts: String::new(),
                data,
                // The message payload carries the tool's output, not
                // the failure; the flag rides beside it so the row
                // says what the live event below says.
                is_error,
                surface_op: None,
            })
    });
    if tool_result_seq.is_some()
        && let Err(reason) = this.flush_log().await
    {
        tracing::warn!(
            end_reason = ?reason,
            "tool-result flush failed; the row waits in the spill"
        );
    }
    Arc::make_mut(&mut session.messages).push(message);
    session.surface_seqs.push(tool_result_seq.unwrap_or(0));
    // The call is answered: the next user turn may follow it.
    session.pending_tool_calls.remove(&call.id);
    release_deferred_user(session);
    this.sink
        .emit(AgentEvent::Model(ContentPart::ToolResult(ToolResult {
            tool_use_id: call.id.clone(),
            tool_name: Some(call.name.clone()),
            content,
            structured_content: None,
            is_error: Some(is_error),
            metadata,
            truncated_by,
        })));
}
