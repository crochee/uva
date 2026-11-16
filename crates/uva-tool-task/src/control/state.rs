//! The sub-agent lifecycle: the four states a delegated child can be
//! in, and the transitions the control surface drives.
//!
//! The machine is pure — no handles, no clock, no runtime — so the
//! whole transition table is testable without a run, a log or a
//! session. The prototype's reducer is the reference: every
//! transition here answers the question the prototype's answered, and
//! an illegal transition is **refused** (with an error the model
//! reads), never silently applied and never left hanging.
//!
//! | operation | running | idle | parked | aborted |
//! |---|---|---|---|---|
//! | `step` | advances | refused | refused | refused |
//! | `finish` | → idle | refused | refused | refused |
//! | `message` | steer | → running (follow-up) | → running (cold wake) | refused |
//! | `interrupt` | → idle | refused | refused | refused |
//! | `kill` | → aborted | → aborted | → aborted | refused |
//! | `age` | — | → parked at the TTL | — | — |
//!
//! `interrupt` is not `kill`: the first stops the round and leaves the
//! child able to answer another message, the second is the terminal
//! state.

use std::time::Duration;

/// How long an idle child stays in memory before it is parked.
///
/// The artefacts and the child session stay on disk either way; only
/// the in-memory handles are released.
pub const PARK_TTL: Duration = Duration::from_secs(5 * 60);

/// Where a delegated child is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildStatus {
    /// A round is in flight.
    Running,
    /// The last round ended; the child can be asked for another one.
    Idle,
    /// The child has been idle past the TTL; its handles are released.
    Parked,
    /// The child was hard-terminated. Terminal.
    Aborted,
}

impl ChildStatus {
    /// The name the model sees.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Parked => "parked",
            Self::Aborted => "aborted",
        }
    }
}

impl std::fmt::Display for ChildStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a child's round ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundOutcome {
    /// The round ran to its end.
    Completed,
    /// The round was stopped — an interrupt, or the parent going away.
    Cancelled,
    /// The round ended in an error.
    Failed,
}

/// What a `message` did, decided by the target's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageOutcome {
    /// The child was running: the message steers its round.
    Steered,
    /// The child was idle in memory: the message starts another round.
    FollowedUp,
    /// The child was parked: the message wakes it from a cold start.
    Woken,
}

/// A transition the machine refuses.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    /// The operation needs a running child and the target is not one.
    #[error("only a running sub-agent can {action} (this one is {status})")]
    NotRunning {
        /// What the caller tried to do.
        action: &'static str,
        /// The state the child is in.
        status: ChildStatus,
    },
    /// The child is terminal: nothing may be done to it any more.
    #[error(
        "`aborted` is terminal: this sub-agent cannot {action} (its \
             session stays readable)"
    )]
    Aborted {
        /// What the caller tried to do.
        action: &'static str,
    },
}

/// One child's state: the row the control surface's roster shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildState {
    /// The child's session id — the handle the model names.
    pub id: String,
    /// The peer agent the child runs as.
    pub name: String,
    /// Where the child is in its lifecycle.
    pub status: ChildStatus,
    /// One line about what the child is doing now.
    pub activity: String,
    /// Whether the child was ever woken from a cold start.
    pub revived: bool,
    /// How long the child has been idle.
    pub idle_for: Duration,
}

impl ChildState {
    /// A child that has just been dispatched.
    #[must_use]
    pub fn spawn(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status: ChildStatus::Running,
            activity: "starting".to_string(),
            revived: false,
            idle_for: Duration::ZERO,
        }
    }

    /// The child's next step: only a running child advances.
    ///
    /// # Errors
    ///
    /// Refused when the child is not running.
    pub fn step(
        &mut self,
        activity: impl Into<String>,
    ) -> Result<(), TransitionError> {
        self.require_running("advance")?;
        self.activity = activity.into();
        Ok(())
    }

    /// The child's round ended.
    ///
    /// # Errors
    ///
    /// Refused when the child is not running.
    pub fn finish(
        &mut self,
        outcome: RoundOutcome,
    ) -> Result<(), TransitionError> {
        self.require_running("finish")?;
        self.status = ChildStatus::Idle;
        self.activity = match outcome {
            RoundOutcome::Completed => "finished",
            RoundOutcome::Cancelled => "cancelled",
            RoundOutcome::Failed => "failed",
        }
        .to_string();
        self.idle_for = Duration::ZERO;
        Ok(())
    }

    /// A message, dispatched by the target's state.
    ///
    /// # Errors
    ///
    /// Refused when the child is terminal.
    pub fn message(&mut self) -> Result<MessageOutcome, TransitionError> {
        match self.status {
            ChildStatus::Running => {
                self.activity = "steered".to_string();
                Ok(MessageOutcome::Steered)
            }
            ChildStatus::Idle => {
                self.status = ChildStatus::Running;
                self.activity = "follow-up round".to_string();
                self.idle_for = Duration::ZERO;
                Ok(MessageOutcome::FollowedUp)
            }
            ChildStatus::Parked => {
                self.status = ChildStatus::Running;
                self.activity = "cold wake · new round".to_string();
                self.revived = true;
                self.idle_for = Duration::ZERO;
                Ok(MessageOutcome::Woken)
            }
            ChildStatus::Aborted => Err(TransitionError::Aborted {
                action: "be messaged",
            }),
        }
    }

    /// The child's current round is stopped; the child stays usable.
    ///
    /// # Errors
    ///
    /// Refused when the child is not running.
    pub fn interrupt(&mut self) -> Result<(), TransitionError> {
        self.require_running("be interrupted")?;
        self.status = ChildStatus::Idle;
        self.activity = "interrupted".to_string();
        self.idle_for = Duration::ZERO;
        Ok(())
    }

    /// The child is hard-terminated. Terminal.
    ///
    /// # Errors
    ///
    /// Refused when the child is already terminal.
    pub fn kill(&mut self) -> Result<(), TransitionError> {
        if self.status == ChildStatus::Aborted {
            return Err(TransitionError::Aborted {
                action: "be killed",
            });
        }
        self.status = ChildStatus::Aborted;
        self.activity = "hard-terminated".to_string();
        Ok(())
    }

    /// Time passes: an idle child parks once it has been idle for the
    /// TTL. Nothing else ages.
    ///
    /// Returns whether the child parked.
    pub fn age(&mut self, elapsed: Duration) -> bool {
        if self.status != ChildStatus::Idle {
            return false;
        }
        self.idle_for += elapsed;
        if self.idle_for >= PARK_TTL {
            self.status = ChildStatus::Parked;
            self.activity = "parked".to_string();
            return true;
        }
        false
    }

    /// Whether the child is running a round right now.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.status == ChildStatus::Running
    }

    /// The refusal an operation answers with when the child is not
    /// running; `Ok(())` when it is.
    pub(crate) fn require_running(
        &self,
        action: &'static str,
    ) -> Result<(), TransitionError> {
        if self.is_running() {
            return Ok(());
        }
        Err(TransitionError::NotRunning {
            action,
            status: self.status,
        })
    }
}
