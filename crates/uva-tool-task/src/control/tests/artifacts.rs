//! The artefact set: `<id>.{md,jsonl,patch}` in the child's session
//! directory, and the host seam that says where that is.

use std::sync::Arc;

use serde_json::{Value, json};
use uva_agent::AgentRegistry;
use uva_provider::Message;
use uva_session::{SessionEvent, subagent_enter, subagent_exit, user_message};

use super::{
    super::{
        ControlSurface,
        artifacts::{ARTIFACTS_DIR, RoundRecord, SESSION_LOG_FILE, write},
    },
    scan::FakeIndex,
};

/// The transcript a child session holds: the delegation header, the
/// dispatch prompt, a tool call and its result, the answer and the
/// footer — the rows the run itself would have written.
fn transcript() -> String {
    let rows: Vec<Value> = vec![
        serde_json::to_value(subagent_enter("child-1", "parent", 1, "scout"))
            .expect("the header serialises"),
        serde_json::to_value(user_message(
            serde_json::to_value(Message::user("map the repo"))
                .expect("the message serialises"),
            None,
        ))
        .expect("the user row serialises"),
        serde_json::to_value(SessionEvent::ToolCall {
            seq: 3,
            ts: "2026-01-01T00:00:02Z".to_string(),
            data: json!({
                "call_id": "c1",
                "name": "grep",
                "arguments": { "pattern": "lifecycle" },
            }),
            call_id: Some("c1".to_string()),
            name: Some("grep".to_string()),
        })
        .expect("the call row serialises"),
        serde_json::to_value(SessionEvent::ToolResult {
            seq: 4,
            ts: "2026-01-01T00:00:03Z".to_string(),
            data: serde_json::to_value(Message::tool("one hit", "c1"))
                .expect("the message serialises"),
            is_error: false,
            tool_call_id: Some("c1".to_string()),
            surface_op: None,
        })
        .expect("the result row serialises"),
        serde_json::to_value(SessionEvent::AssistantMessage {
            seq: 5,
            ts: "2026-01-01T00:00:04Z".to_string(),
            data: serde_json::to_value(Message::assistant("the answer"))
                .expect("the message serialises"),
            surface_op: None,
        })
        .expect("the answer row serialises"),
        serde_json::to_value(subagent_exit("child-1", 1, "completed"))
            .expect("the footer serialises"),
    ];
    let mut out = String::new();
    for row in rows {
        out.push_str(&row.to_string());
        out.push('\n');
    }
    out
}

/// A session directory holding [`transcript`].
fn session_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join(SESSION_LOG_FILE), transcript())
        .expect("the transcript");
    dir
}

/// The record a dispatched child writes.
fn record<'a>(patch: Option<String>) -> RoundRecord<'a> {
    RoundRecord {
        id: "child-1",
        agent: "scout",
        parent: "parent",
        depth: 1,
        status: "completed",
        patch,
    }
}

/// The set lands where the convention says, with the round's facts in
/// the markdown and the transcript frozen beside it.
#[test]
fn the_artefact_set_lands_in_the_sessions_artefacts_directory() {
    let dir = session_dir();
    let paths = write(dir.path(), &record(None)).expect("the set is written");

    let artifacts = dir.path().join(ARTIFACTS_DIR);
    assert_eq!(paths.markdown, artifacts.join("child-1.md"));
    assert_eq!(paths.transcript, artifacts.join("child-1.jsonl"));
    assert_eq!(paths.patch, None, "no isolated round, no patch");

    assert_eq!(
        std::fs::read_to_string(&paths.transcript).expect("the copy"),
        transcript(),
        "the .jsonl artefact is the child's transcript, unchanged"
    );

    let markdown =
        std::fs::read_to_string(&paths.markdown).expect("the rendering");
    assert!(markdown.starts_with("# Sub-agent `child-1`"), "{markdown}");
    for expected in [
        "| agent | `scout` |",
        "| parent session | `parent` |",
        "| round | completed |",
        "### User\n\nmap the repo",
        "### Tool call `grep`",
        "\"pattern\": \"lifecycle\"",
        "### Tool result",
        "one hit",
        "### Assistant\n\nthe answer",
        "> sub-agent `child-1` opened: agent `scout`, parent `parent`, \
         depth 1",
        "> sub-agent `child-1` closed: completed",
    ] {
        assert!(
            markdown.contains(expected),
            "missing {expected:?}:\n{markdown}"
        );
    }
}

/// An isolated round's diff is the third artefact; a round that
/// changed nothing leaves no `.patch` behind.
#[test]
fn a_patch_artefact_is_written_only_when_the_round_produced_one() {
    let dir = session_dir();
    let paths = write(
        dir.path(),
        &record(Some("diff --git a/x.rs b/x.rs\n".to_string())),
    )
    .expect("the set is written");
    let patch = paths.patch.expect("an isolated round has a patch");
    assert_eq!(patch, dir.path().join("artifacts/child-1.patch"));
    assert_eq!(
        std::fs::read_to_string(patch).expect("the patch"),
        "diff --git a/x.rs b/x.rs\n"
    );

    let dir = session_dir();
    let paths =
        write(dir.path(), &record(Some("   \n".to_string()))).expect("written");
    assert_eq!(paths.patch, None, "an empty patch is not a claim");
    assert!(!dir.path().join("artifacts/child-1.patch").exists());
}

/// A child without a durable session has no artefact set, and saying
/// so is an error the surface logs — never an invented directory.
#[test]
fn a_child_without_a_session_log_gets_no_artefacts() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let error = write(dir.path(), &record(None)).expect_err("no transcript");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(!dir.path().join(ARTIFACTS_DIR).exists());
}

/// The surface resolves the session directory through the host's
/// index: the tool never assumes a storage layout of its own.
#[test]
fn the_surface_writes_the_set_through_the_hosts_index() {
    let dir = session_dir();
    let session = dir.path().to_path_buf();
    let index = Arc::new(FakeIndex::new());
    index.with_session_dir("child-1", session.clone());
    let surface =
        ControlSurface::new(Arc::new(AgentRegistry::new())).with_index(index);

    surface.write_artifacts("parent", "child-1", "scout", 1, "completed", None);

    assert!(session.join(ARTIFACTS_DIR).join("child-1.md").is_file());
    assert!(session.join(ARTIFACTS_DIR).join("child-1.jsonl").is_file());
}

/// A host with no session directory writes nothing and stays quiet:
/// the round already happened.
#[test]
fn a_host_without_session_directories_writes_nothing() {
    let surface = ControlSurface::new(Arc::new(AgentRegistry::new()));
    surface.write_artifacts("parent", "child-1", "scout", 1, "completed", None);
}
