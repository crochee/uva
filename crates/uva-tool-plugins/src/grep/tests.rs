//! The `grep` tool's own tests: the matches it answers, and the
//! answers it refuses.

use std::{path::PathBuf, sync::Arc};

use serde_json::json;

use super::*;

/// A workspace with a few files, and the context that points at it.
fn workspace() -> (tempfile::TempDir, Context) {
    let dir = tempfile::tempdir().expect("a temp workspace");
    std::fs::create_dir_all(dir.path().join("src")).expect("the src dir");
    std::fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    // TODO: wire the tool\n}\n",
    )
    .expect("the file");
    std::fs::write(dir.path().join("src/lib.rs"), "pub fn helper() {}\n")
        .expect("the file");
    std::fs::write(dir.path().join("notes.md"), "TODO: write the notes\n")
        .expect("the file");
    let context = Context::new("s1".to_string(), dir.path().to_path_buf());
    (dir, context)
}

fn first_text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|c| c.text().map(str::to_string))
        .unwrap_or_default()
}

// ---- Tool metadata --------------------------------------------

#[test]
fn tool_uses_agent_facing_name() {
    assert_eq!(GrepTool::new().name(), "grep");
}

#[test]
fn search_is_read_only() {
    let a = GrepTool::new()
        .annotations()
        .expect("grep declares MCP-style annotations");
    assert_eq!(a.read_only_hint, Some(true));
    assert_eq!(a.destructive_hint, Some(false));
    assert_eq!(a.idempotent_hint, Some(true));
    assert_eq!(a.open_world_hint, Some(false));
}

// ---- The contract ----------------------------------------------

#[tokio::test]
async fn the_matches_come_back_as_path_line_text() {
    let (_dir, context) = workspace();
    let tool = GrepTool::new();

    let out = tool.call(json!({ "pattern": "TODO" }), &context).await;

    let text = first_text(&out);
    assert!(
        text.contains("src/main.rs:2:") && text.contains("TODO: wire the tool"),
        "the matches carry the path, the line and the text: {text}"
    );
    assert!(
        text.contains("notes.md:1:"),
        "every file's match comes back: {text}"
    );
}

#[tokio::test]
async fn the_include_filter_narrows_the_files() {
    let (_dir, context) = workspace();
    let tool = GrepTool::new();

    let out = tool
        .call(json!({ "pattern": "TODO", "include": "*.md" }), &context)
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("notes.md:1:") && !text.contains("main.rs"),
        "only the included files are searched: {text}"
    );
}

#[tokio::test]
async fn a_regular_expression_matches_what_it_should() {
    let (_dir, context) = workspace();
    let tool = GrepTool::new();

    let out = tool
        .call(json!({ "pattern": "fn\\s+\\w+\\(" }), &context)
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("src/main.rs:1:") && text.contains("src/lib.rs:1:"),
        "the expression matches both functions: {text}"
    );
}

#[tokio::test]
async fn a_pattern_that_matches_nothing_says_so() {
    let (_dir, context) = workspace();
    let tool = GrepTool::new();

    let out = tool
        .call(json!({ "pattern": "zzz-not-here" }), &context)
        .await;

    assert!(
        first_text(&out).contains("No line matches"),
        "the answer says so: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn an_invalid_regular_expression_is_refused() {
    let (_dir, context) = workspace();
    let tool = GrepTool::new();

    let out = tool.call(json!({ "pattern": "fn (" }), &context).await;

    assert!(
        first_text(&out).contains("not a valid regular expression"),
        "the refusal names the problem: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn a_binary_file_is_skipped() {
    let (_dir, context) = workspace();
    std::fs::write(
        context.workspace_root.join("blob.bin"),
        [0u8, 1, 2, 3, 0, 255],
    )
    .expect("the binary file");
    let tool = GrepTool::new();

    let out = tool.call(json!({ "pattern": "fn" }), &context).await;

    let text = first_text(&out);
    assert!(
        !text.contains("blob.bin"),
        "a binary file is not text: {text}"
    );
}

/// A client-owned workspace cannot be listed: the ACP file methods
/// read and write named files, they do not enumerate a directory.
#[tokio::test]
async fn a_client_owned_workspace_is_refused() {
    struct ClientOwned;
    #[async_trait]
    impl FileSystemOwner for ClientOwned {
        fn session_root(&self, _session_id: &str) -> Option<PathBuf> {
            Some(PathBuf::from("/client"))
        }

        async fn read_file(
            &self,
            _session_id: &str,
            _path: &std::path::Path,
        ) -> Result<Vec<u8>, crate::files::FileError> {
            Ok(Vec::new())
        }

        async fn write_file(
            &self,
            _session_id: &str,
            _path: &std::path::Path,
            _content: &[u8],
            _create_directories: bool,
        ) -> Result<(), crate::files::FileError> {
            Ok(())
        }
    }
    let tool =
        GrepTool::with_owner(Arc::new(ClientOwned) as Arc<dyn FileSystemOwner>);
    let context = Context::new("s1".to_string(), PathBuf::from("/tmp"));

    let out = tool.call(json!({ "pattern": "x" }), &context).await;

    assert!(
        first_text(&out).contains("does not offer a directory listing"),
        "the answer says why it cannot see the files: {}",
        first_text(&out)
    );
}
