//! Reading the children out of the sessions' logs.
//!
//! The sessions are the **durable** record of a delegation: a child
//! opens its session with the `subagent_enter` header that names its
//! parent, so reading the headers answers "which children does this
//! session have?" — including the ones a previous process dispatched
//! and this one never held in memory. The reading is what hydrates
//! the handle table ([`ControlRegistry::adopt`]) and what a cold wake
//! resolves its peer, depth and writer through.
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

/// One delegation, read from a child's own log.
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
    /// The child's log carries the `subagent_exit` footer: its round
    /// ended.
    pub finished: bool,
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

/// The children of `parent_session_id`, read from the sessions' logs.
///
/// A session that cannot be read is skipped: a partial roster beats
/// no roster on a read-only surface.
///
/// The scan window is **newest first**. The index answers with every
/// session the deployment has, and the cap exists to bound the read;
/// taking the sorted-from-the-start window would read the oldest
/// sessions and never reach a child dispatched just before a
/// restart, which is exactly the child the roster is missing. The
/// sort lives here, not in the index, so any host's index gets the
/// same window regardless of the order it returns.
pub async fn discover(
    index: &dyn SessionIndex,
    parent_session_id: &str,
) -> Vec<ChildRecord> {
    let mut ids = index.session_ids().await;
    // Newest first: a ULID's lexicographic order is its creation
    // order, so the reversed order is the recent end of the list.
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let mut found = Vec::new();
    for id in ids.iter().take(SCAN_LIMIT) {
        let Ok(head) = index.read_head(id, HEAD_ROWS).await else {
            continue;
        };
        let Some(record) = child_record(&head, id) else {
            continue;
        };
        if record.parent_session_id != parent_session_id {
            continue;
        }
        let finished = ended(index, id).await;
        found.push(ChildRecord { finished, ..record });
    }
    found
}

/// The delegation a session's header describes, if it is one.
fn child_record(head: &[Value], session_id: &str) -> Option<ChildRecord> {
    for row in head {
        let Some(event) = SessionEvent::from_value(row) else {
            continue;
        };
        if !matches!(event, SessionEvent::SubagentEnter { .. }) {
            continue;
        }
        let parent = event.delegation_parent()?.to_string();
        let child = event.delegation_child().unwrap_or(session_id);
        // The header names the child it opens; a log whose header
        // names another child is not this session's own record.
        if child != session_id {
            continue;
        }
        return Some(ChildRecord {
            id: child.to_string(),
            parent_session_id: parent,
            agent_name: event
                .delegation_agent()
                .unwrap_or("unknown")
                .to_string(),
            depth: u32::try_from(event.delegation_depth().unwrap_or(1))
                .unwrap_or(1),
            finished: false,
        });
    }
    None
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
