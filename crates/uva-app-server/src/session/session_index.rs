//! The control surface's view of the sessions: the server's own
//! registry, read the way the control surface needs it.
//!
//! The control surface asks two things of a session: what its first
//! rows say (the delegation header a child session opens with) and
//! what its last rows say (the `subagent_exit` footer that says the
//! round ended). It also needs the writer of a session that already
//! exists, so a woken child continues in its own log.
//!
//! The server owns the sessions, so the server owns this seam: the
//! control surface never assumes a filesystem, and the server never
//! leaks its storage into the tool.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;
use uva::{
    session::{
        SessionError,
        SessionLog,
        manager::{SERVER_DEFAULT_USER_ID, SessionRegistry},
    },
    tool_task::SessionIndex,
};

use super::EPHEMERAL_SESSION_PREFIX;
use crate::routes::sessions::is_path_component;

/// The server's session index: the sessions this deployment can read.
pub struct ServerSessionIndex {
    registry: Arc<SessionRegistry>,
}

impl ServerSessionIndex {
    /// The index over `registry`.
    #[must_use]
    pub fn new(registry: Arc<SessionRegistry>) -> Self {
        Self { registry }
    }

    /// The user a session belongs to.
    ///
    /// A session this process has not registered since it restarted
    /// has no owner in the registry's index; its directory says who
    /// owns it.
    fn user_of(&self, session_id: &str) -> String {
        if let Some(user) = self.registry.user_of(session_id) {
            return user;
        }
        let root = self.registry.sessions_root();
        let Ok(users) = std::fs::read_dir(root) else {
            return SERVER_DEFAULT_USER_ID.to_string();
        };
        for user in users.flatten() {
            if user.path().join(session_id).is_dir() {
                return user.file_name().to_string_lossy().to_string();
            }
        }
        SERVER_DEFAULT_USER_ID.to_string()
    }
}

/// The session ids one directory holds, ephemeral sessions excluded.
///
/// The directory listing is blocking `std::fs`, so the caller rides
/// the blocking pool; every enumeration in this module funnels
/// through here so the exclusion is one rule.
fn session_dir_ids(dir: &std::path::Path) -> Vec<String> {
    let Ok(sessions) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    sessions
        .flatten()
        .map(|session| session.file_name().to_string_lossy().to_string())
        .filter(|id| !id.starts_with(EPHEMERAL_SESSION_PREFIX))
        .collect()
}

#[async_trait]
impl SessionIndex for ServerSessionIndex {
    async fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .registry
            .list_all()
            .await
            .map(|sessions| {
                sessions.into_iter().map(|session| session.id).collect()
            })
            .unwrap_or_default();
        // The sessions this process has not opened since it restarted
        // are only in the directory: the registry's index starts
        // empty, and a delegation's child session is exactly the kind
        // of session a restarted process has to rediscover. Every
        // user's directory is read — a session's owner is the
        // directory it sits in.
        let root = self.registry.sessions_root().to_path_buf();
        let scanned = tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            let Ok(users) = std::fs::read_dir(&root) else {
                return found;
            };
            for user in users.flatten() {
                found.extend(session_dir_ids(&user.path()));
            }
            found
        })
        .await
        .unwrap_or_default();
        ids.extend(scanned);
        ids.sort();
        ids.dedup();
        ids
    }

    async fn session_ids_of_user(&self, user_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .registry
            .list_all()
            .await
            .map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|session| {
                        session.user_id == user_id
                            && !session.id.starts_with(EPHEMERAL_SESSION_PREFIX)
                    })
                    .map(|session| session.id)
                    .collect()
            })
            .unwrap_or_default();
        // The user-scoped half of `session_ids`: the registry holds
        // what this process opened for the user, the user's one
        // directory holds what any process ever wrote — a delegated
        // child is exactly the kind of session only the directory
        // knows. A hand-built `user_id` must not climb out of the
        // sessions root, so it is held to one path component.
        if !is_path_component(user_id) {
            return ids;
        }
        let dir = self.registry.sessions_root().join(user_id);
        let scanned =
            tokio::task::spawn_blocking(move || session_dir_ids(&dir))
                .await
                .unwrap_or_default();
        ids.extend(scanned);
        ids.sort();
        ids.dedup();
        ids
    }

    async fn read_head(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let user = self.user_of(id);
        self.registry.sink(&user, id).read_head_rows(limit).await
    }

    async fn read_tail(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let user = self.user_of(id);
        self.registry.sink(&user, id).read_tail(limit).await
    }

    fn writer(&self, id: &str) -> Option<Arc<dyn SessionLog>> {
        let user = self.user_of(id);
        Some(self.registry.writer(&user, id) as Arc<dyn SessionLog>)
    }

    /// A session's directory under the deployment's sessions root.
    ///
    /// The path is computed, not probed: a child's directory exists
    /// from the moment its first row is written, and the artefact set
    /// is written after that. `user_of` resolves the owner — from the
    /// registry, or from the directory that holds the session when
    /// this process has not opened it — so a session the control
    /// surface recovered from disk still lands in the right folder.
    fn session_dir(&self, id: &str) -> Option<std::path::PathBuf> {
        let user = self.user_of(id);
        Some(self.registry.sessions_root().join(user).join(id))
    }
}

/// The slot the `tool-task` row holds the index in.
///
/// The brick is assembled before the mount publishes the `sessions`
/// service, so the row hands the tool a slot and fills it once the
/// mount finished — the same shape the child-session writer source
/// uses.
pub struct SessionIndexSlot {
    inner: RwLock<Option<Arc<dyn SessionIndex>>>,
}

impl SessionIndexSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Fill the slot.
    pub fn set(&self, index: Arc<dyn SessionIndex>) {
        *self.inner.write() = Some(index);
    }
}

impl Default for SessionIndexSlot {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionIndex for SessionIndexSlot {
    async fn session_ids(&self) -> Vec<String> {
        // The guard is dropped before the await: the index is cloned
        // out of the slot, not held across the call.
        let index = { self.inner.read().clone() };
        match index {
            Some(index) => index.session_ids().await,
            None => Vec::new(),
        }
    }

    async fn session_ids_of_user(&self, user_id: &str) -> Vec<String> {
        let index = { self.inner.read().clone() };
        match index {
            Some(index) => index.session_ids_of_user(user_id).await,
            None => Vec::new(),
        }
    }

    async fn read_head(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let index = { self.inner.read().clone() };
        match index {
            Some(index) => index.read_head(id, limit).await,
            None => Ok(Vec::new()),
        }
    }

    async fn read_tail(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, SessionError> {
        let index = { self.inner.read().clone() };
        match index {
            Some(index) => index.read_tail(id, limit).await,
            None => Ok(Vec::new()),
        }
    }

    fn writer(&self, id: &str) -> Option<Arc<dyn SessionLog>> {
        let index = { self.inner.read().clone() };
        index.and_then(|index| index.writer(id))
    }

    fn session_dir(&self, id: &str) -> Option<std::path::PathBuf> {
        let index = { self.inner.read().clone() };
        index.and_then(|index| index.session_dir(id))
    }
}

#[cfg(test)]
mod tests {
    use uva::session::{subagent_enter, subagent_exit};

    use super::*;

    /// A registry with one parent session and one delegated child.
    async fn registry_with_child(
        root: &std::path::Path,
    ) -> Arc<SessionRegistry> {
        let registry = Arc::new(SessionRegistry::new(root.to_path_buf()));
        registry
            .create_with_user("parent".to_string(), "alice".to_string())
            .await
            .expect("the parent session");
        let child = registry.writer("alice", "child-1");
        child.append_event_nowait(&subagent_enter(
            "child-1", "parent", 1, "scout",
        ));
        child.append_event_nowait(&subagent_exit("child-1", 1, "completed"));
        child.flush().await.expect("the child's rows are durable");
        registry
    }

    /// The index reads the sessions the way the control surface needs
    /// them: the delegation header at the head, the footer at the
    /// tail, and the writer of a session that already exists.
    #[tokio::test]
    async fn the_index_reads_a_child_sessions_header_and_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry_with_child(dir.path()).await;
        let index = ServerSessionIndex::new(Arc::clone(&registry));

        let ids = index.session_ids().await;
        assert!(ids.contains(&"child-1".to_string()), "{ids:?}");

        let head = index.read_head("child-1", 4).await.expect("the head");
        assert_eq!(head.len(), 2, "the header and the footer");
        assert_eq!(head[0]["type"], "subagent_enter");
        assert_eq!(
            head[0]["data"]["parent_session_id"], "parent",
            "the header names the parent"
        );

        let tail = index.read_tail("child-1", 4).await.expect("the tail");
        assert_eq!(
            tail.last().and_then(|row| row["type"].as_str()),
            Some("subagent_exit")
        );

        assert!(
            index.writer("child-1").is_some(),
            "the child's session has a writer"
        );
    }

    /// The user-scoped half of the enumeration: one user's sessions
    /// from both sources, ephemeral sessions excluded, and a
    /// hand-built user id that is not one path component answers
    /// with nothing from the directory.
    #[tokio::test]
    async fn user_scoped_enumeration_reads_one_users_half() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let alice = registry.writer("alice", "alice-child");
        alice.append_event_nowait(&subagent_enter(
            "alice-child",
            "parent",
            1,
            "scout",
        ));
        alice.flush().await.expect("the rows are durable");
        let bob = registry.writer("bob", "bob-child");
        bob.append_event_nowait(&subagent_enter(
            "bob-child",
            "parent",
            1,
            "scout",
        ));
        bob.flush().await.expect("the rows are durable");
        std::fs::create_dir_all(
            dir.path().join("alice").join("anthropic-ephemeral"),
        )
        .expect("the ephemeral session's directory");
        let index = ServerSessionIndex::new(Arc::clone(&registry));

        let ids = index.session_ids_of_user("alice").await;
        assert!(ids.contains(&"alice-child".to_string()), "{ids:?}");
        assert!(
            !ids.contains(&"bob-child".to_string()),
            "another user's session is not enumerated: {ids:?}"
        );
        assert!(
            !ids.contains(&"anthropic-ephemeral".to_string()),
            "ephemeral sessions are excluded: {ids:?}"
        );
        assert!(
            index.session_ids_of_user("../bob").await.is_empty(),
            "a hand-built user id reads no directory"
        );
    }
    /// A session the process has never opened is still found: the
    /// directory is where a restarted process's sessions live.
    #[tokio::test]
    async fn the_index_finds_a_session_this_process_never_opened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        // A session written by an earlier process: the directory holds
        // it, the registry's index does not.
        let orphan = registry.writer("alice", "orphan");
        orphan.append_event_nowait(&subagent_enter(
            "orphan", "parent", 1, "scout",
        ));
        orphan.flush().await.expect("the rows are durable");
        drop(orphan);

        let restarted =
            Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let index = ServerSessionIndex::new(Arc::clone(&restarted));
        let ids = index.session_ids().await;
        assert!(ids.contains(&"orphan".to_string()), "{ids:?}");
    }

    /// The artefact set's home: the child's own session directory,
    /// under the deployment's sessions root.
    #[tokio::test]
    async fn the_index_names_the_directory_the_artefact_set_lands_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry_with_child(dir.path()).await;
        let index = ServerSessionIndex::new(Arc::clone(&registry));

        let session = index.session_dir("child-1").expect("a directory");
        assert_eq!(session, dir.path().join("alice").join("child-1"));
        assert!(
            session.is_dir(),
            "the child's own session directory holds its artefacts"
        );
    }

    /// The slot is empty until the host fills it, and a surface with
    /// an empty slot reads nothing.
    #[tokio::test]
    async fn an_empty_slot_reads_nothing() {
        let slot = SessionIndexSlot::new();
        assert!(slot.session_ids().await.is_empty());
        assert!(
            slot.read_head("child-1", 4)
                .await
                .expect("no rows")
                .is_empty()
        );
        assert!(slot.writer("child-1").is_none());
    }
}
