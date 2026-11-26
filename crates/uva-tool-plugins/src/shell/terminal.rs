//! Who runs a session's commands.
//!
//! The deployment's own machine is the default: the shell tool spawns
//! the process under its own policy and its own sandbox. A host whose
//! **client** owns the terminal — an editor that runs the agent
//! against the terminal the user watches — installs an owner that asks
//! the client instead, so the command lands in the pane the user can
//! see and interrupt.
//!
//! The seam exists for the same reason the file seam does
//! ([`crate::files::FileSystemOwner`]): the tool's contract must not
//! drift from where the command actually runs. The description shown
//! to the model names both paths, and the owner decides which one a
//! session takes.
//!
//! The session id is part of every call: one deployment serves many
//! sessions, and only the sessions whose client owns the terminal run
//! there — every other session keeps running on this machine.

use std::{path::Path, time::Duration};

use async_trait::async_trait;
use thiserror::Error;

/// Why a command could not run in the session's terminal.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TerminalError {
    /// The terminal refused the command.
    #[error("{0}")]
    Failed(String),
}

/// What one command produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalRun {
    /// The output, as the terminal showed it.
    ///
    /// A client terminal is one pane, one stream: what arrives here
    /// is what the pane holds, with the client's own line endings and
    /// its own interleaving of the command's two streams.
    pub output: String,
    /// The command's exit status, when it finished.
    pub exit_code: Option<i32>,
    /// Whether the command was killed for running past its deadline.
    pub timed_out: bool,
}

/// The owner of the terminals a session's commands run in.
#[async_trait]
pub trait TerminalOwner: Send + Sync {
    /// Whether this session's commands run in the client's terminal
    /// rather than on this machine.
    fn owns(&self, session_id: &str) -> bool;

    /// Run one command in the session's terminal and collect what it
    /// produced.
    ///
    /// `command` is the same string the shell tool would hand to
    /// `sh -c`; the owner decides how it is run. `cwd` is the
    /// session's working directory, and `timeout` the deadline the
    /// tool would have enforced locally.
    async fn run(
        &self,
        session_id: &str,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<TerminalRun, TerminalError>;
}

/// The owner of every session that runs on this machine.
///
/// It owns nothing: every session's commands run locally, under the
/// shell tool's own policy. It is the default the tool carries when
/// no host installed a client owner.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalTerminals;

#[async_trait]
impl TerminalOwner for LocalTerminals {
    fn owns(&self, _session_id: &str) -> bool {
        false
    }

    async fn run(
        &self,
        _session_id: &str,
        _command: &str,
        _cwd: &Path,
        _timeout: Duration,
    ) -> Result<TerminalRun, TerminalError> {
        // Unreachable in practice: the tool only calls `run` for a
        // session `owns` claims, and this owner claims none. The
        // answer is a refusal rather than a silent success — a
        // command that never ran must not look like one that did.
        Err(TerminalError::Failed(
            "this deployment runs commands on its own machine".to_string(),
        ))
    }
}
