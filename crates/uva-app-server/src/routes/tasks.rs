//! `task` delegation HTTP surface — replaces the N+1 fan-out
//! the frontend's `tasksApi.listAll` was doing by scanning
//! recent sessions server-side and returning one flat list of
//! `TaskDelegation` records.
//!
//! The `task` tool is model-facing (`uva-tool-task`); since
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
//! The child's own log is the **only** record of the delegation: the
//! parent's log keeps the ordinary `tool_call` / `tool_result` rows
//! and no `subagent_enter` mirror. Finding a delegation therefore
//! cannot work by scanning the parent's tail — it works by
//! enumerating the user's sessions and reading each log's **head**,
//! because the header is the child log's first event. The
//! enumeration has to cover the sessions the manager's in-memory
//! index does not hold: a child session is opened through its
//! writer, and the registry's `writer()` does not register the
//! session, so only the on-disk directory knows it exists (and the
//! write of the header made that directory).
//!
//! The id names the delegation by the pair that defines it — the
//! parent that delegated and the child that was delegated to — so it
//! stays the same for the delegation's whole life. A row offset
//! would not: the child's log grows as the run proceeds, so the
//! same delegation would answer to a different id on the next
//! request, and a cursor naming it would no longer find it.

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uva::session::SessionEvent;

use super::{
    helpers::paginate,
    sessions::{is_ephemeral_session, unregistered_session_ids},
};
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
/// Wire shape mirrors `uva-web/src/api/types.ts::TaskDelegation`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// Cap on the fan-out: how many recent sessions one scan reads. Keeps
/// a workspace with thousands of sessions from exhausting the
/// request budget from this page.
///
/// The window is **newest first** (see [`recent_session_ids`]): a
/// cap is only safe when the rows it can drop are the ones nobody is
/// looking for, and a delegation that just started is the row this
/// page exists to show.
const TASK_SCAN_LIMIT: usize = 50;

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
        RpcError::from(uva::core::Error::session(format!("{e}")))
    })?;
    let delegation = delegation_of_child(&raw, &child_id).ok_or_else(|| {
        RpcError::from(uva::core::Error::not_found(format!(
            "no delegation to '{child_id}'"
        )))
    })?;
    // The id names one delegation, so the record it resolves to must
    // be that one: the child's own log names the parent that
    // delegated, and a request naming a different parent is asking
    // for a delegation that does not exist.
    if delegation.parent_session_id != parent_id {
        return Err(RpcError::from(uva::core::Error::not_found(format!(
            "no delegation of '{parent_id}' to '{child_id}'"
        ))));
    }
    Ok(delegation)
}

/// Walk the most recent `limit` sessions and extract every
/// delegation they record. Sessions that fail to load are silently
/// skipped — a partial result is better than nothing on this
/// read-only page.
///
/// A delegation's record is the **child's** log, and the child's log
/// opens with the header — so discovery reads each candidate's head,
/// not its tail. The candidates are the user's sessions from both
/// sources: the manager's registry, and the sessions directory (a
/// child session lives only on disk until some other surface opens
/// it).
pub(crate) async fn scan_recent_sessions(
    state: &AppState,
    user_id: &str,
    limit: usize,
) -> Vec<TaskDelegation> {
    // The delegation a header row describes, keyed by the child it
    // names. One delegation is one log — the child's own — but a log
    // written by an older build may still hold the parent's mirror,
    // and the same child would then be discovered twice.
    //
    // The header row is kept alongside the child: it is the fallback
    // record for a delegation whose own log cannot be read, so the
    // delegation is still listed rather than silently dropped.
    let mut candidates: Vec<(String, Value)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for session_id in recent_session_ids(state, user_id, limit).await {
        let sink = state.session_manager.sink(user_id, &session_id);
        // Bounded head read: the delegation header is the child
        // log's first event, so one head read answers the whole
        // question. A tail window could not — the header is the log's
        // first row, and a child log grows past any fixed window.
        let Ok(Some(head)) = sink.read_head().await else {
            continue;
        };
        let Some(child_id) = delegation_child(&head) else {
            continue;
        };
        if seen.insert(child_id.clone()) {
            candidates.push((child_id, head));
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
                let parent_id = SessionEvent::from_value(&marker)
                    .and_then(|event| {
                        event.delegation_parent().map(str::to_string)
                    })
                    .unwrap_or_default();
                to_delegation(&parent_id, &child_id, &marker, None, "")
            });
        if listed.insert(delegation.id.clone()) {
            out.push(delegation);
        }
    }
    out
}

/// The child a session's **first** event names, when that first event
/// is the delegation header.
///
/// The reader is deliberately narrow: the delegation header opens a
/// child session, so a session whose first event is not one is not a
/// delegation this route can describe — and reading any further
/// would let an ordinary session's later row masquerade as one.
fn delegation_child(head: &Value) -> Option<String> {
    let event = SessionEvent::from_value(head)?;
    if !matches!(event, SessionEvent::SubagentEnter { .. }) {
        return None;
    }
    event.delegation_child().map(str::to_string)
}

/// The user's sessions, newest first, capped at `limit`.
///
/// Two sources, because either alone misses the sessions this page
/// exists to find:
///
/// - the manager's in-memory index holds the sessions this process
///   created (`create_with_user`);
/// - the user's sessions directory holds every session the user has,
///   including delegated children, which are opened through their
///   writer and never registered.
async fn recent_session_ids(
    state: &AppState,
    user_id: &str,
    limit: usize,
) -> Vec<String> {
    let mut ids: Vec<String> = state
        .session_manager
        .list_all()
        .await
        .map(|sessions| {
            sessions
                .into_iter()
                .filter(|session| {
                    session.user_id == user_id
                        && !is_ephemeral_session(&session.id)
                })
                .map(|session| session.id)
                .collect()
        })
        .unwrap_or_default();
    let registered: HashSet<String> = ids.iter().cloned().collect();
    // The directory listing is blocking `std::fs`, so it rides the
    // blocking pool like every other disk scan on the runtime.
    let root = state.session_manager.sessions_root().to_path_buf();
    let scan_user = user_id.to_string();
    let on_disk = tokio::task::spawn_blocking(move || {
        unregistered_session_ids(&root, &scan_user, &registered)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    ids.extend(on_disk);
    ids.sort();
    ids.dedup();
    // Session ids are ULIDs: their lexicographic order is their
    // creation order. Reversing it makes the cap a **newest-first**
    // window, so the delegation that just started is never the row
    // the budget drops.
    ids.reverse();
    ids.truncate(limit);
    ids
}

/// One delegation, read from the child's own log.
///
/// The child's log opens with the `subagent_enter` header naming the
/// delegation and closes with the matching `subagent_exit` footer;
/// the dispatch prompt is the child's first `user_message` row (or the
/// legacy `UserInput` envelope an older log holds). A log
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
    let (enter, marker) = rows.iter().find_map(|row| {
        let event = SessionEvent::from_value(row)?;
        if !matches!(event, SessionEvent::SubagentEnter { .. }) {
            return None;
        }
        if event.delegation_child() != Some(child_id) {
            return None;
        }
        Some((event, row))
    })?;
    let parent_id = enter.delegation_parent()?;
    let exit = find_exit(rows, child_id);
    let prompt = rows
        .iter()
        .find(|row| {
            // The typed `user_message` this build writes, or the
            // legacy `UserInput` envelope an older log holds — the
            // same two-tag rule every prompt reader uses.
            matches!(
                row.get("type").and_then(Value::as_str),
                Some("user_message" | "UserInput")
            )
        })
        .and_then(|row| {
            // The typed row carries a provider `Message` — decode it
            // back and ask for its text. A log from before the typed
            // writer kept `{"text": …}` in the payload instead.
            row.get("data").and_then(|data| {
                serde_json::from_value::<uva::provider::Message>(data.clone())
                    .ok()
                    .and_then(|message| message.content.extract_text())
                    .or_else(|| {
                        data.get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
            })
        })
        .unwrap_or_default()
        .to_string();
    Some(to_delegation(
        parent_id,
        child_id,
        marker,
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
            SessionEvent::from_value(row).is_some_and(|event| {
                matches!(event, SessionEvent::SubagentExit { .. })
                    && event.delegation_child() == Some(child_id)
            })
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
        return Err(RpcError::from(uva::core::Error::validation(format!(
            "invalid task id '{id}': expected <parentSessionId>:<childSessionId>"
        ))));
    };
    for (half, kind) in [
        (parent_id, "parent session id"),
        (child_id, "child session id"),
    ] {
        validate_resource_name(half).map_err(|error| {
            RpcError::from(uva::core::Error::validation(format!(
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
    let enter_event = SessionEvent::from_value(enter);
    let agent = enter_event
        .as_ref()
        .and_then(|event| event.delegation_agent())
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".into());
    let depth = enter_event
        .as_ref()
        .and_then(|event| event.delegation_depth())
        .unwrap_or(0) as u32;
    let status = exit.and_then(|row| {
        SessionEvent::from_value(row)
            .and_then(|event| event.delegation_status().map(str::to_string))
    });
    TaskDelegation {
        id: delegation_id(parent_id, child_id),
        parent_session_id: parent_id.to_string(),
        child_session_id: Some(child_id.to_string()),
        parent_depth: depth,
        agent,
        prompt: prompt.to_string(),
        started_at: enter.get("ts").and_then(Value::as_str).map(str::to_string),
        finished_at: exit
            .and_then(|row| row.get("ts").and_then(Value::as_str))
            .map(str::to_string),
        status: match (&status, exit) {
            (Some(raw), _) => exit_status(Some(raw.as_str())),
            (None, Some(_)) => exit_status(None),
            (None, None) => "running".to_string(),
        },
    }
}

#[cfg(test)]
mod tests;
