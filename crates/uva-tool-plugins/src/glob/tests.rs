//! The `glob` tool's own tests: the paths it answers, and the
//! answers it refuses.

use std::{path::PathBuf, sync::Arc};

use serde_json::json;

use super::*;

/// A workspace with a few files, and the context that points at it.
fn workspace() -> (tempfile::TempDir, Context) {
    let dir = tempfile::tempdir().expect("a temp workspace");
    std::fs::create_dir_all(dir.path().join("src")).expect("the src dir");
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}")
        .expect("the file");
    std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}")
        .expect("the file");
    std::fs::write(dir.path().join("README.md"), "readme").expect("the file");
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
    assert_eq!(GlobTool::new().name(), "glob");
}

#[test]
fn discovery_is_read_only() {
    let a = GlobTool::new()
        .annotations()
        .expect("glob declares MCP-style annotations");
    assert_eq!(a.read_only_hint, Some(true));
    assert_eq!(a.destructive_hint, Some(false));
    assert_eq!(a.idempotent_hint, Some(true));
    assert_eq!(a.open_world_hint, Some(false));
}

#[test]
fn parameters_require_the_pattern() {
    let params = GlobTool::new().parameters();
    let required = params["required"]
        .as_array()
        .expect("required should be a list");
    let labels: Vec<&str> =
        required.iter().filter_map(|v| v.as_str()).collect();
    assert!(labels.contains(&"pattern"));
}

// ---- The contract ----------------------------------------------

#[tokio::test]
async fn the_matching_paths_come_back_relative_to_the_root() {
    let (_dir, context) = workspace();
    let tool = GlobTool::new();

    let out = tool.call(json!({ "pattern": "**/*.rs" }), &context).await;

    let text = first_text(&out);
    assert!(
        text.contains("src/main.rs") && text.contains("src/lib.rs"),
        "the matching paths come back: {text}"
    );
    assert!(
        !text.contains("README.md"),
        "a file that does not match stays out: {text}"
    );
    assert!(
        text.lines().all(|line| !line.starts_with('/')),
        "the paths are relative: {text}"
    );
}

#[tokio::test]
async fn a_search_under_a_subdirectory_only_finds_that_subtree() {
    let (_dir, context) = workspace();
    let tool = GlobTool::new();

    let out = tool
        .call(json!({ "pattern": "*.rs", "path": "src" }), &context)
        .await;

    let text = first_text(&out);
    assert!(
        text.contains("main.rs") && text.contains("lib.rs"),
        "the subdirectory's files come back: {text}"
    );
}

#[tokio::test]
async fn a_pattern_that_matches_nothing_says_so() {
    let (_dir, context) = workspace();
    let tool = GlobTool::new();

    let out = tool.call(json!({ "pattern": "**/*.py" }), &context).await;

    assert!(
        first_text(&out).contains("No file matches"),
        "the answer says so: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn an_empty_pattern_is_refused() {
    let (_dir, context) = workspace();
    let tool = GlobTool::new();

    let out = tool.call(json!({ "pattern": "  " }), &context).await;

    assert!(
        first_text(&out).contains("`pattern` is empty"),
        "the refusal names the argument: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn a_path_outside_the_workspace_is_refused() {
    let (_dir, context) = workspace();
    let tool = GlobTool::new();

    let out = tool
        .call(json!({ "pattern": "*", "path": "../elsewhere" }), &context)
        .await;

    assert!(
        first_text(&out).to_lowercase().contains("outside"),
        "the guard refuses the path: {}",
        first_text(&out)
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
        GlobTool::with_owner(Arc::new(ClientOwned) as Arc<dyn FileSystemOwner>);
    let context = Context::new("s1".to_string(), PathBuf::from("/tmp"));

    let out = tool.call(json!({ "pattern": "*" }), &context).await;

    assert!(
        first_text(&out).contains("does not offer a directory listing"),
        "the answer says why it cannot see the files: {}",
        first_text(&out)
    );
}
