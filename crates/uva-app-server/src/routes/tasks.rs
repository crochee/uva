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
//! The child's own log is the **only** record this build writes: the
//! parent's log keeps the ordinary `tool_call` / `tool_result` rows
//! and no `subagent_enter` mirror (a mirror is what a log from an
//! older build opens with, and discovery reads that too). Finding a
//! delegation therefore cannot work by scanning the parent's tail —
//! it is the **discovery** module's job (`uva-tool-task`'s scan,
//! over the `SessionIndex` seam): enumerate the user's newest
//! sessions, read each head, answer which children exist. This route
//! owns the description — what each discovered child's own log says
//! on the wire: dispatch prompt, closing footer, status.
//!
//! The id names the delegation by the pair that defines it — the
//! parent that delegated and the child that was delegated to — so it
//! stays the same for the delegation's whole life. A row offset
//! would not: the child's log grows as the run proceeds, so the
//! same delegation would answer to a different id on the next
//! request, and a cursor naming it would no longer find it.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uva::{
    session::{RowTag, SessionEvent, restore_row},
    tool_task::{ChildRecord, RecordSource, Scope, discover},
};

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
    let mut delegations = delegations_of_user(&state, &user_id).await;
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

/// The user's delegations: discovery answers which children exist,
/// and each child's own log describes it.
///
/// Discovery — the enumeration window, the header rule, the
/// legacy-mirror dialect — lives in `uva-tool-task`'s scan module
/// behind the `SessionIndex` seam; this route owns the wire shape. A
/// delegation whose own log cannot be read is still listed — the
/// record degrades, no prompt and no end — rather than silently
/// dropped.
pub(crate) async fn delegations_of_user(
    state: &AppState,
    user_id: &str,
) -> Vec<TaskDelegation> {
    let index = crate::session::session_index::ServerSessionIndex::new(
        Arc::clone(&state.session_manager),
    );
    let records = discover(&index, Scope::User(user_id.to_string())).await;
    let mut out = Vec::new();
    for record in &records {
        if record.source == RecordSource::OwnLog {
            // The child's own log is the complete record: the
            // header, the dispatch prompt, the closing footer.
            let sink = state.session_manager.sink(user_id, &record.id);
            let raw = sink.read().await.unwrap_or_default();
            if let Some(delegation) = delegation_of_child(&raw, &record.id) {
                out.push(delegation);
                continue;
            }
        }
        out.push(degraded_delegation(record));
    }
    out
}

/// The record a delegation falls back to when its own log cannot be
/// read — a legacy parent's mirror, or a child whose full read
/// failed: the header's facts only, no prompt and no end.
fn degraded_delegation(record: &ChildRecord) -> TaskDelegation {
    TaskDelegation {
        id: delegation_id(&record.parent_session_id, &record.id),
        parent_session_id: record.parent_session_id.clone(),
        child_session_id: Some(record.id.clone()),
        parent_depth: record.depth,
        agent: record.agent_name.clone(),
        prompt: String::new(),
        started_at: record.started_at.clone(),
        finished_at: None,
        status: "running".to_string(),
    }
}

/// Whether a row is the delegation's dispatch prompt.
///
/// The question is asked of the **restored** row, so the answer does
/// not depend on how the row was written: a prompt is a
/// [`RowTag::UserMessage`] row today and was a legacy envelope
/// before, and the row vocabulary is what says which one this row
/// is. A read site that spelled either tag would have to keep the
/// two in step — and the pair it forgot would drop the prompt.
fn is_user_prompt(row: &Value) -> bool {
    restore_row(row).restores_to(RowTag::UserMessage)
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
        let event = restore_row(row).event()?;
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
        .find(|row| is_user_prompt(row))
        .and_then(|row| {
            // The restored row projects the model message it
            // carries, and the prompt is that message's text. The
            // projection covers both shapes a prompt row comes in:
            // the provider `Message` this build writes, and the
            // `{"text": …}` payload a log from before the typed
            // writer kept.
            restore_row(row)
                .message()
                .and_then(|message| message.content.extract_text())
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
            restore_row(row).event().is_some_and(|event| {
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
    let enter_event = restore_row(enter).event();
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
        restore_row(row)
            .event()
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
