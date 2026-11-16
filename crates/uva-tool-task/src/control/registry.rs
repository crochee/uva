//! The control surface's handle table: what the surface keeps for
//! every child of a session, and the child's memory lifecycle.
//!
//! A live child needs four things the roster alone does not carry:
//! the sender half of its steer channel, its own cancellation token,
//! its session writer, and the peer it runs. The table keeps all four
//! next to the child's state, so the three control tools can act on a
//! child without knowing how it was started.
//!
//! The table is keyed by the **parent** session: a session's model
//! only ever sees its own children.
//!
//! # The memory lifecycle
//!
//! A child that has been idle past [`PARK_TTL`](super::state::PARK_TTL)
//! is **parked**: its four handles are dropped, its row stays. That is
//! what `parked` means here — the process no longer holds the child,
//! so a message has to wake it from its session log again
//! ([`MessageTarget::ColdWake`]). Nothing else releases a child's
//! handles: `kill` reaches the terminal state and releases them
//! because a terminal child will never need a round again.
//!
//! The session that owns a bucket is never a row, and [`ControlRegistry::tick`]
//! refuses to park a row whose id is the bucket it sits in: the main
//! session has no lifecycle. Only delegated children do.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use futures::channel::mpsc::UnboundedSender;
use parking_lot::{Mutex, RwLock};
use uva_agent::{Agent, ChannelInbox};
use uva_core::{CancelToken, Clock, SharedClock};
use uva_provider::Message;
use uva_session::SessionLog;

use super::state::{ChildState, ChildStatus, RoundOutcome, TransitionError};

/// What the model sees of one child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildSummary {
    /// The child's session id — the handle the model names.
    pub id: String,
    /// The peer agent the child runs as.
    pub name: String,
    /// Where the child is in its lifecycle.
    pub status: ChildStatus,
    /// One line about what the child is doing now.
    pub activity: String,
}

/// A control operation the surface refuses.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// This session has no child of that id.
    #[error("no sub-agent `{0}` in this session")]
    UnknownChild(String),
    /// The lifecycle refused the transition.
    #[error("{0}")]
    Rejected(#[from] TransitionError),
    /// The child is running but has no channel to steer it through.
    #[error("this sub-agent has no live run to steer")]
    NoLiveRun,
    /// The child's peer is not in the catalog.
    #[error("the peer agent `{0}` is not registered")]
    UnknownPeer(String),
    /// The child's session log cannot be read or written.
    #[error("this sub-agent's session log is not reachable")]
    NoSession,
    /// No session index is installed.
    #[error(
        "no session index is installed: the control surface cannot \
             read the sessions"
    )]
    NoIndex,
}

/// What a message to a running child needs: where the steer row goes
/// and the channel the running round drains.
#[derive(Clone)]
pub struct SteerTarget {
    /// The child's session writer — the steer row's home.
    pub writer: Option<Arc<dyn SessionLog>>,
    /// The sender half of the running round's inbox.
    pub steer: UnboundedSender<Message>,
}

/// What another round of a child needs.
#[derive(Clone)]
pub struct FollowUpTarget {
    /// The peer the child runs as.
    pub peer: Arc<dyn Agent>,
    /// The child's session writer — the round's rows go there.
    pub writer: Option<Arc<dyn SessionLog>>,
    /// The child's own cancellation token.
    pub cancel: Arc<dyn CancelToken>,
    /// The round's inbox: the channel a steer lands in.
    pub inbox: Arc<ChannelInbox>,
    /// The sender half of the round's inbox.
    pub steer: UnboundedSender<Message>,
    /// The depth the round runs at.
    pub depth: usize,
}

/// What a message to a child does, decided by the child's state.
#[derive(Clone)]
pub enum MessageTarget {
    /// The child is running: the message steers its round.
    Steer(SteerTarget),
    /// The child is idle in memory: the message starts another round.
    FollowUp(FollowUpTarget),
    /// The child is not in this process's memory: the message has to
    /// wake it from a cold start.
    ColdWake,
}

impl std::fmt::Debug for MessageTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Steer(target) => {
                f.debug_tuple("Steer").field(target).finish()
            }
            Self::FollowUp(target) => {
                f.debug_tuple("FollowUp").field(target).finish()
            }
            Self::ColdWake => f.write_str("ColdWake"),
        }
    }
}

impl std::fmt::Debug for SteerTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteerTarget")
            .field("writer", &self.writer.is_some())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for FollowUpTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FollowUpTarget")
            .field("writer", &self.writer.is_some())
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

/// What a cold wake resolved for the child it wakes.
pub struct ColdWake {
    /// The agent name the child ran under, read from its header.
    pub name: String,
    /// The peer the woken round runs.
    pub peer: Arc<dyn Agent>,
    /// The child's session writer.
    pub writer: Option<Arc<dyn SessionLog>>,
    /// The token the woken round is cancelled by.
    pub cancel: Arc<dyn CancelToken>,
    /// The depth the woken round runs at.
    pub depth: usize,
}

/// What the surface keeps for one child.
pub struct ChildHandle {
    state: ChildState,
    /// The sender half of the child's steer channel.
    steer: Option<UnboundedSender<Message>>,
    /// The child's own cancellation token: the interrupt trigger.
    cancel: Option<Arc<dyn CancelToken>>,
    /// The child's session writer.
    writer: Option<Arc<dyn SessionLog>>,
    /// The peer the child runs as.
    peer: Option<Arc<dyn Agent>>,
    /// The depth the child's rounds run at.
    depth: usize,
    /// When the child last entered `idle`; the parking TTL counts
    /// from here. `None` while the child runs, is parked, or is
    /// terminal.
    idle_since: Option<DateTime<Utc>>,
}

impl ChildHandle {
    /// A handle for a child in `state`, cancelled by `cancel`.
    #[must_use]
    pub fn new(state: ChildState, cancel: Arc<dyn CancelToken>) -> Self {
        Self {
            state,
            steer: None,
            cancel: Some(cancel),
            writer: None,
            peer: None,
            depth: 1,
            idle_since: None,
        }
    }

    /// A row for a child this process does not hold: one recovered
    /// from the session log. It has no handles by construction,
    /// because `parked` is exactly "the handles are gone".
    #[must_use]
    pub fn adopted(id: impl Into<String>, name: impl Into<String>) -> Self {
        let mut state = ChildState::spawn(id, name);
        // A child read back from a log is parked in the only sense
        // that matters to the control surface: nothing is in memory.
        state.status = ChildStatus::Parked;
        state.activity = "released from memory (message wakes it)".to_string();
        state.idle_for = super::state::PARK_TTL;
        Self {
            state,
            steer: None,
            cancel: None,
            writer: None,
            peer: None,
            depth: 1,
            idle_since: None,
        }
    }

    /// Attach the sender half of the child's steer channel.
    #[must_use]
    pub fn with_steer(mut self, steer: UnboundedSender<Message>) -> Self {
        self.steer = Some(steer);
        self
    }

    /// Attach the child's session writer.
    #[must_use]
    pub fn with_writer(mut self, writer: Arc<dyn SessionLog>) -> Self {
        self.writer = Some(writer);
        self
    }

    /// Attach the peer the child runs as.
    #[must_use]
    pub fn with_peer(mut self, peer: Arc<dyn Agent>) -> Self {
        self.peer = Some(peer);
        self
    }

    /// Set the depth the child's rounds run at.
    #[must_use]
    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// The child's state.
    #[must_use]
    pub fn state(&self) -> &ChildState {
        &self.state
    }

    /// Whether the child's in-memory handles — its steer channel, its
    /// cancellation token, its session writer and its peer — are
    /// still attached.
    ///
    /// A parked child, a killed child and a child recovered from a
    /// log have none: the process holds the row, not the child.
    #[must_use]
    pub fn has_handles(&self) -> bool {
        self.steer.is_some()
            || self.cancel.is_some()
            || self.writer.is_some()
            || self.peer.is_some()
    }

    /// Release the child's handles. The row stays: the roster still
    /// names the child, and a message can wake it.
    fn release(&mut self) {
        self.steer = None;
        self.cancel = None;
        self.writer = None;
        self.peer = None;
    }
}

impl std::fmt::Debug for ChildHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildHandle")
            .field("id", &self.state.id)
            .field("status", &self.state.status)
            .field("handles", &self.has_handles())
            .finish_non_exhaustive()
    }
}

/// The handle table: one child per id, per parent session.
pub struct ControlRegistry {
    sessions: Mutex<HashMap<String, HashMap<String, ChildHandle>>>,
    clock: RwLock<SharedClock>,
}

impl Default for ControlRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlRegistry {
    /// An empty table reading the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(SharedClock::system())
    }

    /// An empty table whose TTL is measured on `clock`.
    ///
    /// Tests install a clock they can advance; production keeps the
    /// system clock.
    #[must_use]
    pub fn with_clock(clock: SharedClock) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            clock: RwLock::new(clock),
        }
    }

    /// Replace the clock the TTL is measured on.
    ///
    /// A builder seam: install it before the first child is filed.
    pub fn set_clock(&self, clock: SharedClock) {
        *self.clock.write() = clock;
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.read().now()
    }

    /// File a child's handle under its parent session.
    ///
    /// A child that is already filed is replaced: a cold wake files
    /// the same id again with the handles of its new round.
    pub fn register(&self, parent: &str, mut handle: ChildHandle) {
        if handle.state.status == ChildStatus::Idle {
            handle.idle_since = Some(self.now());
        }
        let mut sessions = self.sessions.lock();
        sessions
            .entry(parent.to_string())
            .or_default()
            .insert(handle.state.id.clone(), handle);
    }

    /// File a child this process does not hold: one read back from
    /// the session log.
    ///
    /// The row is `parked` — no handles exist to release — so the
    /// roster survives a restart and a message to it takes the
    /// cold-wake branch.
    ///
    /// Returns `false` when the child is already filed (the live row
    /// always wins over the log's copy).
    pub fn adopt(&self, parent: &str, id: &str, name: &str) -> bool {
        let mut sessions = self.sessions.lock();
        let children = sessions.entry(parent.to_string()).or_default();
        if children.contains_key(id) {
            return false;
        }
        children.insert(id.to_string(), ChildHandle::adopted(id, name));
        true
    }

    /// The children of `parent`, as the model sees them.
    ///
    /// This is the roster's only data source: rows come from the
    /// table, never from a scan at read time.
    pub fn list(&self, parent: &str) -> Vec<ChildSummary> {
        let sessions = self.sessions.lock();
        let Some(children) = sessions.get(parent) else {
            return Vec::new();
        };
        let mut rows: Vec<ChildSummary> = children
            .values()
            .map(|handle| ChildSummary {
                id: handle.state.id.clone(),
                name: handle.state.name.clone(),
                status: handle.state.status,
                activity: handle.state.activity.clone(),
            })
            .collect();
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        rows
    }

    /// Advance the lifecycle: every idle child past the TTL parks.
    ///
    /// Returns the `(parent session, child id)` pairs that parked, so
    /// the caller can drop the bookkeeping it kept for them.
    ///
    /// A row whose id is the bucket it sits in is the session that
    /// owns the table — the main session — and never parks.
    pub fn tick(&self) -> Vec<(String, String)> {
        let now = self.now();
        let mut parked = Vec::new();
        let mut sessions = self.sessions.lock();
        for (parent, children) in sessions.iter_mut() {
            for (id, handle) in children.iter_mut() {
                if id == parent {
                    continue;
                }
                let Some(since) = handle.idle_since else {
                    continue;
                };
                let elapsed = now.signed_duration_since(since);
                let Ok(elapsed) = elapsed.to_std() else {
                    // The clock went backwards; do not time-travel the
                    // TTL with it.
                    continue;
                };
                if handle.state.age(elapsed) {
                    handle.release();
                    handle.idle_since = None;
                    parked.push((parent.clone(), id.clone()));
                } else {
                    // `age` consumed the elapsed span; the next tick
                    // measures from here.
                    handle.idle_since = Some(now);
                }
            }
        }
        parked
    }

    /// Whether `parent`'s child `child` still has its in-memory
    /// handles attached; `None` when this session has no such child.
    ///
    /// A parked child, a killed child and a child recovered from a log
    /// answer `false`: the row is the only thing this process keeps.
    #[must_use]
    pub fn has_handles(&self, parent: &str, child: &str) -> Option<bool> {
        let sessions = self.sessions.lock();
        sessions
            .get(parent)
            .and_then(|children| children.get(child))
            .map(ChildHandle::has_handles)
    }

    /// What a message to `child` should do.
    ///
    /// `cancel` is the token the new round is cancelled by. It is
    /// installed together with the move, so an interrupt that lands
    /// right after the move fires the round's own token and not the
    /// one the previous round left behind.
    ///
    /// # Errors
    ///
    /// Refused when this session has no such child, when the child's
    /// state forbids the move, or when a running child has no channel
    /// to steer it through.
    pub fn message_target(
        &self,
        parent: &str,
        child: &str,
        cancel: Arc<dyn CancelToken>,
    ) -> Result<MessageTarget, ControlError> {
        let mut sessions = self.sessions.lock();
        let Some(handle) = sessions
            .get_mut(parent)
            .and_then(|children| children.get_mut(child))
        else {
            return Err(ControlError::UnknownChild(child.to_string()));
        };
        match handle.state.status {
            ChildStatus::Running => {
                handle.state.message().map_err(ControlError::Rejected)?;
                let steer =
                    handle.steer.clone().ok_or(ControlError::NoLiveRun)?;
                Ok(MessageTarget::Steer(SteerTarget {
                    writer: handle.writer.clone(),
                    steer,
                }))
            }
            ChildStatus::Idle => {
                // The peer is checked before the move: a child that
                // cannot run must not be left "running" a round that
                // never starts.
                let peer = handle.peer.clone().ok_or(
                    ControlError::UnknownPeer(handle.state.name.clone()),
                )?;
                let writer = handle.writer.clone();
                let depth = handle.depth;
                handle.state.message().map_err(ControlError::Rejected)?;
                // The previous round's inbox died with that round, so
                // the new one gets a channel of its own — and a token
                // of its own: the round that was interrupted left a
                // cancelled token behind.
                let (inbox, steer) = ChannelInbox::channel();
                handle.steer = Some(steer.clone());
                handle.cancel = Some(Arc::clone(&cancel));
                handle.idle_since = None;
                Ok(MessageTarget::FollowUp(FollowUpTarget {
                    peer,
                    writer,
                    cancel,
                    inbox,
                    steer,
                    depth,
                }))
            }
            ChildStatus::Parked => {
                // The child's memory is gone: the wake reads its log
                // and starts a fresh round. The handle is replaced by
                // the wake itself.
                Ok(MessageTarget::ColdWake)
            }
            ChildStatus::Aborted => {
                Err(ControlError::Rejected(TransitionError::Aborted {
                    action: "be messaged",
                }))
            }
        }
    }

    /// File a woken child and hand over its new round.
    ///
    /// The wake takes over the parked row: the new round gets a
    /// channel of its own, and the child is running again.
    ///
    /// # Errors
    ///
    /// Refused when the child's state forbids the move.
    pub fn cold_wake(
        &self,
        parent: &str,
        child: &str,
        wake: ColdWake,
    ) -> Result<FollowUpTarget, ControlError> {
        let ColdWake {
            name,
            peer,
            writer,
            cancel,
            depth,
        } = wake;
        let mut state = ChildState::spawn(child, &name);
        state.activity = "cold wake · new round".to_string();
        state.revived = true;
        let (inbox, steer) = ChannelInbox::channel();
        let mut handle = ChildHandle::new(state, Arc::clone(&cancel))
            .with_steer(steer.clone())
            .with_peer(Arc::clone(&peer))
            .with_depth(depth);
        if let Some(writer) = writer.clone() {
            handle = handle.with_writer(writer);
        }
        self.register(parent, handle);
        Ok(FollowUpTarget {
            peer,
            writer,
            cancel,
            inbox,
            steer,
            depth,
        })
    }

    /// Fire the interrupt of `child`.
    ///
    /// The returned token is the child's own: firing it stops the
    /// round that is in flight and leaves the child able to answer
    /// another message.
    ///
    /// # Errors
    ///
    /// Refused when this session has no such child, or when the
    /// child's state forbids the move.
    pub fn interrupt(
        &self,
        parent: &str,
        child: &str,
    ) -> Result<Arc<dyn CancelToken>, ControlError> {
        let mut sessions = self.sessions.lock();
        let Some(handle) = sessions
            .get_mut(parent)
            .and_then(|children| children.get_mut(child))
        else {
            return Err(ControlError::UnknownChild(child.to_string()));
        };
        // The state owns the refusal and its wording; the token is
        // read only after the child is known to be interruptible, so a
        // refused interrupt never stops a round.
        handle
            .state
            .require_running("be interrupted")
            .map_err(ControlError::Rejected)?;
        let cancel = handle.cancel.clone().ok_or(ControlError::NoLiveRun)?;
        handle.state.interrupt().map_err(ControlError::Rejected)?;
        handle.idle_since = Some(self.clock.read().now());
        Ok(cancel)
    }

    /// Hard-terminate `child`: the terminal state.
    ///
    /// The child's handles are released with the move: a terminal
    /// child will never need another round.
    ///
    /// # Errors
    ///
    /// Refused when this session has no such child, or when the child
    /// is already terminal.
    pub fn kill(&self, parent: &str, child: &str) -> Result<(), ControlError> {
        let mut sessions = self.sessions.lock();
        let Some(handle) = sessions
            .get_mut(parent)
            .and_then(|children| children.get_mut(child))
        else {
            return Err(ControlError::UnknownChild(child.to_string()));
        };
        handle.state.kill().map_err(ControlError::Rejected)?;
        handle.release();
        handle.idle_since = None;
        Ok(())
    }

    /// Record that a round of `child` ended.
    ///
    /// A child that was interrupted while the round was in flight is
    /// already idle: the interrupt is what happened, so the settle
    /// leaves it alone.
    pub fn settle(&self, parent: &str, child: &str, outcome: RoundOutcome) {
        let mut sessions = self.sessions.lock();
        let Some(handle) = sessions
            .get_mut(parent)
            .and_then(|children| children.get_mut(child))
        else {
            return;
        };
        if handle.state.is_running() {
            let _ = handle.state.finish(outcome);
            handle.idle_since = Some(self.clock.read().now());
        }
    }
}

impl std::fmt::Debug for ControlRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sessions = self.sessions.lock();
        f.debug_struct("ControlRegistry")
            .field("sessions", &sessions.len())
            .finish_non_exhaustive()
    }
}
