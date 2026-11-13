//! Loop-lifecycle tests, split by concern so each file stays
//! under the 400-line reviewability cap.

mod approval;
mod approval_dispatch;
mod approval_resume;
mod cancellation;
mod log_writer;
mod max_iterations;
mod one_row_per_turn;
mod steer;
mod step_pairing;
mod streaming;
mod structural_rows;
mod structured_output;
mod text_only;
mod tool_execution;
mod tool_projection;
mod tool_restriction;
mod writer_history;

#[tokio::test]
async fn table_driven_runs_every_fixture() {
    macro_rules! run {
        ($m:ident :: $f:ident) => {
            $m::$f().await;
        };
    }

    run!(approval::read_only_tools_are_never_asked_about);
    run!(approval::a_high_risk_call_suspends_before_it_runs);
    run!(approval::a_writerless_ask_is_refused_not_suspended);
    run!(approval_dispatch::a_high_risk_delegated_call_is_blocked);
    run!(approval_dispatch::dispatch_is_recorded_for_high_and_unknown_calls);
    run!(approval_dispatch::a_failing_dispatch_flush_prevents_execution);
    run!(approval_resume::resume_dispatches_the_approved_call_and_continues);
    run!(approval_resume::resume_must_cover_every_open_interrupt);
    run!(
        approval_resume::a_changed_tool_schema_blocks_resume_with_config_drift
    );
    run!(approval_resume::a_host_ask_approves_the_guarded_arguments);
    run!(approval_resume::resume_refuses_a_dispatched_high_risk_call);
    run!(approval_resume::resume_retries_a_low_idempotent_dispatched_call);
    run!(approval_resume::a_fresh_run_over_open_approvals_is_refused);
    run!(approval_resume::a_rejected_interrupt_writes_an_is_error_result);
    run!(approval_resume::a_resolved_decision_executes_exactly_once);
    run!(approval_resume::resume_decisions_must_cover_every_open_interrupt);
    run!(approval_resume::resume_decisions_empty_list_is_refused);
    run!(approval_resume::resume_decisions_reject_an_unknown_interrupt);
    run!(approval_resume::resume_decisions_apply_by_id_not_by_position);
    run!(log_writer::a_completed_run_flushes_its_structural_rows);
    run!(log_writer::a_completed_run_brackets_its_steps_with_one_turn);
    run!(log_writer::a_crashed_tool_call_repairs_to_a_balanced_tail);
    run!(log_writer::a_cancelled_run_closes_its_turn_with_the_cancel_reason);
    run!(log_writer::a_run_that_fails_setup_closes_its_turn_as_error);
    run!(log_writer::a_crash_in_the_resume_window_repairs_the_reopened_turn);
    run!(cancellation::a_token_cancelled_before_the_run_ends_at_setup);
    run!(cancellation::a_token_cancelled_mid_run_reports_the_interruption);
    run!(cancellation::a_completed_run_ends_with_one_completed_terminal);
    run!(cancellation::panicking_tool_still_reports_session_end);
    run!(cancellation::panicking_delegator_still_reports_session_end);
    run!(cancellation::panicking_strategy_still_reports_session_end);
    run!(cancellation::panicking_provider_still_reports_session_end);
    run!(step_pairing::every_step_start_has_a_matching_end);
    run!(step_pairing::reasoning_reaches_the_next_request);
    run!(structural_rows::structural_rows_go_through_the_writer);
    run!(structural_rows::the_request_header_names_the_configured_provider);
    run!(structural_rows::a_failed_turn_records_its_message);
    run!(max_iterations::run_emits_warning_at_max_iterations);
    run!(max_iterations::with_max_iterations_overrides_default);
    run!(writer_history::a_writer_backed_run_ignores_input_history);
    run!(writer_history::conflicting_input_history_ends_the_run);
    run!(writer_history::a_writer_backed_run_does_not_duplicate_the_input);
    run!(writer_history::a_writer_backed_run_reads_its_turns_back_from_the_log);
    run!(writer_history::a_host_history_that_prefixes_the_log_is_accepted);
    run!(writer_history::a_writer_backed_run_sends_attachments_once_and_warns);
    run!(writer_history::a_writer_backed_prune_warns_once_per_run);
    run!(one_row_per_turn::a_streamed_reply_is_one_row_and_one_message_after_resume);
    run!(one_row_per_turn::a_reply_with_a_tool_call_keeps_text_and_tool_use_in_one_row);
    run!(one_row_per_turn::a_failed_tool_leaves_its_failure_on_the_row);
    run!(one_row_per_turn::deltas_still_stream_as_events_but_are_not_persisted);
    run!(streaming::streamed_text_deltas_commit_as_one_block);
    run!(structured_output::declared_output_schema_warns_when_never_submitted);
    run!(structured_output::no_output_schema_means_no_warning);
    run!(structured_output::satisfied_output_schema_warns_not);
    run!(text_only::run_emits_full_event_lifecycle_for_text_only_response);
    run!(text_only::isdone_text_does_not_duplicate_streamed_chunks);
    run!(text_only::isdone_text_emitted_only_when_no_streamed_chunks);
    run!(text_only::isdone_tool_calls_dedup_against_streamed_tool_call_end);
    run!(tool_execution::token_usage_emitted_per_iteration_can_be_aggregated);
    run!(tool_execution::run_executes_tool_and_emits_tool_result);
    run!(
        tool_execution::cancelled_mid_sequential_batch_tags_undispatched_calls
    );
    run!(tool_projection::tool_definitions_project_exposure_and_promote_after_a_call);
    run!(tool_projection::tool_surface_policy_narrows_advertisement_but_not_dispatch);
    run!(tool_projection::run_emits_tool_progress_for_streaming_tool);
    run!(
        tool_restriction::call_to_a_hidden_tool_is_refused_without_executing_it
    );
}
