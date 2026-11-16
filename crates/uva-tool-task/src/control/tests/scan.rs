//! Reading children out of the sessions' logs: the durable side of the
//! roster.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use uva_session::{SessionError, SessionLog, subagent_enter, subagent_exit};

use super::super::scan::{ChildRecord, SessionIndex, discover};

/// A session index over rows the test wrote.
#[derive(Default)]
pub struct FakeIndex {
    rows: Mutex<HashMap<String, Vec<Value>>>,
    writers: Mutex<HashMap<String, Arc<dyn SessionLog>>>,
    dirs: Mutex<HashMap<String, PathBuf>>,
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

    let found = discover(&index, "parent").await;
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0],
        ChildRecord {
            id: "child-1".to_string(),
            parent_session_id: "parent".to_string(),
            agent_name: "scout".to_string(),
            depth: 1,
            finished: false,
        }
    );
}

#[tokio::test]
async fn a_session_that_is_not_a_child_of_this_session_is_ignored() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, false);
    index.with_child("child-2", "another-parent", "scout", 1, false);

    let found = discover(&index, "parent").await;
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

    let found = discover(&index, "parent").await;
    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn a_child_whose_log_ends_with_the_footer_is_finished() {
    let index = FakeIndex::new();
    index.with_child("child-1", "parent", "scout", 1, true);
    index.with_child("child-2", "parent", "scout", 1, false);

    let found = discover(&index, "parent").await;
    let finished: Vec<&ChildRecord> =
        found.iter().filter(|record| record.finished).collect();
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

    let found = discover(&index, "parent").await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].id, "01J8NEWCHILD");
    assert!(found[0].finished);
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

    let found = discover(&index, "parent").await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].depth, 2);
}
