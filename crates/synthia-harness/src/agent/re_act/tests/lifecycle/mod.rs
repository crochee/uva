//! Loop-lifecycle tests, split by concern so each file stays
//! under the 400-line reviewability cap.

mod cancellation;
mod log_writer;
mod max_iterations;
mod persistence;
mod step_pairing;
mod streaming;
mod structured_output;
mod text_only;
mod tool_execution;
mod tool_projection;
mod tool_restriction;

#[tokio::test]
async fn table_driven_runs_every_fixture() {
    macro_rules! run {
        ($m:ident :: $f:ident) => {
            $m::$f().await;
        };
    }

    run!(log_writer::a_completed_run_flushes_its_structural_rows);
    run!(cancellation::a_token_cancelled_before_the_run_ends_at_setup);
    run!(cancellation::a_token_cancelled_mid_run_reports_the_interruption);
    run!(cancellation::a_completed_run_ends_with_one_completed_terminal);
    run!(cancellation::panicking_tool_still_reports_session_end);
    run!(cancellation::panicking_synthetic_tool_still_reports_session_end);
    run!(cancellation::panicking_strategy_still_reports_session_end);
    run!(cancellation::panicking_provider_still_reports_session_end);
    run!(step_pairing::every_step_start_has_a_matching_end);
    run!(step_pairing::reasoning_reaches_the_next_request);
    run!(max_iterations::run_emits_warning_at_max_iterations);
    run!(max_iterations::with_max_iterations_overrides_default);
    run!(persistence::run_persists_checkpoint_on_cancel);
    run!(persistence::resume_replays_from_snapshot);
    run!(persistence::cancelled_before_tool_persists_checkpoint);
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
