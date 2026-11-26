//! The deployment's terminal owner.
//!
//! A deployment serves many sessions, and a session's commands run in
//! one of two places: this machine, under the shell tool's own policy
//! and sandbox, or — when the client owns the terminal — the client's
//! pane, under the client's own rules.
//!
//! The registry is the deployment's one answer: a session with no
//! client owner runs here; a session with one asks the client. The
//! shell tool holds this owner and nothing else, so the model is told
//! one thing about where a command runs and the routing cannot drift
//! from it.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::RwLock;
use uva::tool_shell::{TerminalError, TerminalOwner, TerminalRun};

/// The owner of every session's terminal.
pub struct SessionTerminals {
    /// The clients that own their sessions' terminals, keyed by
    /// session id.
    clients: RwLock<HashMap<String, Arc<dyn TerminalOwner>>>,
}

impl std::fmt::Debug for SessionTerminals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTerminals")
            .field("claimed_sessions", &self.clients.read().len())
            .finish_non_exhaustive()
    }
}

impl Default for SessionTerminals {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTerminals {
    /// The owner over this machine's terminals.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
        }
    }

    /// The client that owns `session_id`'s terminal.
    ///
    /// Registering a session that already has an owner replaces it —
    /// a client that reconnects brings its own connection, and the
    /// old one would answer for a session it no longer holds.
    pub fn register(&self, session_id: &str, owner: Arc<dyn TerminalOwner>) {
        self.clients.write().insert(session_id.to_string(), owner);
    }

    /// Drop the client that owned `session_id`'s terminal.
    ///
    /// The session's commands go back to this machine: a client that
    /// goes away does not take the deployment's own machine with it.
    pub fn release(&self, session_id: &str) {
        self.clients.write().remove(session_id);
    }

    /// The client that owns `session_id`'s terminal, if any.
    #[must_use]
    pub fn client(&self, session_id: &str) -> Option<Arc<dyn TerminalOwner>> {
        self.clients.read().get(session_id).cloned()
    }
}

#[async_trait]
impl TerminalOwner for SessionTerminals {
    fn owns(&self, session_id: &str) -> bool {
        self.client(session_id).is_some()
    }

    async fn run(
        &self,
        session_id: &str,
        command: &str,
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> Result<TerminalRun, TerminalError> {
        match self.client(session_id) {
            Some(owner) => owner.run(session_id, command, cwd, timeout).await,
            None => Err(TerminalError::Failed(
                "this deployment runs commands on its own machine".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An owner that answers with a fixed run, so the registry's
    /// routing can be told apart from this machine.
    struct FixedTerminals {
        answer: &'static str,
    }

    #[async_trait]
    impl TerminalOwner for FixedTerminals {
        fn owns(&self, _session_id: &str) -> bool {
            true
        }

        async fn run(
            &self,
            _session_id: &str,
            _command: &str,
            _cwd: &std::path::Path,
            _timeout: Duration,
        ) -> Result<TerminalRun, TerminalError> {
            Ok(TerminalRun {
                output: self.answer.to_string(),
                exit_code: Some(0),
                timed_out: false,
            })
        }
    }

    #[tokio::test]
    async fn a_claimed_session_runs_in_its_clients_terminal() {
        let owner = SessionTerminals::new();
        assert!(
            !owner.owns("s1"),
            "an unclaimed session runs on this machine"
        );
        owner.register(
            "s1",
            Arc::new(FixedTerminals {
                answer: "from the client",
            }),
        );
        assert!(owner.owns("s1"), "the client owns the session");
        let run = owner
            .run(
                "s1",
                "echo hi",
                std::path::Path::new("/tmp"),
                Duration::from_secs(5),
            )
            .await
            .expect("the client answers");
        assert_eq!(run.output, "from the client");
        // Another session is untouched.
        assert!(!owner.owns("s2"));
    }

    #[tokio::test]
    async fn a_released_session_goes_back_to_this_machine() {
        let owner = SessionTerminals::new();
        owner.register(
            "s1",
            Arc::new(FixedTerminals {
                answer: "from the client",
            }),
        );
        owner.release("s1");
        assert!(!owner.owns("s1"));
        assert!(owner.client("s1").is_none());
    }
}
