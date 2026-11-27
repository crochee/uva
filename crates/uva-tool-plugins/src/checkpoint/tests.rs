//! The checkpoint pair's own tests: the boundary contract, the
//! refusals, and the store seam both tools call through.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;

/// A store the tests script: it answers with a fixed checkpoint and
/// remembers the rewinds it was asked to apply.
#[derive(Default)]
struct ScriptedStore {
    checkpoint: Mutex<Option<(u64, String)>>,
    rewinds: Mutex<Vec<(String, String)>>,
    fail: Mutex<Option<String>>,
}

impl ScriptedStore {
    fn with_checkpoint(seq: u64, goal: &str) -> Self {
        Self {
            checkpoint: Mutex::new(Some((seq, goal.to_string()))),
            ..Self::default()
        }
    }
}

#[async_trait]
impl RewindStore for ScriptedStore {
    async fn checkpoint(
        &self,
        _session_id: &str,
    ) -> Result<Option<(u64, String)>, String> {
        if let Some(failure) = self.fail.lock().expect("fail mutex").clone() {
            return Err(failure);
        }
        Ok(self.checkpoint.lock().expect("checkpoint mutex").clone())
    }

    async fn rewind(
        &self,
        session_id: &str,
        report: &str,
    ) -> Result<(), RewindError> {
        if let Some(failure) = self.fail.lock().expect("fail mutex").clone() {
            return Err(RewindError::Failed(failure));
        }
        if self.checkpoint.lock().expect("checkpoint mutex").is_none() {
            return Err(CHECKPOINT_NONE);
        }
        self.rewinds
            .lock()
            .expect("rewinds mutex")
            .push((session_id.to_string(), report.to_string()));
        *self.checkpoint.lock().expect("checkpoint mutex") = None;
        Ok(())
    }
}

/// The store as the seam's trait object, keeping the concrete
/// handle for the assertions.
fn store_of(store: &Arc<ScriptedStore>) -> Arc<dyn RewindStore> {
    store.clone()
}

fn make_context() -> Context {
    Context::new("s1".to_string(), std::path::PathBuf::from("/tmp"))
}

fn first_text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default()
}

// ---- Tool metadata ---------------------------------------------

#[test]
fn the_pair_is_named_for_the_model() {
    let store = Arc::new(ScriptedStore::default());
    assert_eq!(CheckpointTool::new(store_of(&store)).name(), "checkpoint");
    assert_eq!(RewindTool::new(store_of(&store)).name(), "rewind");
}

#[test]
fn the_pair_runs_alone() {
    let store = Arc::new(ScriptedStore::default());
    assert_eq!(
        CheckpointTool::new(store_of(&store)).mode(),
        ExecutionMode::Sequential
    );
    assert_eq!(
        RewindTool::new(store_of(&store)).mode(),
        ExecutionMode::Sequential
    );
}

// ---- checkpoint ------------------------------------------------

#[tokio::test]
async fn a_checkpoint_marks_and_reports_the_mark() {
    let store = Arc::new(ScriptedStore::default());
    let tool = CheckpointTool::new(store_of(&store));
    let out = tool
        .call(json!({"goal": "try the redis lock"}), &make_context())
        .await;
    assert!(out.is_error.is_none(), "{:?}", out.content);
    // The mark rides the row: the harness's commit path lifts this
    // metadata into the tool result's `checkpoint` member.
    assert_eq!(
        out.metadata.get("checkpoint"),
        Some(&json!({"objective": "try the redis lock"})),
        "the mark is the durable fact: {:?}",
        out.metadata
    );
    let text = first_text(&out);
    assert!(
        text.contains("Checkpoint: try the redis lock"),
        "the answer tells the model what it marked: {text}"
    );
}

#[tokio::test]
async fn a_second_checkpoint_while_one_is_active_is_refused() {
    let store = Arc::new(ScriptedStore::with_checkpoint(7, "the active one"));
    let tool = CheckpointTool::new(store_of(&store));
    let out = tool.call(json!({"goal": "another"}), &make_context()).await;
    assert_eq!(out.is_error, Some(true));
    let text = first_text(&out);
    assert!(
        text.contains("already active") && text.contains("the active one"),
        "the refusal names the active checkpoint: {text}"
    );
    assert!(
        out.metadata.get("checkpoint").is_none(),
        "a refused call marks nothing"
    );
}

// ---- rewind ----------------------------------------------------

#[tokio::test]
async fn a_rewind_applies_through_the_store() {
    let store = Arc::new(ScriptedStore::with_checkpoint(7, "the approach"));
    let tool = RewindTool::new(store_of(&store));
    let out = tool
        .call(
            json!({"report": "The redis lock deadlocks; the mutex is enough."}),
            &make_context(),
        )
        .await;
    assert!(out.is_error.is_none(), "{:?}", out.content);
    let rewinds = store.rewinds.lock().expect("rewinds mutex");
    assert_eq!(
        rewinds.as_slice(),
        [(
            "s1".to_string(),
            "The redis lock deadlocks; the mutex is enough.".to_string()
        )],
        "the rewind reached the store with the session and the report"
    );
}

#[tokio::test]
async fn a_rewind_without_a_checkpoint_says_so() {
    let tool = RewindTool::new(Arc::new(ScriptedStore::default()));
    let out = tool
        .call(json!({"report": "a report"}), &make_context())
        .await;
    assert_eq!(out.is_error, Some(true));
    assert!(
        first_text(&out).contains("no checkpoint"),
        "the refusal names the reason"
    );
}

// ---- the closed default ----------------------------------------

#[tokio::test]
async fn without_a_store_the_pair_fails_closed() {
    let checkpoint = CheckpointTool::new(Arc::new(NoRewinds));
    let out = checkpoint
        .call(json!({"goal": "try"}), &make_context())
        .await;
    assert_eq!(
        out.is_error,
        Some(true),
        "a checkpoint nothing will honour is refused"
    );

    let rewind = RewindTool::new(Arc::new(NoRewinds));
    let out = rewind
        .call(json!({"report": "a report"}), &make_context())
        .await;
    assert_eq!(out.is_error, Some(true));
}

// ---- malformed arguments ---------------------------------------

#[tokio::test]
async fn malformed_arguments_answer_with_the_reason() {
    let store = Arc::new(ScriptedStore::default());
    let out = CheckpointTool::new(store_of(&store))
        .call(json!({"goal": ""}), &make_context())
        .await;
    assert_eq!(out.is_error, Some(true));
    let out = RewindTool::new(store_of(&store))
        .call(json!({"report": 3}), &make_context())
        .await;
    assert_eq!(out.is_error, Some(true));
}
