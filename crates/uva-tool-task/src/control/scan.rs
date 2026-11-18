//! Reading the delegations out of the sessions' logs.
//!
//! The sessions are the **durable** record of a delegation: a child
//! opens its session with the `subagent_enter` header that names its
//! parent, so reading the heads answers "which delegations does this
//! deployment have?" — including the ones a previous process
//! dispatched and this one never held in memory. The reading is what
//! hydrates the handle table ([`ControlRegistry::adopt`]) and what a
//! cold wake resolves its peer, depth and writer through; the listing
//! page and the attribution callers ask the same module, so the
//! enumeration window, the header rule and the legacy-mirror dialect
//! each have exactly one answer here.
//!
//! The host owns the sessions, so the host owns the access: the
//! [`SessionIndex`] seam says which sessions exist, how to read them,
//! where a session's directory is (the artefact set's home) and which
//! writer continues one. The reading itself lives here.
//!
//! [`ControlRegistry::adopt`]: super::registry::ControlRegistry::adopt

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;
use uva_session::{SessionError, SessionEvent, SessionLog};

/// How many of the **newest** sessions one scan reads.
///
/// The window is a budget, and a budget only holds if what it drops
/// is what nobody is looking for. Session ids are ULIDs, so their
/// lexicographic order is their creation order; [`discover`] sorts
/// newest first before applying the cap, which keeps a child one
/// previous process dispatched — the one the roster is missing — at
/// the front of the window instead of behind the oldest sessions.
pub const SCAN_LIMIT: usize = 50;

/// How many rows of a session's head one scan reads.
///
/// The delegation header is one of the first rows of a child session
/// — the log's own metadata row may sit in front of it — so the scan
/// reads a small window rather than the first row alone.
pub const HEAD_ROWS: usize = 4;

/// How many rows of a session's tail one scan reads.
///
/// The `subagent_exit` footer is the last row of a finished child's
/// log, so a small tail window sees it exactly.
pub const TAIL_ROWS: usize = 4;

/// Where a discovered delegation was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordSource {
    /// The child's own log opens with the header naming itself.
    OwnLog,
    /// A legacy parent's log opens with a mirror naming the child.
    /// The record is degraded: no finish, and nothing to adopt or
    /// wake — those need the child's own log, which could not be
    /// read. Mirrors are read compatibility for logs an older build
    /// wrote; this build writes the header into the child's log only.
    ParentMirror,
}

/// One delegation, read from the sessions' logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildRecord {
    /// The child's session id — the handle the model names.
    pub id: String,
    /// The session that delegated to this child.
    pub parent_session_id: String,
    /// The peer agent the child runs as.
    pub agent_name: String,
    /// How deep the delegation is.
    pub depth: u32,
    /// The header row's own timestamp, when it carries one.
    ///
    /// Ordering (the listing page sorts by start time) is a
    /// discovery fact; the description that follows supplies the
    /// rest.
    pub started_at: Option<String>,
    /// Whether the child's log carries the `subagent_exit` footer.
    ///
    /// `None` is "not probed, or not knowable": a listing page
    /// derives its status from the full log it reads anyway, and a
    /// mirror record cannot see the child's tail at all.
    pub finished: Option<bool>,
    /// Where the record was read from.
    pub source: RecordSource,
}

/// Which delegations one discovery answers for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The children of one parent — the roster's question. Every
    /// session the host knows is enumerated and the parent's own are
    /// kept: a parent's children cannot belong to another user, and
    /// the newest-first window is the deployment's, not one user's.
    Parent(String),
    /// Every delegation of one user — the listing page's question.
    /// Only the user's sessions are enumerated.
    User(String),
}

/// The host's access to the sessions a delegation touched.
///
/// A host that keeps sessions in a database, in a remote store or in
/// the filesystem implements this trait; the crate never assumes a
/// layout.
#[async_trait]
pub trait SessionIndex: Send + Sync {
    /// The ids of the sessions the host knows.
    async fn session_ids(&self) -> Vec<String>;

    /// The ids of the sessions the host knows for `user_id`.
    ///
    /// The user-scoped half of [`session_ids`](Self::session_ids): a
    /// listing page answers for one user, and a host that keeps
    /// sessions per user reads one directory instead of every
    /// user's. Ephemeral sessions are excluded, and a `user_id` that
    /// is not one path component answers with the registered half
    /// only — a hand-built id must not climb out of the host's
    /// sessions root.
    async fn session_ids_of_user(&self, user_id: &str) -> Vec<String>;

    /// The first `limit` rows of session `id`.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the session cannot be read.
    async fn read_head(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError>;

    /// The last `limit` rows of session `id`.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the session cannot be read.
    async fn read_tail(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError>;

    /// The writer of session `id`, when the session already exists.
    ///
    /// A cold wake writes the next round into the child's own
    /// session, so it needs the session's writer — not a new one.
    fn writer(&self, id: &str) -> Option<Arc<dyn SessionLog>>;

    /// The directory session `id` lives in, when the host keeps its
    /// sessions on a filesystem.
    ///
    /// It is where the child's artefact set lands
    /// (`artifacts/<id>.{md,jsonl,patch}`), so a host that stores
    /// sessions elsewhere returns `None` and the surface skips the
    /// artefacts instead of inventing a location.
    fn session_dir(&self, _id: &str) -> Option<PathBuf> {
        None
    }
}

/// The delegations the `scope` asks for, read from the sessions'
/// logs.
///
/// A session that cannot be read is skipped: a partial answer beats
/// no answer on a read-only surface.
///
/// The scan window is **newest first**. The cap exists to bound the
/// read; taking the sorted-from-the-start window would read the
/// oldest sessions and never reach a child dispatched just before a
/// restart, which is exactly the child the roster is missing. The
/// sort lives here, not in the index, so any host's index gets the
/// same window regardless of the order it returns.
///
/// One child may be named by two heads — its own header and a legacy
/// parent's mirror. The own-log record wins; the mirror surfaces only
/// when the child's own log was never found.
pub async fn discover(
    index: &dyn SessionIndex,
    scope: Scope,
) -> Vec<ChildRecord> {
    let mut ids = match &scope {
        Scope::Parent(_) => index.session_ids().await,
        Scope::User(user) => index.session_ids_of_user(user).await,
    };
    // Newest first: a ULID's lexicographic order is its creation
    // order, so the reversed order is the recent end of the list.
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let mut found: Vec<ChildRecord> = Vec::new();
    for id in ids.iter().take(SCAN_LIMIT) {
        let Ok(head) = index.read_head(id, HEAD_ROWS).await else {
            continue;
        };
        for record in head_records(&head, id) {
            if !scope_keeps(&scope, &record) {
                continue;
            }
            let record = match (&scope, record.source) {
                // The roster reads the tail window for the footer; a
                // listing page reads the child's full log anyway, so
                // the probe would be a read it never uses — and a
                // mirror cannot see the child's tail at all.
                (Scope::Parent(_), RecordSource::OwnLog) => {
                    let finished = ended(index, &record.id).await;
                    ChildRecord {
                        finished: Some(finished),
                        ..record
                    }
                }
                _ => record,
            };
            keep_newest(&mut found, record);
        }
    }
    found
}

/// Whether `record` is one of the `scope`'s delegations.
fn scope_keeps(scope: &Scope, record: &ChildRecord) -> bool {
    match scope {
        Scope::Parent(parent) => record.parent_session_id == *parent,
        Scope::User(_) => true,
    }
}

/// File `record`, resolving a double discovery of one child: the
/// own-log record is the child's own statement, so it replaces a
/// mirror; two records of the same source are one discovery too many,
/// and the first (the newest session's) is kept.
fn keep_newest(found: &mut Vec<ChildRecord>, record: ChildRecord) {
    if let Some(existing) =
        found.iter_mut().find(|existing| existing.id == record.id)
    {
        if existing.source == RecordSource::ParentMirror
            && record.source == RecordSource::OwnLog
        {
            *existing = record;
        }
        return;
    }
    found.push(record);
}

/// The delegation facts a session's head rows state.
///
/// The `subagent_enter` rows within the head window decide: one
/// naming the session itself makes it a child with its own record;
/// one naming another child makes the session a legacy parent
/// holding the mirror an older build wrote at the head of its log.
/// The window — not the first row — is the rule: the log's own
/// metadata row may sit in front of a header, and an ordinary
/// session's rows contain no `subagent_enter` to masquerade as one.
///
/// Attribution callers (session listings, `follow`) ask the same
/// question over their own head slice — pass a window of
/// [`HEAD_ROWS`] rows, not a whole log.
pub fn head_records(head: &[Value], session_id: &str) -> Vec<ChildRecord> {
    let mut records = Vec::new();
    for row in head {
        let Some(event) = SessionEvent::from_value(row) else {
            continue;
        };
        if !matches!(event, SessionEvent::SubagentEnter { .. }) {
            continue;
        }
        let Some(parent) = event.delegation_parent() else {
            continue;
        };
        let child = event.delegation_child().unwrap_or(session_id);
        let started_at = row
            .get("ts")
            .and_then(Value::as_str)
            .filter(|ts| !ts.is_empty())
            .map(str::to_string);
        records.push(ChildRecord {
            id: child.to_string(),
            parent_session_id: parent.to_string(),
            agent_name: event
                .delegation_agent()
                .unwrap_or("unknown")
                .to_string(),
            depth: u32::try_from(event.delegation_depth().unwrap_or(1))
                .unwrap_or(1),
            started_at,
            finished: None,
            source: if child == session_id {
                RecordSource::OwnLog
            } else {
                RecordSource::ParentMirror
            },
        });
    }
    records
}

/// Whether a child's log carries the `subagent_exit` footer.
async fn ended(index: &dyn SessionIndex, session_id: &str) -> bool {
    let Ok(tail) = index.read_tail(session_id, TAIL_ROWS).await else {
        return false;
    };
    tail.iter().any(|row| {
        SessionEvent::from_value(row).is_some_and(|event| {
            matches!(event, SessionEvent::SubagentExit { .. })
        })
    })
}
