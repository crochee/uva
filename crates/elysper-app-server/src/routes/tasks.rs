//! `task` delegation HTTP surface — replaces the N+1 fan-out
//! the frontend's `tasksApi.listAll` was doing by scanning
//! recent sessions server-side and returning one flat list of
//! `TaskDelegation` records.
//!
//! The `task` tool is model-facing (`elysper-tool-plugins`); since
//! delegated children run in their **own sessions** (see
//! `crate::session::subagent_router`), the delegation's record
//! lives in the child's own log: it opens with a
//! `subagent_enter` header
//! (`data: { child_session_id, parent_session_id, depth,
//! agent_name }`), carries the dispatch prompt as its `UserInput`
//! row, and closes with the matching `subagent_exit` carrying the
//! terminal status.
//!
//! Surface:
//! - `GET /api/v1/tasks`             — list delegations across recent sessions
//! - `GET /api/v1/tasks/{id}`        — fetch one delegation by `parentId:childId`
//!
//! A delegation is recorded **twice** in the workspace: the
//! delegated child's own log opens with the header described above,
//! and the root parent's log carries a mirror of the same pair so
//! the parent's transcript can show the delegation. The child's own
//! log is the one that holds the whole story — the header, the
//! dispatch prompt, and the closing footer — so it is the one this
//! route reads, and the mirror is what makes the scan find the
//! delegation in the first place.
//!
//! The id names the delegation by the pair that defines it — the
//! parent that delegated and the child that was delegated to — so
//! it stays the same for the delegation's whole life. A row offset
//! would not: the child's log grows as the run proceeds, so the
//! same delegation would answer to a different id on the next
//! request, and a cursor naming it would no longer find it.

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::helpers::paginate;
use crate::{
    api::{
        List,
        PageQuery,
        resolve_page,
        validate_resource_name,
        validate_sort,
    },
    rpc::RpcError,
    state::AppState,
};

/// Sortable fields for the tasks list endpoint.
const TASK_SORT_WHITELIST: &[&str] = &["started_at"];

/// `TaskDelegation` — one delegation, flattened for the frontend.
/// Wire shape mirrors `elysper-web/src/api/types.ts::TaskDelegation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDelegation {
    /// Synthetic id `<parentSessionId>:<childSessionId>` — survives a
    /// refresh, and names one delegation for good (see
    /// `delegation_id`).
    pub id: String,
    pub parent_session_id: String,
    pub child_session_id: Option<String>,
    pub parent_depth: u32,
    pub agent: String,
    pub prompt: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub status: String,
}

/// Cap on the fan-out: how many recent sessions to scan. Keeps a
/// workspace with thousands of sessions from exhausting the
/// request budget from this page.
const TASK_SCAN_LIMIT: usize = 50;

/// How many rows of each session's log the scan reads.
const TASK_SCAN_ROWS: usize = 256;

/// `GET /api/v1/tasks` — server-side scan.
///
/// The page is taken from what the scan found, so the cursor names
/// the delegation the next page starts after. That only works if
/// the list is **sorted** — an unsorted list makes the cursor's
/// position move between requests, so the next page repeats or
/// skips rows. The order is the delegation's start time (newest
/// first), with the id as the tiebreaker, so a page boundary never
/// lands between two rows that share a timestamp.
pub async fn list_tasks(
    state: Arc<AppState>,
    mut page: PageQuery,
) -> Result<List<TaskDelegation>, RpcError> {
    // The default sort is applied to the query itself, so the
    // resolved page carries it: a request that names no sort gets
    // the same newest-first order the cursor resumes in. Leaving it
    // to the resolved page's "no sort" case would order the list
    // ascending while the cursor still resumed from the end.
    if page.sort.is_none() {
        page.sort = Some("-started_at".to_string());
    }
    validate_sort(
        page.sort.as_deref().unwrap_or("-started_at"),
        TASK_SORT_WHITELIST,
    )?;
    let resolved = resolve_page(&page)?;
    let user_id = state.default_user_id().to_string();
    let mut delegations =
        scan_recent_sessions(&state, &user_id, TASK_SCAN_LIMIT).await;
    sort_delegations(&mut delegations, &resolved);
    Ok(paginate(
        delegations,
        &resolved,
        |delegation: &TaskDelegation| delegation.id.as_str(),
    ))
}

/// `GET /api/v1/tasks/{id}` — fetch one delegation by synthetic id.
///
/// The id names the delegation by the pair that defines it — the
/// parent that delegated and the child that was delegated to — so
/// the lookup reads the child's own log (which holds the header,
/// the prompt, and the footer) rather than a row offset that a
/// later write would move.
pub async fn get_task(
    state: Arc<AppState>,
    id: String,
) -> Result<TaskDelegation, RpcError> {
    let (parent_id, child_id) = parse_task_id(&id)?;
    let user_id = state.default_user_id().to_string();
    let sink = state.session_manager.sink(&user_id, &child_id);
    let raw = sink.read().await.map_err(|e| {
        RpcError::from(elysper::core::Error::session(format!("{e}")))
    })?;
    let delegation = delegation_of_child(&raw, &child_id).ok_or_else(|| {
        RpcError::from(elysper::core::Error::not_found(format!(
            "no delegation to '{child_id}'"
        )))
    })?;
    // The id names one delegation, so the record it resolves to must
    // be that one: the child's own log names the parent that
    // delegated, and a request naming a different parent is asking
    // for a delegation that does not exist.
    if delegation.parent_session_id != parent_id {
        return Err(RpcError::from(elysper::core::Error::not_found(format!(
            "no delegation of '{parent_id}' to '{child_id}'"
        ))));
    }
    Ok(delegation)
}

/// Walk the most recent `limit` sessions and extract every
/// delegation they record. Sessions that fail to load are silently
/// skipped — a partial result is better than nothing on this
/// read-only page.
pub(crate) async fn scan_recent_sessions(
    state: &AppState,
    user_id: &str,
    limit: usize,
) -> Vec<TaskDelegation> {
    let sessions = match state.session_manager.list_all().await {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    // The delegation a marker row describes, keyed by the child it
    // names. A delegation is recorded twice — the child's own log
    // opens with its header, and the root parent's log carries a
    // mirror — so the same child arrives from two sessions and
    // would otherwise be listed twice.
    //
    // The marker row is kept alongside the child: it is the fallback
    // record for a delegation whose own log cannot be read, so the
    // delegation is still listed rather than silently dropped.
    let mut candidates: Vec<(String, Value)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for session in sessions.into_iter().take(limit) {
        let sink = state.session_manager.sink(user_id, &session.id);
        // Bounded read: the delegation rows live at the turn
        // boundaries, so the tail window carries them without paying
        // for a whole session's history per candidate.
        let raw = match sink.read_tail(TASK_SCAN_ROWS).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        for row in &raw {
            if row.get("type").and_then(Value::as_str) != Some("subagent_enter")
            {
                continue;
            }
            let Some(child_id) = row
                .pointer("/data/child_session_id")
                .and_then(Value::as_str)
            else {
                continue;
            };
            if seen.insert(child_id.to_string()) {
                candidates.push((child_id.to_string(), row.clone()));
            }
        }
    }
    // Each delegation's own record lives in the child's log, which
    // holds the header, the dispatch prompt, and the closing footer.
    //
    // The dedup happens on the record's own id rather than on the
    // candidate: the child's log is what says who delegated, so two
    // discoveries of one delegation — the parent's mirror and the
    // child's own header — are one row only if the record agrees
    // with itself, and the record is what the id is built from.
    let mut out: Vec<TaskDelegation> = Vec::new();
    let mut listed: HashSet<String> = HashSet::new();
    for (child_id, marker) in candidates {
        let sink = state.session_manager.sink(user_id, &child_id);
        let raw = sink.read().await.unwrap_or_default();
        let delegation =
            delegation_of_child(&raw, &child_id).unwrap_or_else(|| {
                // The child's own log is the complete record, but a
                // delegation whose child log cannot be read is still
                // a delegation: the parent's mirror says so. The
                // record degrades — no prompt, no end — rather than
                // the delegation vanishing from the list.
                let parent_id = marker
                    .pointer("/data/parent_session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                to_delegation(parent_id, &child_id, &marker, None, "")
            });
        if listed.insert(delegation.id.clone()) {
            out.push(delegation);
        }
    }
    out
}

/// One delegation, read from the child's own log.
///
/// The child's log opens with the `subagent_enter` header naming the
/// delegation and closes with the matching `subagent_exit` footer;
/// the dispatch prompt is the child's first `UserInput` row. A log
/// that holds no such header describes no delegation of this parent
/// to this child.
///
/// The header is the record's own statement of who delegated, so the
/// pair it names is the delegation's id — not whatever the scan
/// happened to find the delegation under.
fn delegation_of_child(
    rows: &[Value],
    child_id: &str,
) -> Option<TaskDelegation> {
    let enter = rows.iter().find(|row| {
        row.get("type").and_then(Value::as_str) == Some("subagent_enter")
            && row
                .pointer("/data/child_session_id")
                .and_then(Value::as_str)
                == Some(child_id)
    })?;
    let parent_id = enter
        .pointer("/data/parent_session_id")
        .and_then(Value::as_str)?;
    let exit = find_exit(rows, child_id);
    let prompt = rows
        .iter()
        .find(|row| {
            row.get("type").and_then(Value::as_str) == Some("UserInput")
        })
        .and_then(|row| row.pointer("/data/text").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    Some(to_delegation(
        parent_id,
        child_id,
        enter,
        exit.as_ref(),
        &prompt,
    ))
}

/// The delegation's identity: the parent that delegated and the
/// child that was delegated to.
///
/// Both halves are fixed at delegation time — the parent's session
/// id and the child's session id — so the pair names one delegation
/// for good. A row offset would not: the child's log grows as the
/// run proceeds, so the same delegation would answer to a
/// different id on the next request, and a cursor naming it would
/// no longer find it.
fn delegation_id(parent_id: &str, child_id: &str) -> String {
    format!("{parent_id}:{child_id}")
}

/// Order the delegations by the requested field, with the id as the
/// tiebreaker.
///
/// The tiebreaker is what makes the order **total**: two
/// delegations that started in the same millisecond would otherwise
/// have no defined order between them, and the page boundary could
/// fall between them — repeating one on the next page and losing
/// the other.
fn sort_delegations(
    delegations: &mut [TaskDelegation],
    resolved: &crate::api::ResolvedPage,
) {
    delegations.sort_by(|a, b| {
        started_at(a)
            .cmp(&started_at(b))
            .then_with(|| a.id.cmp(&b.id))
    });
    if resolved.descending {
        delegations.reverse();
    }
}

/// A delegation's start instant, parsed for ordering.
///
/// The wire shape carries the timestamp as text; comparing the
/// parsed instants is what makes the order chronological rather
/// than lexicographic.
fn started_at(delegation: &TaskDelegation) -> Option<DateTime<Utc>> {
    delegation.started_at.as_deref().and_then(|raw| {
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    })
}

/// The matching `subagent_exit` row for one child, if the
/// delegation already finished.
fn find_exit(rows: &[Value], child_id: &str) -> Option<Value> {
    rows.iter()
        .find(|row| {
            row.get("type").and_then(Value::as_str) == Some("subagent_exit")
                && row
                    .pointer("/data/child_session_id")
                    .and_then(Value::as_str)
                    == Some(child_id)
        })
        .cloned()
}

/// Marker status → the wire vocabulary the frontend renders:
/// `completed` → `succeeded`, anything else keeps its own label.
fn exit_status(raw: Option<&str>) -> String {
    match raw {
        Some("completed") => "succeeded".to_string(),
        Some(other) => other.to_string(),
        None => "failed".to_string(),
    }
}

/// Split a delegation id into the parent that delegated and the
/// child that was delegated to.
///
/// The split is on the **last** colon: the parent's half may itself
/// contain one, and the child's half — a ULID — never does.
///
/// Both halves are checked as resource names because the child's
/// half reaches the filesystem: it becomes a directory name under
/// the user's sessions root, so a crafted id must not be able to
/// walk out of it.
fn parse_task_id(id: &str) -> Result<(String, String), RpcError> {
    let Some((parent_id, child_id)) = id.rsplit_once(':') else {
        return Err(RpcError::from(elysper::core::Error::validation(format!(
            "invalid task id '{id}': expected <parentSessionId>:<childSessionId>"
        ))));
    };
    for (half, kind) in [
        (parent_id, "parent session id"),
        (child_id, "child session id"),
    ] {
        validate_resource_name(half).map_err(|error| {
            RpcError::from(elysper::core::Error::validation(format!(
                "invalid {kind} in task id: {error}"
            )))
        })?;
    }
    Ok((parent_id.to_string(), child_id.to_string()))
}

/// Build one delegation from its `subagent_enter` row (and the
/// matching exit row when the delegation finished).
fn to_delegation(
    parent_id: &str,
    child_id: &str,
    enter: &Value,
    exit: Option<&Value>,
    prompt: &str,
) -> TaskDelegation {
    let str_at = |pointer: &str| {
        enter
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    TaskDelegation {
        id: delegation_id(parent_id, child_id),
        parent_session_id: parent_id.to_string(),
        child_session_id: Some(child_id.to_string()),
        parent_depth: enter
            .pointer("/data/depth")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        agent: str_at("/data/agent_name").unwrap_or_else(|| "unknown".into()),
        prompt: prompt.to_string(),
        started_at: enter.get("ts").and_then(Value::as_str).map(str::to_string),
        finished_at: exit
            .and_then(|row| row.get("ts").and_then(Value::as_str))
            .map(str::to_string),
        status: match exit {
            Some(row) => {
                exit_status(row.pointer("/data/status").and_then(Value::as_str))
            }
            None => "running".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// A hermetic `AppState` for the tasks route.
    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp workspace");
        let sessions = elysper::session::manager::SessionRegistry::new(
            dir.path().join("sessions"),
        );
        let state =
            AppState::for_test(sessions, dir.path().to_path_buf()).await;
        (Arc::new(state), dir)
    }

    /// Record one delegation the way the router does: the child's
    /// own log opens with the `subagent_enter` header and the
    /// dispatch prompt, and the root parent's log carries the mirror
    /// of the same pair.
    async fn seed_delegation(
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
        child_sink
            .append(&serde_json::json!({
                "type": "UserInput",
                "seq": 2,
                "ts": "2026-11-07T00:00:01Z",
                "data": { "text": prompt },
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
        let cursor =
            first.next_cursor.clone().expect("a cursor to resume from");

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
}
