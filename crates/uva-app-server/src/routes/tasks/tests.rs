//! `tasks` route tests: the delegation list/detail, its pagination and
//! its id, plus the two prompt dialects a child log may carry.
//!
//! The module lives beside `tasks.rs` so it stays out of the route
//! module's line budget (`make check-test-layout`).

use std::sync::Arc;

use super::*;

/// A hermetic `AppState` for the tasks route.
async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state = AppState::for_test(sessions, dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// Record one delegation the way the router does: the child's
/// own log opens with the `subagent_enter` header and the
/// dispatch prompt, and the root parent's log carries the mirror
/// an older build wrote.
async fn seed_delegation(
    state: &AppState,
    user: &str,
    parent: &str,
    child: &str,
    prompt: &str,
    finished: bool,
) {
    seed_delegation_dialect(
        state, user, parent, child, prompt, finished, false,
    )
    .await;
}

/// Record one delegation the way **this** build does: the child's
/// own log opens with the header and the prompt, and the parent's
/// log holds no delegation row at all.
///
/// The child session is written through its sink, exactly as the
/// sub-agent router opens it — and that path does not register the
/// session with the manager, so the child exists only on disk. This
/// is the shape discovery must handle.
async fn seed_delegation_without_mirror(
    state: &AppState,
    user: &str,
    parent: &str,
    child: &str,
    prompt: &str,
    finished: bool,
) {
    state
        .session_manager
        .create_with_user(parent.to_string(), user.to_string())
        .await
        .expect("the parent session exists");
    let child_sink = state.session_manager.sink(user, child);
    child_sink
        .append(&serde_json::json!({
            "type": "subagent_enter",
            "seq": 1,
            "ts": "2026-11-07T00:00:00Z",
            "data": {
                "child_session_id": child,
                "parent_session_id": parent,
                "depth": 1,
                "agent_name": "coder",
            },
        }))
        .await
        .expect("the child's header is appended");
    let prompt_message =
        serde_json::to_value(uva::provider::Message::user(prompt))
            .expect("the prompt message serializes");
    child_sink
        .append(&serde_json::json!({
            "type": "user_message",
            "seq": 2,
            "ts": "2026-11-07T00:00:01Z",
            "data": prompt_message,
        }))
        .await
        .expect("the child's prompt is appended");
    if finished {
        child_sink
            .append(&serde_json::json!({
                "type": "subagent_exit",
                "seq": 3,
                "ts": "2026-11-07T00:00:02Z",
                "data": { "child_session_id": child, "depth": 1, "status": "completed" },
            }))
            .await
            .expect("the child's footer is appended");
    }
}

/// [`seed_delegation`] in either prompt dialect: the typed
/// `user_message` this build writes, or the legacy `UserInput`
/// envelope an older log holds.
async fn seed_delegation_dialect(
    state: &AppState,
    user: &str,
    parent: &str,
    child: &str,
    prompt: &str,
    finished: bool,
    legacy_prompt: bool,
) {
    state
        .session_manager
        .create_with_user(parent.to_string(), user.to_string())
        .await
        .expect("the parent session exists");
    let parent_sink = state.session_manager.sink(user, parent);
    let header = serde_json::json!({
        "type": "subagent_enter",
        "seq": 1,
        "ts": "2026-11-07T00:00:00Z",
        "data": {
            "child_session_id": child,
            "parent_session_id": parent,
            "depth": 1,
            "agent_name": "coder",
        },
    });
    parent_sink
        .append(&header)
        .await
        .expect("the parent's mirror row is appended");

    let child_sink = state.session_manager.sink(user, child);
    child_sink
        .append(&header)
        .await
        .expect("the child's header is appended");
    let prompt_row = if legacy_prompt {
        // A log written before the typed writer kept
        // `{"text": …}` in a `UserInput` row's payload.
        serde_json::json!({
            "type": "UserInput",
            "seq": 2,
            "ts": "2026-11-07T00:00:01Z",
            "data": {"text": prompt},
        })
    } else {
        let prompt_message =
            serde_json::to_value(uva::provider::Message::user(prompt))
                .expect("the prompt message serializes");
        serde_json::json!({
            "type": "user_message",
            "seq": 2,
            "ts": "2026-11-07T00:00:01Z",
            "data": prompt_message,
        })
    };
    child_sink
        .append(&prompt_row)
        .await
        .expect("the child's prompt is appended");
    if finished {
        child_sink
            .append(&serde_json::json!({
                "type": "subagent_exit",
                "seq": 3,
                "ts": "2026-11-07T00:00:02Z",
                "data": { "child_session_id": child, "depth": 1, "status": "completed" },
            }))
            .await
            .expect("the child's footer is appended");
    }
}

/// The list is paginated: a page asks for what it shows, and a
/// page with more to show says so.
///
/// The route used to scan and return every delegation it found —
/// no cursor, no limit — so a busy workspace answered with a list
/// as long as the scan.
#[tokio::test]
async fn the_task_list_is_paginated() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation(&state, &user, "s1", "c1", "first", true).await;
    seed_delegation(&state, &user, "s2", "c2", "second", true).await;
    seed_delegation(&state, &user, "s3", "c3", "third", true).await;

    let page = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(2),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the list");

    assert_eq!(page.data.len(), 2, "the page holds the asked-for rows");
    assert!(
        page.next_cursor.is_some(),
        "a page with more to show says so: {page:?}"
    );
    assert_eq!(page.total, Some(3), "the total names the whole scan");
}

/// The cursor names a delegation, not a row offset, so the next
/// page resumes where the first stopped.
///
/// The id used to embed the row's position in the scan's tail
/// window. That position moves as the child's log grows — the
/// delegation that answered to `parent:3` answered to
/// `parent:4` after one more row landed — so the second page
/// found no row with the cursor's id and came back empty.
#[tokio::test]
async fn the_cursor_resumes_after_the_first_page() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation(&state, &user, "s1", "c1", "first", true).await;
    seed_delegation(&state, &user, "s2", "c2", "second", true).await;
    seed_delegation(&state, &user, "s3", "c3", "third", true).await;

    let first = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(2),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the first page");
    let cursor = first.next_cursor.clone().expect("a cursor to resume from");

    // The child's log grows — the delegation's own transcript
    // gains a row — and the id must not move with it.
    let child_sink = state.session_manager.sink(&user, "c3");
    child_sink
        .append(&serde_json::json!({
            "type": "assistant_message",
            "seq": 4,
            "ts": "2026-11-07T00:00:03Z",
            "data": { "role": "assistant" },
        }))
        .await
        .expect("the child log grows");

    let second = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(2),
            cursor: Some(cursor),
            sort: None,
        },
    )
    .await
    .expect("the second page");

    assert_eq!(
        second.data.len(),
        1,
        "the second page holds what the first left: {second:?}"
    );
    assert!(
        second.next_cursor.is_none(),
        "the last page says so: {second:?}"
    );
}

/// A delegation is discovered from the child's own log alone.
///
/// This build writes no mirror into the parent's log, and the child
/// session is never registered with the manager — it is opened
/// through its writer, so the in-memory index does not hold it — and
/// the delegation header sits at the log's **head**. A scan over the
/// registry's tails therefore finds nothing; the route has to
/// enumerate the user's sessions on disk and read each log's head.
#[tokio::test]
async fn a_delegation_is_found_without_a_parent_mirror() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation_without_mirror(
        &state,
        &user,
        "p1",
        "c1",
        "child only",
        true,
    )
    .await;

    let page = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(50),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the list");

    assert_eq!(
        page.data.len(),
        1,
        "the child's own log is the record: {page:?}"
    );
    assert_eq!(page.data[0].parent_session_id, "p1");
    assert_eq!(page.data[0].child_session_id.as_deref(), Some("c1"));
    assert_eq!(page.data[0].prompt, "child only");
    assert_eq!(page.data[0].status, "succeeded");
}

/// One delegation is listed once, however many logs record it.
///
/// The delegation lives in the child's log and is mirrored in the
/// root parent's log. The scan reads both, so without the
/// dedup the same delegation was listed twice — once per log that
/// mentions it.
#[tokio::test]
async fn one_delegation_is_listed_once() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation(&state, &user, "s1", "c1", "only", true).await;

    let page = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(50),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the list");

    assert_eq!(
        page.data.len(),
        1,
        "the delegation is listed once: {page:?}"
    );
    assert_eq!(
        page.data[0].id, "s1:c1",
        "the id names the delegation by its pair"
    );
}

/// The id the list hands out is the id the detail route
/// resolves.
#[tokio::test]
async fn the_listed_id_resolves_to_its_delegation() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation(&state, &user, "s1", "c1", "the prompt", true).await;

    let page = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(50),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the list");
    let id = page.data[0].id.clone();

    let one = get_task(Arc::clone(&state), id)
        .await
        .expect("the delegation");

    assert_eq!(one.id, page.data[0].id, "the same delegation");
    assert_eq!(one.prompt, "the prompt", "the prompt comes with it");
    assert_eq!(one.status, "succeeded", "and so does the status");
}

/// A child log written before the typed writer keeps its dispatch
/// prompt in a legacy `UserInput` row. The reader carries a
/// `{"text": …}` fallback for exactly that row, but the search used
/// to match `user_message` only — so the fallback was unreachable
/// and every old delegation answered `prompt: ""`.
#[tokio::test]
async fn a_legacy_prompt_row_still_yields_the_prompt() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation_dialect(
        &state,
        &user,
        "s1",
        "c1",
        "legacy prompt",
        true,
        true,
    )
    .await;

    let page = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(50),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the list");

    assert_eq!(page.data.len(), 1, "the delegation is listed");
    assert_eq!(
        page.data[0].prompt, "legacy prompt",
        "the legacy dialect still names the prompt"
    );
}

/// A delegation that has not finished yet says so.
#[tokio::test]
async fn an_unfinished_delegation_is_running() {
    let (state, _dir) = test_state().await;
    let user = state.default_user_id().to_string();
    seed_delegation(&state, &user, "s1", "c1", "in flight", false).await;

    let page = list_tasks(
        Arc::clone(&state),
        PageQuery {
            limit: Some(50),
            cursor: None,
            sort: None,
        },
    )
    .await
    .expect("the list");

    assert_eq!(page.data[0].status, "running", "no footer, no end");
}
