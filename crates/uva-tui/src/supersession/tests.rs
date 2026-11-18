//! The supersession rule's own tests: the conditions, one by one.

use uva_session::RowTag;

use super::*;

#[test]
fn a_content_row_past_the_start_position_retires_the_projection() {
    assert!(retires_projection(
        RowTag::AssistantMessage,
        Some(13),
        12,
        false
    ));
    assert!(retires_projection(RowTag::ToolResult, Some(13), 12, false));
}

#[test]
fn a_row_at_or_before_the_start_position_keeps_the_projection() {
    assert!(!retires_projection(
        RowTag::AssistantMessage,
        Some(12),
        12,
        false
    ));
    assert!(!retires_projection(
        RowTag::AssistantMessage,
        Some(11),
        12,
        false
    ));
}

#[test]
fn a_row_that_is_not_content_keeps_the_projection() {
    // The prompt, the step boundary and the usage frame are not what
    // the projection renders: retiring it on those would blank the
    // streaming answer.
    assert!(!retires_projection(
        RowTag::UserMessage,
        Some(13),
        12,
        false
    ));
    assert!(!retires_projection(RowTag::Step, Some(13), 12, false));
    assert!(!retires_projection(RowTag::Usage, Some(13), 12, false));
}

#[test]
fn a_turn_boundary_retires_the_projection_once_the_run_is_over() {
    assert!(retires_projection(RowTag::Turn, Some(13), 12, true));
    assert!(!retires_projection(RowTag::Turn, Some(13), 12, false));
}

#[test]
fn a_late_frame_may_not_bring_a_retired_projection_back() {
    assert!(!live_frame_admitted(12, 12));
    assert!(!live_frame_admitted(11, 12));
}

#[test]
fn a_later_step_is_admitted_and_an_unretired_projection_takes_any_frame() {
    assert!(live_frame_admitted(13, 12));
    assert!(live_frame_admitted(12, 0));
}
