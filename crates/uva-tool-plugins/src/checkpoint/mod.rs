//! The `checkpoint` and `rewind` tools: explore, then cut back.
//!
//! omp ships this pair (`packages/coding-agent/src/tools/checkpoint.ts`):
//! a long task goes down a wrong path, and the cheapest recovery is not
//! to *undo* the files — it is to **drop the exploration from the
//! conversation** and keep what was learned:
//!
//! ```text
//! model: checkpoint({goal: "try the redis lock"})
//!   … many steps of exploration that did not work …
//! model: rewind({report: "The redis lock deadlocks under …; the
//!                      in-process mutex is enough because …"})
//!   → the surface from the checkpoint on is replaced by the report
//! ```
//!
//! The next request then carries `[…prefix…, report]` — the wrong
//! path is gone from the context, the lesson stays. This is the
//! *conversation* half of undo: files are not touched (what the
//! exploration wrote stays written; the report says what to do about
//! it).
//!
//! # The seam
//!
//! Where the checkpoint state lives and how a rewind is applied is the
//! host's business, not the tool's: the tool holds one
//! [`RewindStore`] — the same seam `goal` uses for its durable state.
//! The shipped `NoRewinds` answers "no checkpoint" and refuses every
//! rewind, so a deployment that mounts the bricks without a store
//! fails closed instead of marking boundaries nothing will honour.
//!
//! # The checkpoint is a boundary, not a snapshot
//!
//! The checkpoint marks **where** to cut; it does not copy anything.
//! The durable marker is the `checkpoint` tool's own row (its
//! `checkpoint` member — lifted by the harness's commit path the way
//! the file tools' `changed` member is), so the log answers "what was
//! the active checkpoint" on its own.

use std::sync::Arc;

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{Context, ExecutionMode, Tool, ToolOutput};

/// The `checkpoint` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::CheckpointToolPlugin;

#[cfg(test)]
mod tests;

/// Where the checkpoint state lives and how a rewind is applied.
///
/// The host installs the implementation; the tools never speak a
/// log. A rewind **replaces the surface** from the checkpoint on with
/// the report — the durable half of "explore, then cut back".
#[async_trait]
pub trait RewindStore: Send + Sync {
    /// The active checkpoint, if one is: its log seq (the boundary a
    /// rewind cuts back to) and its goal.
    ///
    /// # Errors
    ///
    /// The store's own failure; the string says why.
    async fn checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<(u64, String)>, String>;

    /// Replace the surface from the active checkpoint on with
    /// `report`.
    ///
    /// # Errors
    ///
    /// [`CHECKPOINT_NONE`] when no checkpoint is active — the store's
    /// own failure otherwise.
    async fn rewind(
        &self,
        session_id: &str,
        report: &str,
    ) -> Result<(), RewindError>;
}

/// Why a rewind did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindError {
    /// No checkpoint is active: there is no boundary to cut back to.
    NoCheckpoint,
    /// The store failed; the string says why.
    Failed(String),
}

/// The refusal every store uses for a rewind with no checkpoint.
pub const CHECKPOINT_NONE: RewindError = RewindError::NoCheckpoint;

impl std::fmt::Display for RewindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RewindError::NoCheckpoint => f.write_str(
                "no checkpoint is active; there is no boundary to cut \
                 back to",
            ),
            RewindError::Failed(message) => {
                write!(f, "the rewind failed: {message}")
            }
        }
    }
}

/// The store that answers nothing: the default a deployment has when
/// it mounts the bricks without a store.
///
/// A checkpoint nobody will honour is worse than no checkpoint: the
/// model would plan around a rewind that cannot come. Both tools
/// fail closed instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRewinds;

#[async_trait]
impl RewindStore for NoRewinds {
    async fn checkpoint(
        &self,
        _session_id: &str,
    ) -> Result<Option<(u64, String)>, String> {
        // The read refuses rather than answering "none": "no active
        // checkpoint" is a store's answer, and this deployment has no
        // store — a checkpoint it marks is a promise nothing will
        // honour.
        Err("no rewind store is configured; checkpoints cannot be              honoured here"
            .to_string())
    }

    async fn rewind(
        &self,
        _session_id: &str,
        _report: &str,
    ) -> Result<(), RewindError> {
        Err(CHECKPOINT_NONE)
    }
}

/// What `checkpoint` is asked.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct CheckpointRequest {
    #[schemars(
        length(min = 1, max = 500),
        description = "What you are about to try. One sentence."
    )]
    goal: String,
}

/// `checkpoint` — mark the boundary a later `rewind` cuts back to.
///
/// omp's shape: the checkpoint is **active** until the rewind; a
/// second checkpoint while one is active is refused (the exploration
/// that is already running has one boundary — its own start).
pub struct CheckpointTool {
    store: Arc<dyn RewindStore>,
}

impl std::fmt::Debug for CheckpointTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointTool").finish_non_exhaustive()
    }
}

impl CheckpointTool {
    /// The tool that marks through `store`.
    #[must_use]
    pub fn new(store: Arc<dyn RewindStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for CheckpointTool {
    fn name(&self) -> &str {
        "checkpoint"
    }

    fn description(&self) -> &str {
        "Mark the boundary a later `rewind` cuts back to. Use it \
         before trying a risky or speculative approach: if the \
         approach fails, call `rewind` with what you learned and the \
         conversation drops the exploration, keeping the lesson. You \
         MUST call `rewind` before the task is done — an active \
         checkpoint is a promise to report."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(CheckpointRequest))
            .expect("CheckpointRequest schema is always serializable")
    }

    /// A checkpoint decides what the rest of the turn means: it runs
    /// alone.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: CheckpointRequest = match serde_json::from_value(input) {
            Ok(request) => request,
            Err(error) => {
                return ToolOutput::error(format!(
                    "Invalid arguments: {error}"
                ));
            }
        };
        // The schema says one sentence at least; the runtime says it
        // too — a boundary with nothing on it marks nothing.
        if request.goal.trim().is_empty() {
            return ToolOutput::error(
                "Invalid arguments: the goal must not be blank",
            );
        }
        match self.store.checkpoint(&context.session_id).await {
            Ok(Some((_, active))) => ToolOutput::error(format!(
                "a checkpoint is already active ({active}); finish it \
                 with `rewind` before starting another"
            )),
            Ok(None) => {
                // The mark rides the row: the harness's commit path
                // lifts it into the tool result's `checkpoint`
                // member, so the log is the one place a later rewind
                // (or a restart) reads it from.
                ToolOutput::text(format!(
                    "Checkpoint: {}. Finish the exploration and report \
                     your findings with `rewind`.",
                    request.goal
                ))
                .with_metadata(
                    "checkpoint",
                    serde_json::json!({ "objective": request.goal }),
                )
            }
            Err(message) => ToolOutput::error(format!(
                "the checkpoint could not be read: {message}"
            )),
        }
    }
}

/// What `rewind` is asked.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct RewindRequest {
    #[schemars(
        length(min = 1),
        description = "What the exploration found. This replaces the \
                       exploration in the conversation: write what a \
                       fresh attempt needs to know."
    )]
    report: String,
}

/// `rewind` — cut the conversation back to the checkpoint, keeping
/// the report.
pub struct RewindTool {
    store: Arc<dyn RewindStore>,
}

impl std::fmt::Debug for RewindTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RewindTool").finish_non_exhaustive()
    }
}

impl RewindTool {
    /// The tool that rewinds through `store`.
    #[must_use]
    pub fn new(store: Arc<dyn RewindStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for RewindTool {
    fn name(&self) -> &str {
        "rewind"
    }

    fn description(&self) -> &str {
        "Cut the conversation back to the active `checkpoint`, \
         replacing the exploration with your report. The files are \
         NOT touched: what the exploration wrote stays written — say \
         what to do about it in the report. This ends the \
         checkpoint."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(RewindRequest))
            .expect("RewindRequest schema is always serializable")
    }

    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: RewindRequest = match serde_json::from_value(input) {
            Ok(request) => request,
            Err(error) => {
                return ToolOutput::error(format!(
                    "Invalid arguments: {error}"
                ));
            }
        };
        // A rewind with no report would replace the exploration with
        // nothing — the lesson is the point of the cut.
        if request.report.trim().is_empty() {
            return ToolOutput::error(
                "Invalid arguments: the report must not be blank",
            );
        }
        match self
            .store
            .rewind(&context.session_id, &request.report)
            .await
        {
            Ok(()) => ToolOutput::text(
                "Rewound. The exploration is gone from the \
                 conversation; your report stands in its place. The \
                 checkpoint is closed — start the next approach.",
            ),
            Err(error) => ToolOutput::error(error.to_string()),
        }
    }
}
