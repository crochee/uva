//! Sample phase: transcript assembly, request construction,
//! provider sampling, usage observation, part coalescing, and
//! the assistant-turn commit.

use std::sync::Arc;

use synthia_context::AgentState;
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    Role,
    ToolChoice,
    ToolUse,
};
use synthia_steering::HookEvent;
use synthia_tool::ToolOutput;
use tracing::{debug, info};

use super::{
    super::{
        ChunkAssembler,
        panic::isolate,
        projection::{ToolProjectionCache, tool_definitions},
        state::{RunState, SampleOutcome, Transcript},
    },
    ReActLoop,
    tools,
};
use crate::events::{SessionEndReason, WarningKind};

/// Returns an `Arc` so the hot path clones the refcount, never the messages.
pub(super) fn prepare(
    this: &ReActLoop,
    input: &crate::input::AgentInput,
) -> Transcript {
    debug!(
        agent = %this.caps.model.descriptor.name,
        history_len = input.history.len(),
        has_per_dispatch_ctx = input.prompt_context.is_some(),
        "step 1 prepare: assembling system prompt + history"
    );
    // The system prompt is byte-stable: volatile runtime facts live
    // in the RuntimeContext snapshot seam (see
    // `snapshot_runtime_context`), never in the system prompt—
    // that's the prompt-cache-stability contract.
    let assembled = assemble_system_prompt(this, input);
    let messages = crate::prompt::base_transcript(
        &assembled,
        &input.history,
        input.to_message(),
    );
    debug!(
        agent = %this.caps.model.descriptor.name,
        final_messages_len = messages.len(),
        "step 1 prepare: messages vector ready for LLM"
    );
    Arc::new(messages)
}

fn assemble_system_prompt(
    this: &ReActLoop,
    input: &crate::input::AgentInput,
) -> String {
    let ctx = crate::prompt::resolve_prompt_context(input, this.caps.as_ref());
    let assembled = ctx.assemble(&this.caps.model.descriptor);
    debug!(
        agent = %this.caps.model.descriptor.name,
        assembled_len = assembled.len(),
        skills = ctx.skills.len(),
        peer_agents = ctx.agents.len(),
        "step 1 prepare: system prompt assembled"
    );
    assembled
}

/// The harness deliberately overrides neither temperature nor the token cap.
pub(super) fn completion_request(
    this: &ReActLoop,
    model_hint: Option<&String>,
    projection: &mut ToolProjectionCache,
    messages: &Transcript,
) -> CompletionRequest {
    crate::agent::strategy::build_completion_request(
        model_hint.cloned().unwrap_or_default(),
        Arc::clone(messages),
        tool_definitions(
            this.caps.as_ref(),
            &this.synthetic_defs,
            this.synthetic_fingerprint,
            projection,
        ),
        ToolChoice::Auto,
        None,
    )
}

/// In place on the consumed `parts`, no per-part clone. Only adjacent blocks
/// merge: reasoning merges onto the first part of its run, where a stamped
/// signature already sits.
pub(super) fn coalesce_parts(mut parts: Vec<ContentPart>) -> Vec<ContentPart> {
    // Read cursor scans the originals; `write` marks how many
    // coalesced blocks are final. A merged part is dropped by
    // the final truncate; an unmerged one is swapped down.
    let mut write = 0;
    for read in 0..parts.len() {
        // Split so the merged-into block and the incoming part
        // are provably disjoint borrows.
        let (settled, incoming) = parts.split_at_mut(read);
        if write > 0 && merge_adjacent(&mut settled[write - 1], &incoming[0]) {
            continue;
        }
        parts.swap(write, read);
        write += 1;
    }
    parts.truncate(write);
    parts
}

fn merge_adjacent(prev: &mut ContentPart, next: &ContentPart) -> bool {
    match (prev, next) {
        (ContentPart::Text(prev), ContentPart::Text(next)) => {
            prev.text.push_str(&next.text);
            true
        }
        (ContentPart::Reasoning(prev), ContentPart::Reasoning(next)) => {
            prev.text.push_str(&next.text);
            if prev.signature.is_none() {
                prev.signature = next.signature.clone();
            }
            true
        }
        _ => false,
    }
}

pub(super) fn fail_truncated_tool_batch(
    this: &ReActLoop,
    session: &mut RunState,
    calls: &[ToolUse],
) {
    for call in calls {
        let error_text = format!(
            "Tool call \"{}\" was not executed: the response hit the \
             output token limit, so its arguments may be truncated. \
             Re-issue the tool call with complete arguments.",
            call.name
        );
        tools::commit_tool_result(
            this,
            session,
            call,
            ToolOutput::error(error_text),
        );
    }
}

// ===== sampling pass =================================================

/// `Err(SessionEndReason)` means cancellation or a fatal stream error.
pub(crate) async fn sample_once(
    this: &ReActLoop,
    messages: &mut Transcript,
    agent_state: &mut AgentState,
    projection: &mut ToolProjectionCache,
    model_hint: Option<&String>,
    iteration: usize,
) -> Result<SampleOutcome, SessionEndReason> {
    let req =
        Arc::new(completion_request(this, model_hint, projection, messages));
    info!(
        iteration,
        model = %req.model,
        messages_len = messages.len(),
        tool_definitions_len = req.tools.len(),
        "step 2 sample_once: dispatching streaming completion request to provider"
    );
    notify_provider_start_hooks(this).await;
    // Every hook future was awaited inside the notify loop, so the
    // Arc is back to refcount 1 here and the unwrap succeeds
    // without copying; the fallback keeps the (pathological)
    // shared case correct.
    let req = Arc::try_unwrap(req).unwrap_or_else(|shared| (*shared).clone());

    let (resp, outcome) = match stream_completion(this, req, iteration).await {
        Ok(pair) => pair,
        Err(reason) => return Err(reason),
    };
    observe_provider_success(this, &resp, agent_state).await;
    log_sample_result(iteration, &resp, &outcome);
    Ok(outcome)
}

fn log_sample_result(
    iteration: usize,
    resp: &CompletionResponse,
    outcome: &SampleOutcome,
) {
    info!(
        iteration,
        response_id = %resp.id,
        response_model = %resp.model,
        prompt_tokens = resp.usage.prompt_tokens,
        completion_tokens = resp.usage.completion_tokens,
        cached = resp.cached,
        stop_reason = ?resp.stop_reason,
        assistant_text_len = outcome.result.text.len(),
        tool_call_count = outcome.result.tool_calls.len(),
        "step 2 sample_once: streaming completion returned"
    );
}

async fn notify_provider_start_hooks(this: &ReActLoop) {
    // Decision ignored: `BeforeProviderCall` is observational.
    let _ = super::hooks::dispatch_hook(&this.caps.control.steering, || {
        HookEvent::BeforeProviderCall {
            provider: this.cfg.provider.clone(),
            model: this.cfg.name.clone(),
            estimated_tokens: 0,
        }
    })
    .await;
}

async fn stream_completion(
    this: &ReActLoop,
    req: CompletionRequest,
    iteration: usize,
) -> Result<(CompletionResponse, SampleOutcome), SessionEndReason> {
    let cancel = Arc::clone(&this.run.cancel);
    let cb_cancel = cancel.clone();
    let assembler = ChunkAssembler::new();
    let cb_assembler = assembler.clone();
    let cb_sink = this.sink.clone();
    let result = crate::agent::re_act::panic::catching_panics(
        this.caps.model.provider.complete_with_stream(
            req,
            Some(cancel.clone()),
            Box::new(move |chunk| {
                // Silent mid-chunk drop: the pass-level cancel
                // check right after the stream reports the
                // interruption with its `SessionInterrupted`
                // event; emitting here too would duplicate it.
                if cb_cancel.is_cancelled() {
                    return;
                }
                cb_assembler.ingest(&cb_sink, chunk);
            }),
        ),
    )
    .await;
    // A provider is third-party code; a panic in it must become a
    // session-ending error rather than unwinding the run's task and
    // leaving the caller with a stream that ends without `SessionEnded`.
    let result = match result {
        Ok(inner) => inner,
        Err(message) => {
            return Err(provider_failed(
                this,
                iteration,
                "LLM provider panicked",
                &message,
            )
            .await);
        }
    };

    if this.cancelled("cancelled during LLM stream") {
        return Err(SessionEndReason::Cancelled);
    }
    match result {
        Ok(resp) => {
            let mut outcome = assembler.finalize_outcome(&this.sink);
            // `Arc::make_mut` mutates in place when the
            // assembler's Arc is sole-owner (the common case);
            // when a live event consumer still holds a clone it
            // copies — the only correct way to give this
            // outcome a different `stop_reason` than the one
            // already published.
            Arc::make_mut(&mut outcome.result).stop_reason =
                resp.stop_reason.clone();
            Ok((resp, outcome))
        }
        Err(e) => Err(provider_failed(
            this,
            iteration,
            "LLM stream error",
            &e.to_string(),
        )
        .await),
    }
}

async fn provider_failed(
    this: &ReActLoop,
    iteration: usize,
    context: &str,
    detail: &str,
) -> SessionEndReason {
    let reason = format!("{context}: {detail}");
    // Decision ignored: `OnError` is observational.
    let _ = super::hooks::dispatch_hook(&this.caps.control.steering, || {
        HookEvent::OnError {
            agent_name: this.caps.model.descriptor.name.clone(),
            reason: reason.clone(),
        }
    })
    .await;
    this.sink
        .warn(WarningKind::Hook, Some(iteration), reason.clone());
    SessionEndReason::Error(reason)
}

async fn observe_provider_success(
    this: &ReActLoop,
    resp: &CompletionResponse,
    agent_state: &mut AgentState,
) {
    agent_state.add_token_usage(&resp.usage);
    this.emit_typed(synthia_session::usage(
        resp.usage.prompt_tokens,
        resp.usage.completion_tokens,
        resp.usage.total_tokens,
        resp.usage.reasoning_tokens,
        resp.usage.cache_read_tokens,
        resp.usage.cache_write_tokens,
    ));
    // Consumer-supplied (`Steering`); a panic here must not end the run
    // when every other steering surface degrades.
    isolate(
        &this.sink,
        WarningKind::Hook,
        None,
        "tracker.on_llm_response",
        || {
            this.caps
                .control
                .steering
                .tracker
                .on_llm_response(&resp.usage, agent_state);
        },
    );
    // Decision ignored: `AfterProviderCall` is observational.
    let _ = super::hooks::dispatch_hook(&this.caps.control.steering, || {
        HookEvent::AfterProviderCall {
            provider: this.cfg.provider.clone(),
            model: this.cfg.name.clone(),
            prompt_tokens: resp.usage.prompt_tokens,
            completion_tokens: resp.usage.completion_tokens,
            stop_reason: resp.stop_reason.clone(),
        }
    })
    .await;
}

// ===== assistant commit ==============================================

/// Records every tool call even when the pass produced no text, so `Deferred`
/// promotion stays monotonic across context pruning.
pub(super) fn commit_assistant(
    session: &mut RunState,
    parts: Vec<ContentPart>,
    result: &synthia_provider::SamplingResult,
) {
    // Early-exit before any commit work when this pass produced
    // neither text nor parts.
    if result.text.is_empty() && parts.is_empty() {
        return;
    }
    let parts = coalesce_parts(parts);
    // Keep `Deferred` promotion monotonic: a tool the model has
    // called stays advertised for the rest of the run, even if a
    // context manager later prunes the call from the transcript.
    // `record_call` is idempotent on the fingerprint, so repeat
    // calls across iterations don't allocate.
    for call in &result.tool_calls {
        session.projection.record_call(&call.name);
    }
    Arc::make_mut(&mut session.messages).push(Message {
        role: Role::Assistant,
        content: Content::parts(parts),
        tool_call_id: None,
        name: None,
        tool_result_cleared_at: None,
    });
}

#[cfg(test)]
mod tests {
    use super::{
        super::super::projection::{compute_fingerprint, fnv1a_str},
        coalesce_parts,
    };

    #[test]
    fn identical_inputs_yield_identical_fingerprints() {
        let a = compute_fingerprint(7, 0, 11, 3);
        let b = compute_fingerprint(7, 0, 11, 3);
        assert_eq!(a, b);
    }

    #[test]
    fn registry_version_changes_invalidate_the_fingerprint() {
        let a = compute_fingerprint(7, 0, 11, 3);
        let b = compute_fingerprint(8, 0, 11, 3);
        assert_ne!(a, b);
    }

    #[test]
    fn called_set_changes_invalidate_the_fingerprint() {
        let a = compute_fingerprint(7, fnv1a_str(0, "a"), 11, 3);
        let b = compute_fingerprint(7, fnv1a_str(0, "b"), 11, 3);
        assert_ne!(a, b);
    }

    #[test]
    fn synthetic_count_changes_invalidate_the_fingerprint() {
        let a = compute_fingerprint(7, 0, 11, 3);
        let b = compute_fingerprint(7, 0, 11, 4);
        assert_ne!(a, b);
    }

    #[test]
    fn called_fingerprint_is_order_sensitive() {
        let a = fnv1a_str(fnv1a_str(0, "ab"), "c");
        let b = fnv1a_str(fnv1a_str(0, "c"), "ab");
        assert_ne!(a, b);
    }

    #[test]
    fn called_fingerprint_changes_when_a_name_is_added() {
        let a = fnv1a_str(0, "a");
        let b = fnv1a_str(a, "b");
        assert_ne!(a, b);
    }

    #[test]
    fn coalesce_parts_merges_adjacent_blocks_only() {
        use synthia_provider::{ContentPart, ReasoningContent, TextContent};

        let parts = vec![
            ContentPart::Text(TextContent {
                text: "run one".into(),
                cache_control: None,
            }),
            ContentPart::Text(TextContent {
                text: " run two".into(),
                cache_control: None,
            }),
            ContentPart::Reasoning(ReasoningContent {
                text: "think".into(),
                signature: Some("sig".into()),
            }),
            ContentPart::Reasoning(ReasoningContent {
                text: " more".into(),
                signature: None,
            }),
            ContentPart::Text(TextContent {
                text: "after".into(),
                cache_control: None,
            }),
            ContentPart::ToolUse(synthia_provider::ToolUse {
                id: "c1".into(),
                name: "echo".into(),
                input: serde_json::json!({}),
            }),
        ];
        let out = coalesce_parts(parts);
        let shape: Vec<String> = out
            .iter()
            .map(|p| match p {
                ContentPart::Text(t) => format!("text:{}", t.text),
                ContentPart::Reasoning(r) => {
                    format!("reason:{}:{:?}", r.text, r.signature)
                }
                ContentPart::ToolUse(tu) => format!("call:{}", tu.id),
                other => format!("other:{other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                "text:run one run two".to_string(),
                "reason:think more:Some(\"sig\")".to_string(),
                "text:after".to_string(),
                "call:c1".to_string(),
            ],
            "adjacent same-kind blocks merge; the signature of the \
         first reasoning block survives; kinds never mix"
        );
    }
}
