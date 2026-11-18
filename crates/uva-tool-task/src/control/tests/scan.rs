//! Reading children out of the sessions' logs: the durable side of the
//! roster.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use uva_session::{SessionError, SessionLog, subagent_enter, subagent_exit};

use super::super::scan::{
    ChildRecord,
    RecordSource,
    Scope,
    SessionIndex,
    discover,
};

/// A session index over rows the test wrote.
#[derive(Default)]
pub struct FakeIndex {
    rows: Mutex<HashMap<String, Vec<Value>>>,
    writers: Mutex<HashMap<String, Arc<dyn SessionLog>>>,
    dirs: Mutex<HashMap<String, PathBuf>>,
    owners: Mutex<HashMap<String, String>>,
}

impl FakeIndex {
    /// An index with no sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// File a session's rows.
    pub fn with_session(&self, id: &str, rows: Vec<Value>) -> &Self {
        self.rows.lock().expect("lock").insert(id.to_string(), rows);
        self
    }

    /// File a session's writer.
    pub fn with_writer(&self, id: &str, writer: Arc<dyn SessionLog>) -> &Self {
        self.writers
            .lock()
            .expect("lock")
            .insert(id.to_string(), writer);
        self
    }

    /// File a session's on-disk directory.
    pub fn with_session_dir(&self, id: &str, dir: PathBuf) -> &Self {
        self.dirs.lock().expect("lock").insert(id.to_string(), dir);
        self
    }

    /// File a session's owner. A session with no owner on file
    /// belongs to no user, so a user-scoped enumeration skips it.
    pub fn with_owner(&self, id: &str, user: &str) -> &Self {
        self.owners
            .lock()
            .expect("lock")
            .insert(id.to_string(), user.to_string());
        self
    }

    /// File a child session: the delegation header, and — when the
    /// delegation ended — the footer.
    pub fn with_child(
        &self,
        child: &str,
        parent: &str,
        agent: &str,
        depth: u32,
        finished: bool,
    ) -> &Self {
        let mut rows = vec![
            serde_json::to_value(subagent_enter(child, parent, depth, agent))
                .expect("the header serialises"),
        ];
        if finished {
            rows.push(
                serde_json::to_value(subagent_exit(child, depth, "completed"))
                    .expect("the footer serialises"),
            );
        }
        self.with_session(child, rows)
    }
}

#[async_trait::async_trait]
impl SessionIndex for FakeIndex {
    async fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> =
            self.rows.lock().expect("lock").keys().cloned().collect();
        ids.sort();
        ids
    }

    async fn session_ids_of_user(&self, user_id: &str) -> Vec<String> {
        let rows = self.rows.lock().expect("lock");
        let owners = self.owners.lock().expect("lock");
        let mut ids: Vec<String> = rows
            .keys()
            .filter(|id| owners.get(*id).is_some_and(|owner| owner == user_id))
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    async fn read_head(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let rows = self.rows.lock().expect("lock");
        Ok(rows
            .get(id)
            .map(|rows| rows.iter().take(limit).cloned().collect())
            .unwrap_or_default())
    }

    async fn read_tail(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let rows = self.rows.lock().expect("lock");
        Ok(rows
            .get(id)
            .map(|rows| {
                let skip = rows.len().saturating_sub(limit);
                rows.iter().skip(skip).cloned().collect()
            })
            .unwrap_or_default())
    }

    fn writer(&self, id: &str) -> Option<Arc<dyn SessionLog>> {
        self.writers.lock().expect("lock").get(id).cloned()
    }

    fn session_dir(&self, id: &str) -> Option<PathBuf> {
        self.dirs.lock().expect("lock").get(id).cloned()
    }
}

#[tokio::test]
async fn a_child_session_is_found_by_the_header_it_opens_with() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, false);

    let found = discover(&index, Scope::Parent("parent".into())).await;
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0],
        ChildRecord {
            id: "child-1".to_string(),
            parent_session_id: "parent".to_string(),
            agent_name: "scout".to_string(),
            depth: 1,
            started_at: None,
            finished: Some(false),
            source: RecordSource::OwnLog,
        }
    );
}

#[tokio::test]
async fn a_session_that_is_not_a_child_of_this_session_is_ignored() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, false);
    index.with_child("child-2", "another-parent", "scout", 1, false);

    let found = discover(&index, Scope::Parent("parent".into())).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "child-1");
}

#[tokio::test]
async fn a_session_that_is_no_delegation_at_all_is_ignored() {
    let index = FakeIndex::new();
    index.with_session(
        "plain",
        vec![serde_json::json!({"type": "user_message", "data": {}})],
    );

    let found = discover(&index, Scope::Parent("parent".into())).await;
    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn a_child_whose_log_ends_with_the_footer_is_finished() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, true);
    index.with_child("child-2", "parent", "scout", 1, false);

    let found = discover(&index, Scope::Parent("parent".into())).await;
    let finished: Vec<&ChildRecord> = found
        .iter()
        .filter(|record| record.finished.is_some_and(|ended| ended))
        .collect();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].id, "child-1");
}

#[tokio::test]
async fn the_newest_child_is_found_past_the_oldest_sessions() {
    let index = FakeIndex::new();
    // Fill the scan window with sessions older than the child, so an
    // ascending window would cover all of them and none of the child.
    for ordinal in 0..(super::super::scan::SCAN_LIMIT + 5) {
        index.with_session(
            &format!("00J8OLD{ordinal:04}"),
            vec![serde_json::json!({
                "type": "user_message",
                "data": {},
            })],
        );
    }
    // The child a previous process dispatched is the newest session:
    // it must be inside the window, not behind the older ones.
    index.with_child("01J8NEWCHILD", "parent", "scout", 1, true);

    let found = discover(&index, Scope::Parent("parent".into())).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].id, "01J8NEWCHILD");
    assert_eq!(found[0].finished, Some(true));
}

#[tokio::test]
async fn the_header_survives_a_metadata_row_in_front_of_it() {
    let index = FakeIndex::new();
    let mut rows = vec![serde_json::json!({"_meta": {"kind": "session"}} )];
    rows.push(
        serde_json::to_value(subagent_enter("child-1", "parent", 2, "scout"))
            .expect("the header serialises"),
    );
    index.with_session("child-1", rows);

    let found = discover(&index, Scope::Parent("parent".into())).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].depth, 2);
}

#[tokio::test]
async fn a_legacy_mirror_and_the_own_log_are_one_delegation() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, false);
    index.with_owner("child-1", "alice");
    index.with_owner("parent", "alice");
    // The parent's log opens with the mirror an older build wrote,
    // naming the same child: one delegation, discovered twice.
    index.with_session(
        "parent",
        vec![
            serde_json::to_value(subagent_enter(
                "child-1", "parent", 1, "scout",
            ))
            .expect("the mirror serialises"),
        ],
    );

    let found = discover(&index, Scope::User("alice".into())).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].id, "child-1");
    assert_eq!(found[0].source, RecordSource::OwnLog);
    // The listing page derives its status from the full log it reads
    // anyway, so a user-scoped discovery spends no tail probe.
    assert_eq!(found[0].finished, None);
}

#[tokio::test]
async fn a_mirror_without_the_child_log_is_a_degraded_record() {
    let index = FakeIndex::new();
    index.with_owner("parent", "alice");
    index.with_session(
        "parent",
        vec![
            serde_json::to_value(subagent_enter(
                "child-1", "parent", 1, "scout",
            ))
            .expect("the mirror serialises"),
        ],
    );

    let found = discover(&index, Scope::User("alice".into())).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].source, RecordSource::ParentMirror);
    assert_eq!(found[0].parent_session_id, "parent");
    assert_eq!(found[0].finished, None, "a mirror cannot see the tail");
}

#[tokio::test]
async fn a_user_scoped_discovery_reads_only_that_users_sessions() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, false);
    index.with_owner("child-1", "alice");
    index.with_child("child-2", "parent", "scout", 1, false);
    index.with_owner("child-2", "bob");

    let found = discover(&index, Scope::User("alice".into())).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].id, "child-1");
}

#[tokio::test]
async fn a_parent_scoped_discovery_surfaces_mirrors_for_the_consumer_to_refuse()
{
    // The roster adopts and wakes own-log children only; the mirror
    // still surfaces, so that refusal is a decision the consumer
    // makes, not a fact the scan silently dropped.
    let index = FakeIndex::new();
    index.with_session(
        "parent",
        vec![
            serde_json::to_value(subagent_enter(
                "child-1", "parent", 1, "scout",
            ))
            .expect("the mirror serialises"),
        ],
    );

    let found = discover(&index, Scope::Parent("parent".into())).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].source, RecordSource::ParentMirror);
}
