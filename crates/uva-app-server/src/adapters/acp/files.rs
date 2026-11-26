//! The client that owns a session's files.
//!
//! ACP lets the client own the workspace: the agent asks the client
//! to read and write the files (`fs/read_text_file` /
//! `fs/write_text_file`) instead of touching the disk. A client that
//! advertises the capabilities in `initialize` gets exactly that —
//! the session's `read` and `write` tools reach the client's own
//! files, so an editor's session works on what the user has open.
//!
//! The owner is registered per session: the connection answers for
//! the session it opened, and a session the client did not claim
//! keeps the deployment's own disk.

use std::path::{Path, PathBuf};

use agent_client_protocol::{
    Client,
    v1::{
        ConnectionTo,
        schema::{
            FileSystemCapabilities,
            ReadTextFileRequest,
            WriteTextFileRequest,
        },
    },
};
use async_trait::async_trait;
use uva::tool_files::{FileError, FileSystemOwner};

/// The files a client owns, over the connection it opened.
pub(crate) struct ClientFileSystem {
    connection: ConnectionTo<Client>,
    /// The client's working directory: the root the session's paths
    /// resolve against.
    root: PathBuf,
    /// What the client offered in `initialize`.
    capabilities: FileSystemCapabilities,
}

impl ClientFileSystem {
    /// The owner over the client's files.
    pub(crate) fn new(
        connection: ConnectionTo<Client>,
        root: PathBuf,
        capabilities: FileSystemCapabilities,
    ) -> Self {
        Self {
            connection,
            root,
            capabilities,
        }
    }

    /// The error for an operation the client did not offer.
    fn unoffered(method: &str) -> FileError {
        FileError::Failed(format!(
            "the client does not offer `{method}`; it did not advertise the \
             capability in `initialize`"
        ))
    }
}

#[async_trait]
impl FileSystemOwner for ClientFileSystem {
    fn session_root(&self, _session_id: &str) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    async fn read_file(
        &self,
        session_id: &str,
        path: &Path,
    ) -> Result<Vec<u8>, FileError> {
        if !self.capabilities.read_text_file {
            return Err(Self::unoffered("fs/read_text_file"));
        }
        let request = ReadTextFileRequest::new(
            session_id.to_string(),
            path.to_path_buf(),
        );
        let response = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(|error| {
                FileError::Failed(format!(
                    "the client refused to read '{}': {error}",
                    path.display()
                ))
            })?;
        Ok(response.content.into_bytes())
    }

    async fn write_file(
        &self,
        session_id: &str,
        path: &Path,
        content: &[u8],
        _create_directories: bool,
    ) -> Result<(), FileError> {
        if !self.capabilities.write_text_file {
            return Err(Self::unoffered("fs/write_text_file"));
        }
        let text = std::str::from_utf8(content).map_err(|error| {
            FileError::Failed(format!(
                "the file is not valid UTF-8 and cannot be written through \
                 the client: {error}"
            ))
        })?;
        let request = WriteTextFileRequest::new(
            session_id.to_string(),
            path.to_path_buf(),
            text.to_string(),
        );
        self.connection
            .send_request(request)
            .block_task()
            .await
            .map(|_| ())
            .map_err(|error| {
                FileError::Failed(format!(
                    "the client refused to write '{}': {error}",
                    path.display()
                ))
            })
    }
}
