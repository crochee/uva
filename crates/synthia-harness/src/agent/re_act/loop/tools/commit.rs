//! Tool-result commit: fold the batch's outputs back into the
//! live transcript and publish them.

use std::sync::Arc;

use synthia_context::TouchedPair;
use synthia_provider::{
    Content,
    ContentPart,
    Message,
    TextContent,
    ToolResult,
    ToolUse,
};
use synthia_tool::ToolOutput;
use tracing::info;

use super::{super::ReActLoop, did_not_run, text_parts};
use crate::{
    agent::re_act::{r#loop::hooks, state::RunState},
    events::AgentEvent,
};

pub(crate) async fn commit_all_results(
    this: &ReActLoop,
    session: &mut RunState,
    calls: &[ToolUse],
    mut outputs: Vec<Option<ToolOutput>>,
    touched: Vec<TouchedPair>,
) {
    if !touched.is_empty() {
        for (read, write) in touched {
            if let Some(p) = read {
                session.touched.read_files.push(p);
            }
            if let Some(p) = write {
                session.touched.modified_files.push(p);
            }
        }
    }
    // Last resort: every loop-exit path above fills its
    // unanswered slots with a cause-tagged error, so a slot
    // reaching here unfilled means no dispatch path claimed it.
    let unclaimed = outputs.iter().filter(|slot| slot.is_none()).count();
    let mut error_count = 0usize;
    for (idx, call) in calls.iter().enumerate() {
        let mut output = outputs[idx].take().unwrap_or_else(|| {
            did_not_run(call, "no dispatch path claimed it")
        });
        let is_error = output.is_error.unwrap_or(false);
        if is_error {
            error_count += 1;
        }
        // Steering: rewrite the textual projection of successful
        // outputs through the configured transformer, then append
        // any AppendToToolResult-point hints. Error outputs pass
        // through verbatim. Non-text parts (images) are preserved.
        if !is_error {
            let text = text_parts(&output.content);
            if !text.is_empty() {
                let mut text = this
                    .caps
                    .control
                    .steering
                    .output_transformer
                    .transform(text, &call.name, &session.agent_state)
                    .await;
                text = hooks::append_tool_result_hints(
                    &this.caps.control.steering.hints,
                    &call.name,
                    &session.agent_state,
                    text,
                );
                replace_text_parts(&mut output.content, text);
            }
        }
        commit_tool_result(this, session, call, output);
    }
    info!(
        tool_call_count = calls.len(),
        error_count,
        unclaimed,
        history_len = session.messages.len(),
        "step 4 execute_tools: all ToolResults committed to history"
    );
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

pub(crate) fn commit_tool_result(
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
    if call.name == synthia_tool::STRUCTURED_OUTPUT_TOOL_NAME && !is_error {
        session.structured_output = true;
    }
    // `output` is owned here; move its `content` / `metadata` out
    // and clone `content` once for the message — the event takes
    // the original, saving one full `Vec<ContentPart>` copy per
    // committed tool result.
    let synthia_tool::ToolOutput {
        content, metadata, ..
    } = output;
    Arc::make_mut(&mut session.messages).push(Message::tool(
        Content::parts(content.clone()),
        call.id.clone(),
    ));
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
