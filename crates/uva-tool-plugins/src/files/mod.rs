//! Who owns the files a tool call touches.
//!
//! The deployment's own disk is the default owner. A host whose
//! **client** owns the workspace — an editor that runs the agent
//! against the files the user has open — installs an owner that asks
//! the client instead of touching the disk.
//!
//! The seam exists because the tool's own contract must not drift
//! from where the bytes actually live: the tool that reads a file
//! says *which* file it read, and the owner that answers decides
//! *where* that file is. One owner, one answer — the tool does not
//! keep a second path (a local read plus a client read) that could
//! disagree about what the model saw.
//!
//! The session id is part of every call: one deployment serves many
//! sessions, and the files of a client-owned session live in the
//! client's workspace, not in the deployment's.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use thiserror::Error;

/// Why a file operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FileError {
    /// The file does not exist.
    ///
    /// The owner says so explicitly: "the file is missing" and "the
    /// read failed" lead the tool to different next moves (append to
    /// an empty file vs. report the failure), and only the owner can
    /// tell them apart.
    #[error("the file does not exist")]
    Missing,
    /// The operation failed for another reason.
    #[error("{0}")]
    Failed(String),
}

/// The owner of the files a session's tools touch.
#[async_trait]
pub trait FileSystemOwner: Send + Sync {
    /// The directory this session's relative paths resolve against.
    ///
    /// `None` means "the deployment's own workspace" — the root the
    /// tool's context carries. A client-owned session answers with
    /// the client's directory, so the tool resolves and checks paths
    /// against the workspace the files really live in.
    fn session_root(&self, _session_id: &str) -> Option<PathBuf> {
        None
    }

    /// Read a file's bytes.
    ///
    /// The path is already resolved and checked against the session's
    /// root by the caller.
    async fn read_file(
        &self,
        session_id: &str,
        path: &Path,
    ) -> Result<Vec<u8>, FileError>;

    /// Write a file's bytes.
    ///
    /// The path is already resolved and checked against the session's
    /// root by the caller. `create_directories` is the caller's
    /// request that a missing parent directory be created first: the
    /// owner decides whether it can — the deployment's disk can, a
    /// client that offers no directory operation cannot, and the write
    /// then fails with the client's own answer.
    async fn write_file(
        &self,
        session_id: &str,
        path: &Path,
        content: &[u8],
        create_directories: bool,
    ) -> Result<(), FileError>;
}

/// The deployment's own disk: the default owner.
///
/// Every session reads and writes where the deployment's workspace
/// says — the behaviour every deployment had before a client could
/// own the workspace.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFileSystem;

#[async_trait]
impl FileSystemOwner for LocalFileSystem {
    async fn read_file(
        &self,
        _session_id: &str,
        path: &Path,
    ) -> Result<Vec<u8>, FileError> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(FileError::Missing)
            }
            Err(error) => Err(FileError::Failed(error.to_string())),
        }
    }

    async fn write_file(
        &self,
        _session_id: &str,
        path: &Path,
        content: &[u8],
        create_directories: bool,
    ) -> Result<(), FileError> {
        if create_directories
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                FileError::Failed(format!(
                    "failed to create parent directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        tokio::fs::write(path, content)
            .await
            .map_err(|error| FileError::Failed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_local_owner_reads_and_writes_the_disk() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("a.txt");
        let owner = LocalFileSystem;
        owner
            .write_file("s1", &path, b"hello", true)
            .await
            .expect("the write lands");
        assert_eq!(
            owner.read_file("s1", &path).await.expect("the read lands"),
            b"hello".to_vec()
        );
        assert_eq!(
            owner.session_root("s1"),
            None,
            "the deployment's own workspace is the context's root"
        );
    }

    #[tokio::test]
    async fn the_local_owner_says_when_a_file_is_missing() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let error = LocalFileSystem
            .read_file("s1", &dir.path().join("nope.txt"))
            .await
            .expect_err("the file is missing");
        assert_eq!(error, FileError::Missing);
    }
}
