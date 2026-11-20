//! The depth rule: one place says how deep a session may nest, and
//! remembers what depth each session runs at.
//!
//! # The rule
//!
//! **A session at the cap may not start another round.** Both paths
//! ask this module about the session that starts the round:
//!
//! - the `task` tool's caller — the parent that would spawn a child;
//! - the message tool's caller — the parent that would ask a child
//!   for another round.
//!
//! The delegation path has always said this; the wake path did not
//! say it at all, which left "how deep can a child session nest" with
//! two answers. Now both paths ask the same function about the same
//! session, so at the same depth they refuse the same rounds.
//!
//! The subject is the **caller**, not the child, because the caller
//! is what starts a round in both paths: a `task` call starts the
//! child's first round, and a message starts the child's next one. The
//! child's own depth is the accounting, not the judgment — it is what
//! the round *runs at*, and it is recorded so that a nested `task`
//! call reads it back.
//!
//! The cap itself ([`MAX_SUBAGENT_DEPTH`](crate::spec::MAX_SUBAGENT_DEPTH))
//! lives in `spec` because the crate's public surface names it there;
//! the rule that applies it lives here.
//!
//! # The accounting
//!
//! The depths live in one table ([`DepthLedger`]): the `task` tool
//! records the child it dispatched, the wake path records the child
//! it woke, and a nested `task` call reads its own depth out of it.
//! The table is the only place a depth is written, and the callers
//! never count layers themselves.
//!
//! An entry goes when the child's handles are released: a parked
//! child (its memory is gone) and a killed child (it is terminal)
//! both drop theirs. The surface owns the table, so it is the
//! surface that cleans it.
//!
//! # The defaults
//!
//! A session nothing recorded runs at the root's depth
//! ([`ROOT_DEPTH`]); a child session whose depth nothing recorded is
//! a first-level child ([`FIRST_CHILD_DEPTH`]).

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::spec::MAX_SUBAGENT_DEPTH;

/// The depth of the session that started the run: the root of the
/// tree, and the depth of a session nothing recorded.
pub(crate) const ROOT_DEPTH: usize = 0;

/// The depth of a first-level child session: the depth of a child
/// whose own depth nothing recorded.
pub(crate) const FIRST_CHILD_DEPTH: usize = 1;

/// The depths of the sessions this process runs.
///
/// The table is keyed by session id: the `task` tool records the
/// child it dispatched, the wake path records the child it woke, and
/// a nested `task` call reads its own depth out of it.
#[derive(Default)]
pub(crate) struct DepthLedger {
    depths: Mutex<HashMap<String, usize>>,
}

impl DepthLedger {
    /// An empty ledger.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The depth `session_id` runs at. A session nothing recorded is
    /// a top-level one.
    fn depth_of(&self, session_id: &str) -> usize {
        self.depths
            .lock()
            .get(session_id)
            .copied()
            .unwrap_or(ROOT_DEPTH)
    }

    /// Forget a session's depth: its handles are gone.
    fn forget(&self, session_id: &str) {
        self.depths.lock().remove(session_id);
    }

    /// Record the depth a session runs at.
    fn record(&self, session_id: &str, depth: usize) {
        self.depths.lock().insert(session_id.to_string(), depth);
    }
}

/// The refusal a model reads when a session at the cap may not start
/// another round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DepthRefusal {
    /// The depth of the session that may not start a round.
    depth: usize,
}

impl DepthRefusal {
    /// The depth that was refused.
    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    /// The refusal the `task` tool's cap has always produced: the
    /// wording the model read before the rule moved here, word for
    /// word.
    pub(crate) fn delegation(&self) -> String {
        format!(
            "cannot delegate: maximum sub-agent depth ({MAX_SUBAGENT_DEPTH}) \
             reached; the `task` tool is disabled at this nesting level"
        )
    }

    /// The refusal the message tool's cap produces: the same rule,
    /// said in the message tool's words.
    pub(crate) fn wake(&self) -> String {
        format!(
            "cannot start another round: maximum sub-agent depth \
             ({MAX_SUBAGENT_DEPTH}) reached; the `agents_message` tool \
             is disabled at this nesting level"
        )
    }
}

/// The one judgment: may the session at `depth` start another round?
///
/// A session at the cap may not. The cap is where the tree stops
/// growing, and a session that sits there may not start another
/// round — neither a new child nor another round of a child it holds.
fn admits(depth: usize) -> bool {
    depth < MAX_SUBAGENT_DEPTH
}

/// Admit a round for the session `session_id`: it may start one
/// unless it sits at the cap.
///
/// # Errors
///
/// Refused when the session sits at the cap.
pub(crate) fn admit_round(
    ledger: &DepthLedger,
    session_id: &str,
) -> Result<(), DepthRefusal> {
    let depth = ledger.depth_of(session_id);
    if admits(depth) {
        Ok(())
    } else {
        Err(DepthRefusal { depth })
    }
}

/// Admit a new child session under `session_id`: the depth the child
/// runs at, or the refusal when the parent sits at the cap.
///
/// The child runs one level below its parent: the counting happens
/// here, so no caller counts layers itself.
///
/// # Errors
///
/// Refused when the parent sits at the cap.
pub(crate) fn admit_child(
    ledger: &DepthLedger,
    session_id: &str,
) -> Result<usize, DepthRefusal> {
    let depth = ledger.depth_of(session_id);
    if !admits(depth) {
        return Err(DepthRefusal { depth });
    }
    Ok(depth + 1)
}

/// The accounting: the child session `child_id` runs at `depth`.
///
/// Both paths record the depth of the child they start, so a nested
/// `task` call reads its own depth out of the table instead of
/// starting over at the root.
pub(crate) fn record_child(ledger: &DepthLedger, child_id: &str, depth: usize) {
    ledger.record(child_id, depth);
}

/// The depth `session_id` runs at.
pub(crate) fn depth_of(ledger: &DepthLedger, session_id: &str) -> usize {
    ledger.depth_of(session_id)
}

/// Forget a session's depth: its handles are gone.
pub(crate) fn forget(ledger: &DepthLedger, session_id: &str) {
    ledger.forget(session_id);
}

#[cfg(test)]
mod tests;
