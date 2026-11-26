//! The deployment's file-system owner.
//!
//! A deployment serves many sessions, and the files of a session live
//! in one of two places: the deployment's own disk, or — when the
//! client owns the workspace — the client's. The owner answers both
//! questions for the tools: *where* do this session's paths resolve
//! against, and *who* holds the bytes.
//!
//! The registry is the deployment's one answer: a session with no
//! client owner reads and writes the deployment's disk; a session with
//! one asks the client. Nothing else keeps a second truth — the tools
//! hold this owner and nothing else.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::RwLock;
use uva::tool_files::{FileError, FileSystemOwner, LocalFileSystem};

/// The owner of every session's files.
pub struct SessionFileSystem {
    /// The clients that own their sessions' files, keyed by session
    /// id.
    clients: RwLock<HashMap<String, Arc<dyn FileSystemOwner>>>,
    /// The deployment's own disk: the owner of every session no
    /// client claimed.
    local: Arc<dyn FileSystemOwner>,
}

impl std::fmt::Debug for SessionFileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFileSystem")
            .field("claimed_sessions", &self.clients.read().len())
            .finish_non_exhaustive()
    }
}

impl Default for SessionFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionFileSystem {
    /// The owner over the deployment's own disk.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            local: Arc::new(LocalFileSystem),
        }
    }

    /// The owner over the deployment's disk and a client.
    #[must_use]
    pub fn with_local(local: Arc<dyn FileSystemOwner>) -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            local,
        }
    }

    /// The client that owns `session_id`'s files.
    ///
    /// Registering a session that already has an owner replaces it —
    /// a client that reconnects brings its own connection, and the
    /// old one would answer for a session it no longer holds.
    pub fn register(&self, session_id: &str, owner: Arc<dyn FileSystemOwner>) {
        self.clients.write().insert(session_id.to_string(), owner);
    }

    /// Drop the client that owned `session_id`'s files.
    ///
    /// The session's files go back to the deployment's disk: a client
    /// that goes away does not take the deployment's own workspace
    /// with it.
    pub fn release(&self, session_id: &str) {
        self.clients.write().remove(session_id);
    }

    /// The client that owns `session_id`'s files, if any.
    #[must_use]
    pub fn client(&self, session_id: &str) -> Option<Arc<dyn FileSystemOwner>> {
        self.clients.read().get(session_id).cloned()
    }
}

#[async_trait]
impl FileSystemOwner for SessionFileSystem {
    fn session_root(&self, session_id: &str) -> Option<PathBuf> {
        self.client(session_id)
            .and_then(|owner| owner.session_root(session_id))
    }

    async fn read_file(
        &self,
        session_id: &str,
        path: &Path,
    ) -> Result<Vec<u8>, FileError> {
        match self.client(session_id) {
            Some(owner) => owner.read_file(session_id, path).await,
            None => self.local.read_file(session_id, path).await,
        }
    }

    async fn write_file(
        &self,
        session_id: &str,
        path: &Path,
        content: &[u8],
        create_directories: bool,
    ) -> Result<(), FileError> {
        match self.client(session_id) {
            Some(owner) => {
                owner
                    .write_file(session_id, path, content, create_directories)
                    .await
            }
            None => {
                self.local
                    .write_file(session_id, path, content, create_directories)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An owner that answers with a fixed body, so the registry's
    /// routing can be told apart from the local disk.
    struct FixedOwner {
        root: PathBuf,
        body: Vec<u8>,
    }

    #[async_trait]
    impl FileSystemOwner for FixedOwner {
        fn session_root(&self, _session_id: &str) -> Option<PathBuf> {
            Some(self.root.clone())
        }

        async fn read_file(
            &self,
            _session_id: &str,
            _path: &Path,
        ) -> Result<Vec<u8>, FileError> {
            Ok(self.body.clone())
        }

        async fn write_file(
            &self,
            _session_id: &str,
            _path: &Path,
            _content: &[u8],
            _create_directories: bool,
        ) -> Result<(), FileError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_claimed_session_answers_from_its_client() {
        let owner = SessionFileSystem::new();
        let path = PathBuf::from("/client/file.txt");
        // Unclaimed: the deployment's disk answers.
        assert_eq!(
            owner.session_root("s1"),
            None,
            "an unclaimed session uses the deployment's workspace"
        );
        owner.register(
            "s1",
            Arc::new(FixedOwner {
                root: PathBuf::from("/client"),
                body: b"from the client".to_vec(),
            }),
        );
        assert_eq!(
            owner.session_root("s1"),
            Some(PathBuf::from("/client")),
            "a claimed session resolves against the client's directory"
        );
        assert_eq!(
            owner
                .read_file("s1", &path)
                .await
                .expect("the client answers"),
            b"from the client".to_vec()
        );
        // Another session is untouched.
        assert_eq!(owner.session_root("s2"), None);
    }

    #[tokio::test]
    async fn a_released_session_goes_back_to_the_deployment() {
        let owner = SessionFileSystem::new();
        owner.register(
            "s1",
            Arc::new(FixedOwner {
                root: PathBuf::from("/client"),
                body: Vec::new(),
            }),
        );
        owner.release("s1");
        assert_eq!(owner.session_root("s1"), None);
        assert!(owner.client("s1").is_none());
    }
}
