//! The session's checkpoints, over its one log.
//!
//! The `checkpoint`/`rewind` pair writes through this store: the
//! `checkpoint` tool's mark rides its own row (the `checkpoint`
//! member the harness's commit path lifts from the tool's report),
//! and the `rewind` tool's cut is one `user_message` row whose
//! `surface_op` **replaces** the exploration span with the report.
//!
//! # The cut
//!
//! A rewind replaces the folded surface **from the checkpoint on**:
//! the prefix up to the checkpoint stays, everything after it is
//! shadowed by the report. The positions and the shadowed seqs come
//! from one fold of the log — the same fold the next run builds its
//! transcript from — so what the tool cuts and what the next request
//! sees cannot disagree.
//!
//! The row carries `source: "rewind"`: the marker that tells the next
//! `checkpoint` read "the active checkpoint is closed", the same way
//! a `goal` row's latest state tells the continuation what to do.
//!
//! # The effect lands on the next run
//!
//! The fold applies the replace when the log is next read — the
//! *current* turn keeps the exploration it is still reasoning over.
//! That is the pair's own shape: the model calls `rewind` as its last
//! act, and the next turn (the user's next message, the goal
//! continuation) starts from `[…prefix…, report]`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use uva::{
    session::{
        REWIND_SOURCE,
        SurfaceOp,
        fold_log_surface,
        manager::SessionRegistry,
        typed_event_builders::rewind_row,
    },
    tool_checkpoint::{CHECKPOINT_NONE, RewindError, RewindStore},
};

/// The session's checkpoints, read and written through its log.
pub struct SessionRewinds {
    /// The deployment's sessions: the sink to read, the writer to
    /// append.
    ///
    /// Behind a slot because the sessions service is published by a
    /// row during the mount — the tools that hold this store exist
    /// before the service does.
    sessions: Arc<OnceLock<Arc<SessionRegistry>>>,
}

impl std::fmt::Debug for SessionRewinds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRewinds")
            .field("resolved", &self.sessions.get().is_some())
            .finish_non_exhaustive()
    }
}

impl SessionRewinds {
    /// The store over the slot the boot fills after the mount.
    #[must_use]
    pub fn new(sessions: Arc<OnceLock<Arc<SessionRegistry>>>) -> Self {
        Self { sessions }
    }

    /// The store over a registry that already exists (tests build one
    /// from a registry they own).
    #[cfg(test)]
    fn resolved(sessions: Arc<SessionRegistry>) -> Self {
        let slot = Arc::new(OnceLock::new());
        let _ = slot.set(sessions);
        Self { sessions: slot }
    }

    /// The sessions service, once the boot has filled the slot.
    fn sessions(&self) -> Option<&Arc<SessionRegistry>> {
        self.sessions.get()
    }

    /// The session's rows, when the registry knows the session.
    async fn rows(
        &self,
        session_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let sessions = self.sessions().ok_or_else(|| {
            "the sessions service is not available yet".to_string()
        })?;
        let session = sessions.get(session_id).await.ok_or_else(|| {
            format!("the session '{session_id}' is not known")
        })?;
        sessions
            .sink(&session.user_id, session_id)
            .read()
            .await
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
impl RewindStore for SessionRewinds {
    async fn checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<(u64, String)>, String> {
        let rows = self.rows(session_id).await?;
        // The newest of the two markers decides: a checkpoint row
        // opens one, a rewind row closes it.
        for row in rows.iter().rev() {
            if let Some(goal) = checkpoint_of(row) {
                let seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                return Ok(Some((seq, goal)));
            }
            if rewind_of(row) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    async fn rewind(
        &self,
        session_id: &str,
        report: &str,
    ) -> Result<(), RewindError> {
        let Some((checkpoint_seq, _goal)) = self
            .checkpoint(session_id)
            .await
            .map_err(RewindError::Failed)?
        else {
            return Err(CHECKPOINT_NONE);
        };
        let rows = self.rows(session_id).await.map_err(RewindError::Failed)?;
        // One fold answers both questions the cut needs: where the
        // checkpoint sits on the surface, and which rows the cut
        // shadows. The same fold builds the next run's transcript, so
        // the cut and the next request cannot disagree.
        // The lenient fold: a log whose replaces cannot be validated
        // still folds (the shadowed span replays), so the cut works
        // on every log this build can read.
        let surface = fold_log_surface(&rows);
        let Some(start) = surface
            .surface_seqs
            .iter()
            .position(|seq| *seq == checkpoint_seq)
        else {
            // The checkpoint's row is not on the surface — a
            // log-only row cannot be a surface boundary. The mark
            // rode a `tool_result` row, which is on it, so this is a
            // log this build cannot reconcile: refuse rather than
            // cut the wrong span.
            return Err(RewindError::Failed(format!(
                "the checkpoint row {checkpoint_seq} is not on the \
                 conversation's surface"
            )));
        };
        // The cut stops at the **current turn's prompt**: the turn
        // that is calling `rewind` is balanced (its call's result is
        // being written right now), and a cut that swallowed it would
        // leave a tool result with no call on the surface. omp
        // re-parents the sibling calls it cut; this keeps them whole.
        let Some(end) = surface
            .messages
            .iter()
            .rposition(|message| message["role"] == "user")
        else {
            return Err(RewindError::Failed(
                "the conversation holds no user turn; there is nothing \
                 to cut"
                    .to_string(),
            ));
        };
        if end <= start {
            return Err(RewindError::Failed(
                "the checkpoint is at or after this turn's start; there \
                 is nothing to cut"
                    .to_string(),
            ));
        }
        let shadowed = surface.surface_seqs[start..end].to_vec();

        let sessions = self
            .sessions()
            .ok_or_else(|| {
                RewindError::Failed(
                    "the sessions service is not available yet".to_string(),
                )
            })?
            .clone();
        let session = sessions.get(session_id).await.ok_or_else(|| {
            RewindError::Failed(format!(
                "the session '{session_id}' is not known"
            ))
        })?;
        let writer = sessions.writer(&session.user_id, session_id);
        let report_message = serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": report}],
        });
        let row = rewind_row(
            report_message,
            SurfaceOp::Replace {
                start,
                end,
                source_event_seqs: shadowed,
            },
        );
        // The marker rides the row's own source member.
        writer
            .append_event(&row)
            .await
            .map_err(|error| RewindError::Failed(error.to_string()))?;
        // The answer is a durability promise: the cut is on disk
        // before the tool returns.
        writer
            .flush()
            .await
            .map_err(|error| RewindError::Failed(error.to_string()))
    }
}

/// The checkpoint one row carries, when it carries one.
fn checkpoint_of(row: &serde_json::Value) -> Option<String> {
    let goal = row.get("checkpoint")?.as_str()?;
    (!goal.is_empty()).then(|| goal.to_string())
}

/// Whether this row is a rewind's marker.
fn rewind_of(row: &serde_json::Value) -> bool {
    row.get("source").and_then(|v| v.as_str()) == Some(REWIND_SOURCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with rows on disk, and its registry.
    async fn session_with_rows(
        rows: &[serde_json::Value],
    ) -> (Arc<SessionRegistry>, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp workspace");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let id = "s1".to_string();
        registry
            .create_with_user(id.clone(), "u".to_string())
            .await
            .expect("the session");
        let writer = registry.writer("u", &id);
        writer.append_copied(rows.to_vec()).await.expect("the rows");
        writer.flush().await.expect("the flush");
        (registry, id, dir)
    }

    /// A user row, the shape the writer stamps.
    fn user_row(seq: u64, text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "user_message",
            "seq": seq,
            "ts": "2026-11-27T10:00:00Z",
            "data": {
                "role": "user",
                "content": [{"type": "text", "text": text}],
            },
        })
    }

    /// A tool result row that carries a checkpoint mark.
    fn checkpoint_row(seq: u64, goal: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "tool_result",
            "seq": seq,
            "ts": "2026-11-27T10:00:01Z",
            "data": {"role": "tool", "content": [{"type": "text", "text": "Checkpoint."}]},
            "tool_call_id": "c1",
            "checkpoint": goal,
        })
    }

    #[tokio::test]
    async fn the_latest_marker_decides_the_checkpoint() {
        let rows = vec![
            user_row(1, "start"),
            checkpoint_row(2, "try the lock"),
            user_row(3, "explored"),
        ];
        let (registry, id, _dir) = session_with_rows(&rows).await;
        let store = SessionRewinds::resolved(registry);
        assert_eq!(
            store.checkpoint(&id).await,
            Ok(Some((2, "try the lock".to_string()))),
        );
    }

    #[tokio::test]
    async fn a_rewind_row_closes_the_checkpoint() {
        let rows = vec![
            user_row(1, "start"),
            checkpoint_row(2, "try the lock"),
            serde_json::json!({
                "type": "user_message",
                "seq": 3,
                "ts": "2026-11-27T10:00:02Z",
                "source": "rewind",
                "data": {"role": "user", "content": [{"type": "text", "text": "the lesson"}]},
            }),
        ];
        let (registry, id, _dir) = session_with_rows(&rows).await;
        let store = SessionRewinds::resolved(registry);
        assert_eq!(store.checkpoint(&id).await, Ok(None));
    }

    #[tokio::test]
    async fn a_rewind_without_a_checkpoint_is_refused() {
        let rows = vec![user_row(1, "start")];
        let (registry, id, _dir) = session_with_rows(&rows).await;
        let store = SessionRewinds::resolved(registry);
        assert_eq!(store.rewind(&id, "a report").await, Err(CHECKPOINT_NONE),);
    }

    #[tokio::test]
    async fn a_rewind_shadows_the_exploration_and_keeps_the_prefix() {
        // The shape production has: a checkpoint, the exploration it
        // opened, and the turn that calls `rewind` (whose prompt the
        // cut keeps whole — its call's result is being written).
        let rows = vec![
            user_row(1, "start"),
            checkpoint_row(2, "try the lock"),
            user_row(3, "explored and failed"),
            user_row(4, "cut it back"),
        ];
        let (registry, id, _dir) = session_with_rows(&rows).await;
        let store = SessionRewinds::resolved(Arc::clone(&registry));

        store
            .rewind(&id, "The lock deadlocks; the mutex is enough.")
            .await
            .expect("the rewind applies");

        // The next fold — the same one the next run builds its
        // transcript from — sees the prefix and the report, not the
        // exploration.
        let after = registry.sink("u", &id).read().await.expect("the rows");
        let surface = fold_log_surface(&after);
        // The folded messages carry the provider's `Content` enum,
        // which serialises `{"Single": {…}}` / `{"Multi": […]}`.
        let texts: Vec<String> = surface
            .messages
            .iter()
            .filter_map(|message| {
                let content = &message["content"];
                let single = content
                    .get("Single")
                    .and_then(|part| part["text"].as_str());
                let multi = content.get("Multi").and_then(|parts| {
                    parts.as_array().and_then(|parts| {
                        parts.iter().find_map(|part| part["text"].as_str())
                    })
                });
                single.or(multi).map(str::to_string)
            })
            .collect();
        assert!(
            texts.contains(&"start".to_string()),
            "the prefix stays: {texts:?}"
        );
        assert!(
            texts.contains(
                &"The lock deadlocks; the mutex is enough.".to_string()
            ),
            "the report stands in the exploration's place: {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("explored")),
            "the exploration is gone: {texts:?}"
        );
        assert!(
            texts.contains(&"cut it back".to_string()),
            "the rewinding turn stays whole: {texts:?}"
        );
        // The checkpoint is closed: the marker row is on the log.
        assert_eq!(store.checkpoint(&id).await, Ok(None));
    }
}
