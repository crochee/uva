//! Sample phase: transcript assembly, request construction,
//! provider sampling, usage observation, part coalescing, and
//! the assistant-turn commit.

use std::sync::Arc;

use elysper_context::AgentState;
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    Role,
    ToolChoice,
    ToolUse,
};
use elysper_steering::{HookDecision, RequestParams};
use elysper_tool::ToolOutput;
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

/// The parts of this turn the session log cannot reproduce: everything
/// that is not text. The host's log row carries the text, so only the
/// bytes need a home in the request.
pub(super) fn attachment_parts(
    input: &crate::input::AgentInput,
) -> Vec<ContentPart> {
    input
        .content
        .iter()
        .filter(|part| !matches!(part, ContentPart::Text(_)))
        .cloned()
        .collect()
}

// How many times this thread assembled a system prompt. The counter
// is thread-local so a parallel test run cannot pollute the count of
// the run under test.
#[cfg(test)]
std::thread_local! {
    static SYSTEM_PROMPT_ASSEMBLIES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// The system prompt is byte-stable: volatile runtime facts live in
// the `RuntimeContext` snapshot seam (see
// `snapshot_runtime_context`), never in the system prompt — that's
// the prompt-cache-stability contract. The manifest was snapshotted
// once at run start (`ReActLoop::from_caps`), so this only renders.
//
// The run assembles the prompt once, at setup, and lays that one
// string out; a hint rewrites the message in `messages`, not this.
pub(super) fn assemble_system_prompt(this: &ReActLoop) -> String {
    #[cfg(test)]
    SYSTEM_PROMPT_ASSEMBLIES.with(|count| count.set(count.get() + 1));
    let ctx = &this.prompt_context;
    let assembled = ctx.assemble(&this.caps.model.descriptor);
    debug!(
        agent = %this.caps.model.descriptor.name,
        assembled_len = assembled.len(),
        skills = ctx.skills.len(),
        peer_agents = ctx.agents.len(),
        plugin_sections = ctx.sections().len(),
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
            &this.delegator_defs,
            this.delegator_fingerprint,
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

pub(super) async fn fail_truncated_tool_batch(
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
        )
        .await;
    }
}

// ===== sampling pass =================================================

/// `Err(SessionEndReason)` means cancellation or a fatal stream error.
pub(crate) async fn sample_once(
    this: &ReActLoop,
    messages: &Transcript,
    agent_state: &mut AgentState,
    projection: &mut ToolProjectionCache,
    model_hint: Option<&String>,
    iteration: usize,
) -> Result<SampleOutcome, SessionEndReason> {
    // The step boundary: a policy can end the run or short-circuit the
    // step before the model is asked anything.
    let arbitration = {
        let ctx = this.hook_context(agent_state);
        this.caps
            .control
            .seams
            .step_pre(&ctx, &this.caps.model.descriptor.name, iteration as u32)
            .await
    };
    match arbitration.decision {
        HookDecision::Terminate { reason } | HookDecision::Block { reason } => {
            return Err(SessionEndReason::Error(reason));
        }
        HookDecision::Allow
        | HookDecision::Ask { .. }
        | HookDecision::ReplaceParams(_) => {}
    }
    let req =
        Arc::new(completion_request(this, model_hint, projection, messages));
    info!(
        iteration,
        model = %req.model,
        messages_len = messages.len(),
        tool_definitions_len = req.tools.len(),
        "step 2 sample_once: dispatching streaming completion request to provider"
    );
    notify_provider_start_hooks(this, agent_state).await;
    // Every hook future was awaited inside the notify loop, so the
    // Arc is back to refcount 1 here and the unwrap succeeds
    // without copying; the fallback keeps the (pathological)
    // shared case correct.
    let mut request =
        Arc::try_unwrap(req).unwrap_or_else(|shared| (*shared).clone());
    rewrite_request(this, agent_state, &mut request).await;

    let (resp, outcome) =
        match stream_completion(this, agent_state, request, iteration).await {
            Ok(pair) => pair,
            Err(reason) => return Err(reason),
        };
    observe_provider_success(this, &resp, agent_state).await;
    log_sample_result(iteration, &resp, &outcome);
    Ok(outcome)
}

// The `before_request` point: the parameters the request goes out
// with. The host's transformers chain over them, and a handler that
// answered `ReplaceParams` replaces them.
//
// The message list is not part of the rewrite surface: the params
// carry it read-only, so a rewrite can change the model, the
// temperature, the budget, the tool choice or the tool schemas — and
// nothing else.
async fn rewrite_request(
    this: &ReActLoop,
    state: &AgentState,
    request: &mut CompletionRequest,
) {
    // Nothing can rewrite the request: skip the whole pass — the
    // params snapshot, the dispatch and the write-back — rather than
    // paying for it on every step. The seam layer owns that check.
    let mut params = RequestParams::from_request(request);
    let ctx = this.hook_context(state);
    this.caps
        .control
        .seams
        .llm_request(&ctx, &this.cfg.provider, &mut params)
        .await;
    params.apply_to(request);
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

async fn notify_provider_start_hooks(this: &ReActLoop, state: &AgentState) {
    // `provider_started` is observational: the point accepts no
    // decision, so a handler that returns one is recorded as a
    // diagnostic and the run continues.
    let ctx = this.hook_context(state);
    this.caps
        .control
        .seams
        .provider_started(&ctx, &this.cfg.provider, &this.cfg.name, 0)
        .await;
}

async fn stream_completion(
    this: &ReActLoop,
    state: &AgentState,
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
                state,
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
            // The turn's row is one `assistant_message`, written
            // by the finalize above. Land it now: by the time the
            // loop moves past this model call, the call's fact is
            // durable — a crash one instruction later still
            // leaves the transcript whole. A failed flush is
            // fatal per the run's fail-fast rule.
            this.flush_log().await?;
            Ok((resp, outcome))
        }
        Err(e) => Err(provider_failed(
            this,
            state,
            iteration,
            "LLM stream error",
            &e.to_string(),
        )
        .await),
    }
}

async fn provider_failed(
    this: &ReActLoop,
    state: &AgentState,
    iteration: usize,
    context: &str,
    detail: &str,
) -> SessionEndReason {
    let reason = format!("{context}: {detail}");
    // `provider_failed` is observational: the point accepts no decision,
    // so a handler that returns one is recorded as a diagnostic.
    let ctx = this.hook_context(state);
    this.caps
        .control
        .seams
        .provider_failed(&ctx, &this.caps.model.descriptor.name, &reason)
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
    this.emit_typed(elysper_session::usage(
        resp.usage.prompt_tokens,
        resp.usage.completion_tokens,
        resp.usage.total_tokens,
        resp.usage.reasoning_tokens,
        resp.usage.cache_read_tokens,
        resp.usage.cache_write_tokens,
    ));
    // Consumer-supplied; a panic here must not end the run when every
    // other observation surface degrades.
    for tracker in &this.caps.control.seams.tracker_table() {
        isolate(
            &this.sink,
            WarningKind::Hook,
            None,
            "tracker.on_llm_response",
            || {
                tracker.on_llm_response(&resp.usage, agent_state);
            },
        );
    }
    // `llm_result` is observational: the point accepts no decision, so a
    // handler that returns one is recorded as a diagnostic.
    let ctx = this.hook_context(agent_state);
    this.caps
        .control
        .seams
        .llm_result(
            &ctx,
            &this.cfg.provider,
            &this.cfg.name,
            resp.usage.prompt_tokens,
            resp.usage.completion_tokens,
            resp.stop_reason.clone(),
        )
        .await;
}

// ===== assistant commit ==============================================

/// Records every tool call even when the pass produced no text, so `Deferred`
/// promotion stays monotonic across context pruning.
pub(super) fn commit_assistant(
    session: &mut RunState,
    parts: Vec<ContentPart>,
    result: &elysper_provider::SamplingResult,
    assistant_seq: u64,
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
        session.pending_tool_calls.insert(call.id.clone());
    }
    Arc::make_mut(&mut session.messages).push(Message {
        role: Role::Assistant,
        content: Content::parts(parts),
        tool_call_id: None,
        name: None,
        tool_result_cleared_at: None,
    });
    session.surface_seqs.push(assistant_seq);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper_context::AgentState;
    use elysper_core::{AtomicCancelToken, SystemClock};
    use elysper_session::{LogWriter, in_memory::InMemorySessionSink};
    use futures::channel::mpsc;

    use super::{
        super::{
            super::{
                projection::{compute_fingerprint, fnv1a_str},
                state::RunState,
            },
            ReActLoop,
        },
        SYSTEM_PROMPT_ASSEMBLIES,
        assemble_system_prompt,
        coalesce_parts,
    };
    use crate::{AgentCaps, agent::sink::EventSink, input::AgentInput};

    // A loop with the default caps: the run's setup needs nothing but
    // a provider and a prompt context.
    fn loop_for_test() -> ReActLoop {
        let provider: Arc<dyn elysper_provider::ModelProvider> =
            Arc::new(elysper_test_support::FakeProvider::new(Vec::new()));
        loop_over(AgentCaps::for_provider(provider))
    }

    // The same loop over a cap bundle the caller assembled.
    fn loop_over(caps: AgentCaps) -> ReActLoop {
        let cfg = caps.model.provider.model_config();
        let prompt_context = caps.run_prompt_context(None);
        let run = RunState::seed(
            Arc::new(AtomicCancelToken::shared()),
            0,
            Arc::new(Vec::new()),
            AgentState::from_config(&cfg),
        );
        let (tx, _rx) = mpsc::unbounded();
        ReActLoop::from_caps(
            Arc::new(caps),
            cfg,
            run,
            EventSink::new(Arc::new(tx), None),
            prompt_context,
        )
    }

    // The run assembles its system prompt once, at setup, and keeps
    // it on the run state; `messages` carries the message it sends.
    #[tokio::test]
    async fn cached_system_prompt_matches_assembled() {
        let before = SYSTEM_PROMPT_ASSEMBLIES.with(|count| count.get());
        let mut loop_ = loop_for_test();
        let state = loop_
            .drive_setup(&AgentInput::text("go"))
            .await
            .expect("the setup builds the run state");
        let assemblies =
            SYSTEM_PROMPT_ASSEMBLIES.with(|count| count.get()) - before;
        assert_eq!(
            assemblies, 1,
            "the run assembles its system prompt exactly once, at setup: \
             {assemblies} assemblies"
        );
        assert_eq!(
            state.system_prompt,
            assemble_system_prompt(&loop_),
            "the run's cached system prompt is the assembled one"
        );
    }

    // The writer-backed transcript lays the same prompt out, and
    // assembles it no more often than the caller's transcript does.
    #[tokio::test]
    async fn a_writer_backed_run_assembles_its_system_prompt_once() {
        let writer = Arc::new(LogWriter::new(
            Arc::new(InMemorySessionSink::new("s")),
            Arc::new(SystemClock),
        ));
        let provider: Arc<dyn elysper_provider::ModelProvider> =
            Arc::new(elysper_test_support::FakeProvider::new(Vec::new()));
        let mut caps = AgentCaps::for_provider(provider);
        caps.env.log_writer = Some(writer);
        let mut loop_ = loop_over(caps);
        let before = SYSTEM_PROMPT_ASSEMBLIES.with(|count| count.get());
        let state = loop_
            .drive_setup(&AgentInput::text("go"))
            .await
            .expect("the setup builds the run state");
        let assemblies =
            SYSTEM_PROMPT_ASSEMBLIES.with(|count| count.get()) - before;
        assert_eq!(
            assemblies, 1,
            "the writer-backed run assembles its system prompt exactly \
             once, at setup: {assemblies} assemblies"
        );
        assert_eq!(
            state.system_prompt,
            assemble_system_prompt(&loop_),
            "the run's cached system prompt is the assembled one"
        );
    }

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
    fn delegator_count_changes_invalidate_the_fingerprint() {
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
        use elysper_provider::{ContentPart, ReasoningContent, TextContent};

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
            ContentPart::ToolUse(elysper_provider::ToolUse {
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
