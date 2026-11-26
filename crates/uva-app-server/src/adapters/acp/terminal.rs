//! The client that owns a session's terminal.
//!
//! ACP lets the client own the terminal: the agent asks the client
//! to run a command in the client's own pane (`terminal/create`,
//! `terminal/output`, `terminal/wait_for_exit`, `terminal/kill`,
//! `terminal/release`) instead of spawning a process on the machine
//! the agent runs on. A client that advertises the `terminal`
//! capability gets exactly that — the session's `shell` tool runs its
//! commands in the pane the user watches, so the user sees the work
//! and can interrupt it.
//!
//! The owner is registered per session, exactly like the file owner:
//! the connection answers for the session it opened, and a session
//! the client did not claim keeps running on the deployment's own
//! machine, under the deployment's own policy.
//!
//! The command is the same string the shell tool would hand to `sh -c`
//! — one shell, one string — so the client's pane shows what the
//! model asked for.

use std::{path::Path, time::Duration};

use agent_client_protocol::{
    Client,
    v1::{
        ConnectionTo,
        schema::{
            CreateTerminalRequest,
            KillTerminalRequest,
            ReleaseTerminalRequest,
            TerminalOutputRequest,
            WaitForTerminalExitRequest,
        },
    },
};
use async_trait::async_trait;
use uva::tool_shell::{TerminalError, TerminalOwner, TerminalRun};

/// The client's terminal: the pane the commands run in.
pub(crate) struct ClientTerminal {
    connection: ConnectionTo<Client>,
}

impl ClientTerminal {
    /// The terminal over the connection it opened.
    pub(crate) fn new(connection: ConnectionTo<Client>) -> Self {
        Self { connection }
    }

    /// The error for a request the client refused.
    fn refused(
        action: &str,
        error: agent_client_protocol::Error,
    ) -> TerminalError {
        TerminalError::Failed(format!("the client {action}: {error}"))
    }
}

#[async_trait]
impl TerminalOwner for ClientTerminal {
    fn owns(&self, _session_id: &str) -> bool {
        true
    }

    async fn run(
        &self,
        session_id: &str,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<TerminalRun, TerminalError> {
        // The client's terminal runs a command, not a shell string —
        // the string goes through `sh -c`, the same shell the local
        // path uses.
        let request = CreateTerminalRequest::new(session_id.to_string(), "sh")
            .args(vec!["-c".to_string(), command.to_string()])
            .cwd(Some(cwd.to_path_buf()));
        let created = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(|error| {
                Self::refused("refused to create a terminal", error)
            })?;
        let terminal_id = created.terminal_id;

        // Wait for the command, under the deadline the tool would have
        // enforced here. A client that never answers is a client that
        // hangs the run — the deadline cuts it, and the kill asks the
        // client to stop the command.
        let wait = WaitForTerminalExitRequest::new(
            session_id.to_string(),
            terminal_id.clone(),
        );
        let waited = tokio::time::timeout(
            timeout,
            self.connection.send_request(wait).block_task(),
        )
        .await;
        let mut timed_out = false;
        let mut exit_code = None;
        match waited {
            Ok(Ok(response)) => {
                exit_code =
                    response.exit_status.exit_code.map(|code| code as i32);
            }
            Ok(Err(error)) => {
                // The client failed the wait: release the terminal and
                // report what it said.
                self.release(session_id, &terminal_id).await;
                return Err(Self::refused("failed the terminal wait", error));
            }
            Err(_elapsed) => {
                timed_out = true;
                let kill = KillTerminalRequest::new(
                    session_id.to_string(),
                    terminal_id.clone(),
                );
                let _ = self.connection.send_request(kill).block_task().await;
            }
        }

        // The pane's text: what the user sees, what the model gets.
        let output = self
            .connection
            .send_request(TerminalOutputRequest::new(
                session_id.to_string(),
                terminal_id.clone(),
            ))
            .block_task()
            .await
            .map(|response| response.output)
            .unwrap_or_default();
        self.release(session_id, &terminal_id).await;

        Ok(TerminalRun {
            output,
            exit_code,
            timed_out,
        })
    }
}

impl ClientTerminal {
    /// Give the terminal back to the client.
    ///
    /// Best effort: a client that refuses the release keeps a pane
    /// open on its side, which is its own business — the command has
    /// already finished.
    async fn release(
        &self,
        session_id: &str,
        terminal_id: &agent_client_protocol::v1::schema::TerminalId,
    ) {
        let request = ReleaseTerminalRequest::new(
            session_id.to_string(),
            terminal_id.clone(),
        );
        let _ = self.connection.send_request(request).block_task().await;
    }
}
