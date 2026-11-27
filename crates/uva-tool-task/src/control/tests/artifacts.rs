//! The artefact set: `<id>.{md,jsonl,patch}` in the child's session
//! directory, and the host seam that says where that is.

use std::sync::Arc;

use serde_json::{Value, json};
use uva_agent::AgentRegistry;
use uva_provider::Message;
use uva_session::{
    RowTag,
    SessionEvent,
    subagent_enter,
    subagent_exit,
    user_message,
};

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
            changed: None,
            checkpoint: None,
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
fn record<'a>(patch: Option<&'a str>) -> RoundRecord<'a> {
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
    let paths = write(dir.path(), &record(Some("diff --git a/x.rs b/x.rs\n")))
        .expect("the set is written");
    let patch = paths.patch.expect("an isolated round has a patch");
    assert_eq!(patch, dir.path().join("artifacts/child-1.patch"));
    assert_eq!(
        std::fs::read_to_string(patch).expect("the patch"),
        "diff --git a/x.rs b/x.rs\n"
    );

    let dir = session_dir();
    let paths = write(dir.path(), &record(Some("   \n"))).expect("written");
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

/// The tags the artefact set renders: the child's own conversation —
/// who spoke, which tools ran, how the round ended. Every other tag
/// in the vocabulary is left out on purpose.
const RENDERED: [RowTag; 9] = [
    RowTag::UserMessage,
    RowTag::AssistantMessage,
    RowTag::ToolCall,
    RowTag::ToolResult,
    RowTag::SubagentEnter,
    RowTag::SubagentExit,
    RowTag::Turn,
    RowTag::Warning,
    RowTag::HookBlock,
];

/// Every tag in the vocabulary, read from the vocabulary's own
/// published schema — the same source the wire contract derives its
/// union from. The test keeps no list of its own: a tag added to the
/// vocabulary arrives here on its own.
fn vocabulary() -> Vec<RowTag> {
    let schema = schemars::schema_for!(uva_session::RowTag);
    let variants = schema
        .as_object()
        .and_then(|object| object.get("oneOf"))
        .and_then(Value::as_array)
        .expect("the vocabulary publishes a closed union of tags");
    variants
        .iter()
        .map(|variant| {
            let wire = variant
                .get("const")
                .and_then(Value::as_str)
                .expect("every variant of the union is one tag");
            RowTag::from_wire(wire).expect("the union holds vocabulary tags")
        })
        .collect()
}

/// The artefact set's rendering of a transcript holding exactly these
/// rows.
fn rendering_of(tags: &[RowTag]) -> String {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut transcript = String::new();
    for tag in tags {
        let row = serde_json::to_value(tag.empty_event())
            .expect("the row serialises");
        transcript.push_str(&row.to_string());
        transcript.push('\n');
    }
    std::fs::write(dir.path().join(SESSION_LOG_FILE), transcript)
        .expect("the transcript");
    let paths = write(dir.path(), &record(None)).expect("the set is written");
    std::fs::read_to_string(&paths.markdown).expect("the rendering")
}

/// The artefact set's rendering of a transcript holding exactly these
/// raw rows.
fn rendering_of_rows(rows: &[Value]) -> String {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut transcript = String::new();
    for row in rows {
        transcript.push_str(&row.to_string());
        transcript.push('\n');
    }
    std::fs::write(dir.path().join(SESSION_LOG_FILE), transcript)
        .expect("the transcript");
    let paths = write(dir.path(), &record(None)).expect("the set is written");
    std::fs::read_to_string(&paths.markdown).expect("the rendering")
}

/// A row the current vocabulary does not cover is dropped, not
/// rendered: a tag a newer build wrote (version skew), and a row with
/// no usable tag at all. A row written in an old dialect is not one
/// of these — it is restored into the current vocabulary first, and
/// rendered in current shape.
#[test]
fn a_row_outside_the_vocabulary_is_dropped() {
    let rows = [
        json!({ "type": "a_tag_this_build_predates", "seq": 1, "data": {} }),
        json!({ "seq": 1, "data": { "text": "hi" } }),
    ];
    let empty = rendering_of_rows(&[]);
    for row in rows {
        let rendering = rendering_of_rows(std::slice::from_ref(&row));
        assert_eq!(
            rendering, empty,
            "a row outside the vocabulary reached the rendering: {row}"
        );
    }
}

/// A row written in an old dialect is restored into the current
/// vocabulary before it is rendered, so the set shows what the round
/// did rather than which spelling the log happened to use: a legacy
/// user turn reads as a user turn.
#[test]
fn a_legacy_row_is_restored_and_rendered() {
    let row =
        json!({ "type": "UserInput", "seq": 1, "data": { "text": "hi" } });
    let rendering = rendering_of_rows(std::slice::from_ref(&row));
    assert!(
        rendering.contains("### User\n\nhi"),
        "the restored row did not reach the rendering:\n{rendering}"
    );
}

/// The artefact set takes a stance on every tag in the vocabulary: the
/// tags the transcript is made of are rendered, and the rest are left
/// out on purpose. A tag nobody decided about would render nothing
/// and read as «nothing happened» — this test says every tag is
/// accounted for. The renderer's own match over the vocabulary says
/// the same thing at compile time; this one says it about the
/// rendering that actually comes out.
#[test]
fn every_tag_in_the_vocabulary_is_rendered_or_left_out_on_purpose() {
    let vocabulary = vocabulary();
    for tag in RENDERED {
        assert!(
            vocabulary.contains(&tag),
            "`{}` is not a tag in the vocabulary",
            tag.as_wire(),
        );
    }

    let empty = rendering_of(&[]);
    for tag in vocabulary {
        let rendered = rendering_of(&[tag]) != empty;
        assert_eq!(
            rendered,
            RENDERED.contains(&tag),
            "the artefact set's stance on `{}` drifted: it {}",
            tag.as_wire(),
            if rendered {
                "renders the row"
            } else {
                "leaves the row out"
            },
        );
    }
}
