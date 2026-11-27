//! The `edit` tool's own tests: the exact-match contract, the
//! refusals that make a failed edit explainable, and the seam — the
//! bytes live wherever the owner says they do.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::json;

use super::*;

fn make_context(root: PathBuf) -> Context {
    Context::new("s1".to_string(), root)
}

fn first_text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|c| c.text().map(str::to_string))
        .unwrap_or_default()
}

/// A workspace with one file, and the context that points at it.
fn workspace(body: &str) -> (tempfile::TempDir, Context) {
    let dir = tempfile::tempdir().expect("a temp workspace");
    std::fs::write(dir.path().join("file.txt"), body).expect("the file");
    let context = make_context(dir.path().to_path_buf());
    (dir, context)
}

// ---- Tool metadata --------------------------------------------

#[test]
fn tool_uses_agent_facing_name() {
    assert_eq!(EditTool::new().name(), "edit");
}

/// The tool MUST advertise `Sequential`: an edit is a
/// read-modify-write, so two edits to one path in one LLM pass would
/// lose one of them — the same reason `write` with `mode = "append"`
/// runs sequentially.
#[test]
fn edit_is_sequential_so_two_calls_cannot_race_a_path() {
    assert_eq!(EditTool::new().mode(), ExecutionMode::Sequential);
}

#[test]
fn annotations_are_mcp_native_and_honest() {
    let a = EditTool::new()
        .annotations()
        .expect("edit declares MCP-style annotations");
    assert_eq!(a.read_only_hint, Some(false));
    // An edit replaces the text it was given and nothing else: it is
    // not a destructive update.
    assert_eq!(a.destructive_hint, Some(false));
    // The second run of the same call finds no old text: not
    // idempotent.
    assert_eq!(a.idempotent_hint, Some(false));
    assert_eq!(a.open_world_hint, Some(false));
}

#[test]
fn parameters_require_the_path_and_both_strings() {
    let params = EditTool::new().parameters();
    let required = params["required"]
        .as_array()
        .expect("required should be a list");
    let labels: Vec<&str> =
        required.iter().filter_map(|v| v.as_str()).collect();
    assert!(labels.contains(&"file_path"));
    assert!(labels.contains(&"old_string"));
    assert!(labels.contains(&"new_string"));
}

// ---- The contract ----------------------------------------------

#[tokio::test]
async fn an_exact_match_is_replaced_and_the_line_is_named() {
    let (dir, context) = workspace("alpha\nbeta\ngamma\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "beta",
                "new_string": "BETA",
            }),
            &context,
        )
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("line 2"),
        "the answer names the line it edited: {text}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).expect("the file"),
        "alpha\nBETA\ngamma\n"
    );
}

#[test]
fn the_call_names_the_file_it_will_touch() {
    let (dir, context) = workspace("alpha\nbeta\ngamma\n");
    let tool = EditTool::new();

    let locations = tool.locations(
        &json!({"file_path": "file.txt", "old_string": "beta", "new_string": "BETA"}),
        &context,
    );

    assert_eq!(locations.len(), 1, "one file, one location");
    assert_eq!(
        locations[0].path,
        dir.path().join("file.txt").to_str().unwrap(),
        "the location is the file the call resolves to"
    );
}

#[test]
fn a_call_outside_the_workspace_names_nothing() {
    let (_dir, context) = workspace("alpha\nbeta\ngamma\n");
    let tool = EditTool::new();

    let locations = tool.locations(
        &json!({"file_path": "../../secrets", "old_string": "x", "new_string": "y"}),
        &context,
    );

    assert!(
        locations.is_empty(),
        "a call the tool would refuse names no file: {locations:?}"
    );
}

#[tokio::test]
async fn the_change_is_reported_as_data() {
    let (dir, context) = workspace("alpha\nbeta\ngamma\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "beta",
                "new_string": "BETA",
            }),
            &context,
        )
        .await;

    let diff = FileDiff::from_output(&out).expect("the change is reported");
    assert_eq!(
        diff.path,
        dir.path().join("file.txt").to_str().unwrap(),
        "the report names the file that changed"
    );
    assert_eq!(
        diff.old_text.as_deref(),
        Some("alpha\nbeta\ngamma\n"),
        "the report carries the file's text before the edit"
    );
    assert_eq!(
        diff.new_text, "alpha\nBETA\ngamma\n",
        "the report carries the file's text after the edit"
    );
    assert!(!diff.truncated);
}

#[tokio::test]
async fn a_non_unique_match_is_refused_with_the_lines_it_appears_on() {
    let (_dir, context) = workspace("dup\nkeep\ndup\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "dup",
                "new_string": "other",
            }),
            &context,
        )
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("lines 1, 3"),
        "the refusal names both lines: {text}"
    );
    assert!(
        text.contains("replace_all"),
        "the refusal says how to proceed: {text}"
    );
}

#[tokio::test]
async fn replace_all_replaces_every_occurrence() {
    let (dir, context) = workspace("dup\nkeep\ndup\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "dup",
                "new_string": "other",
                "replace_all": true,
            }),
            &context,
        )
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("Replaced 2 occurrences"),
        "the answer counts the replacements: {text}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).expect("the file"),
        "other\nkeep\nother\n"
    );
}

#[tokio::test]
async fn an_empty_new_string_deletes_the_matched_text() {
    let (dir, context) = workspace("alpha\nbeta\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "alpha\n",
                "new_string": "",
            }),
            &context,
        )
        .await;

    assert!(!first_text(&out).contains("error"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt")).expect("the file"),
        "beta\n"
    );
}

#[tokio::test]
async fn a_miss_that_is_already_applied_says_so() {
    let (_dir, context) = workspace("alpha\nBETA\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "beta",
                "new_string": "BETA",
            }),
            &context,
        )
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("already applied"),
        "the refusal names the change that is already there: {text}"
    );
}

#[tokio::test]
async fn a_plain_miss_says_to_copy_the_exact_text() {
    let (_dir, context) = workspace("alpha\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "beta",
                "new_string": "BETA",
            }),
            &context,
        )
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("verbatim"),
        "the refusal says the text must match exactly: {text}"
    );
    assert!(
        text.contains("whitespace"),
        "the refusal says what usually differs: {text}"
    );
}

#[tokio::test]
async fn an_identical_replacement_is_refused_before_anything_is_read() {
    let (_dir, context) = workspace("alpha\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "alpha",
                "new_string": "alpha",
            }),
            &context,
        )
        .await;

    assert!(
        first_text(&out).contains("identical"),
        "an edit that changes nothing is refused: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn replace_all_over_whitespace_is_refused() {
    let (_dir, context) = workspace("alpha beta\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": " ",
                "new_string": "_",
                "replace_all": true,
            }),
            &context,
        )
        .await;

    assert!(
        first_text(&out).contains("whitespace-only"),
        "replacing every space is never what the model meant: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn a_missing_file_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let context = make_context(dir.path().to_path_buf());
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "absent.txt",
                "old_string": "a",
                "new_string": "b",
            }),
            &context,
        )
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("does not exist"),
        "the refusal names the missing file: {text}"
    );
}

#[tokio::test]
async fn a_path_outside_the_workspace_is_refused() {
    let (_dir, context) = workspace("alpha\n");
    let tool = EditTool::new();

    let out = tool
        .call(
            json!({
                "file_path": "../escape.txt",
                "old_string": "a",
                "new_string": "b",
            }),
            &context,
        )
        .await;

    assert!(
        first_text(&out).to_lowercase().contains("outside"),
        "the guard refuses the path before any read: {}",
        first_text(&out)
    );
}

// ---- The seam --------------------------------------------------

/// An owner that holds one file in memory: the deployment's disk is
/// not involved at all.
struct MemoryOwner {
    body: parking_lot::Mutex<String>,
    written: parking_lot::Mutex<usize>,
}

#[async_trait]
impl FileSystemOwner for MemoryOwner {
    fn session_root(&self, _session_id: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/client"))
    }

    async fn read_file(
        &self,
        _session_id: &str,
        _path: &Path,
    ) -> Result<Vec<u8>, FileError> {
        Ok(self.body.lock().clone().into_bytes())
    }

    async fn write_file(
        &self,
        _session_id: &str,
        _path: &Path,
        content: &[u8],
        _create_directories: bool,
    ) -> Result<(), FileError> {
        *self.written.lock() += 1;
        *self.body.lock() = String::from_utf8_lossy(content).into_owned();
        Ok(())
    }
}

#[tokio::test]
async fn the_edit_lands_in_the_owners_file_not_on_this_disk() {
    let owner = Arc::new(MemoryOwner {
        body: parking_lot::Mutex::new("alpha\nbeta\n".to_string()),
        written: parking_lot::Mutex::new(0),
    });
    let tool =
        EditTool::with_owner(Arc::clone(&owner) as Arc<dyn FileSystemOwner>);
    let context = make_context(PathBuf::from("/client"));

    let out = tool
        .call(
            json!({
                "file_path": "file.txt",
                "old_string": "beta",
                "new_string": "BETA",
            }),
            &context,
        )
        .await;

    assert!(!first_text(&out).contains("error"));
    assert_eq!(owner.body.lock().as_str(), "alpha\nBETA\n");
    assert_eq!(*owner.written.lock(), 1, "the owner wrote once");
}
