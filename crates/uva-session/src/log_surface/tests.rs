use serde_json::{Value, json};

use super::{
    LogRow,
    classify_row,
    fold_log_surface,
    log_is_stamped,
    log_tail_seq,
    surface_events_from_log,
    try_fold_log_surface,
};
use crate::{
    events::{KNOWN_SESSION_EVENT_TYPES, SessionEvent, SurfaceRole},
    jsonl::SessionMetadataHeader,
    surface::fold::{RowKind, row_view},
};

/// The sink's metadata header exactly as the writer emits it.
fn header() -> Value {
    serde_json::to_value(SessionMetadataHeader::fresh("s1"))
        .expect("the header serialises")
}

/// A log whose first line is the sink's metadata header, then the
/// same event rows as the headerless form.
fn headered(events: &[Value]) -> Vec<Value> {
    let mut rows = vec![header()];
    rows.extend_from_slice(events);
    rows
}

fn plain_events() -> Vec<Value> {
    vec![
        json!({"type": "UserInput", "data": {"text": "hello"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "hi"}}),
    ]
}

/// The header is not a row: it contributes no surface entry and,
/// critically, consumes no ordinal. Numbering slice positions
/// instead would put every folded seq one ahead of the
/// `stream_index` the sink assigned that same event.
#[test]
fn a_metadata_header_does_not_shift_surface_seqs() {
    let bare = fold_log_surface(&plain_events());
    let with_header = fold_log_surface(&headered(&plain_events()));

    assert_eq!(
        with_header.messages, bare.messages,
        "the header contributes nothing to the surface",
    );
    assert_eq!(
        with_header.surface_seqs,
        vec![1, 2],
        "seqs must count events, not slice positions",
    );
    assert_eq!(with_header.surface_seqs, bare.surface_seqs);
}

/// The row this build writes for a turn folds to exactly the
/// surface row the legacy dialect produced: a user message
/// carrying the turn's text. Readability of the old dialect is
/// the compat layer's whole job, and a new write must not change
/// what the model sees — not the payload, not the ordinal, not
/// the attachment refs (which ride the row for a transcript
/// reader the model never was).
#[test]
fn a_typed_user_message_folds_like_the_legacy_user_input_row() {
    let legacy = vec![json!({
        "type": "UserInput",
        "seq": 1,
        "ts": "2026-11-14T00:00:00Z",
        "data": {"text": "look at this"},
    })];
    let typed = vec![json!({
        "type": "user_message",
        "seq": 1,
        "ts": "2026-11-14T00:00:00Z",
        "data": uva_provider::Message::user("look at this"),
        "attachments": [{"kind": "image", "hash": "abc123"}],
        "surface_op": "append",
    })];
    let from_legacy = fold_log_surface(&legacy);
    let from_typed = fold_log_surface(&typed);
    assert_eq!(
        from_typed.messages, from_legacy.messages,
        "the model-visible row is the turn's text either way"
    );
    assert_eq!(from_typed.surface_seqs, from_legacy.surface_seqs);
    assert_eq!(
        from_typed.messages.len(),
        1,
        "the attachments are not a second surface row: {from_typed:?}"
    );
}

/// The strict and lenient folds must agree on numbering, or a
/// caller that switches between them (`try_fold_log_surface` is
/// the error-surfacing variant) would see different seqs for the
/// same log.
#[test]
fn strict_and_lenient_folds_agree_on_headered_seqs() {
    let rows = headered(&plain_events());
    let lenient = fold_log_surface(&rows);
    let strict = try_fold_log_surface(&rows).expect("the log folds");

    assert_eq!(strict.surface_seqs, vec![1, 2]);
    assert_eq!(strict.surface_seqs, lenient.surface_seqs);
    assert_eq!(strict.messages, lenient.messages);
}

/// The typed projection carries the same event-only ordinals, so
/// feeding it to `fold_surface` reproduces the raw fold.
#[test]
fn typed_projection_skips_the_header_ordinal() {
    let rows = headered(&plain_events());
    let events = surface_events_from_log(&rows);
    let seqs: Vec<u64> = events
        .iter()
        .map(|event| match event {
            crate::events::SessionEvent::UserMessage { seq, .. }
            | crate::events::SessionEvent::AssistantMessage { seq, .. }
            | crate::events::SessionEvent::ToolResult { seq, .. }
            | crate::events::SessionEvent::Compaction { seq, .. } => *seq,
            other => panic!("unexpected projection: {other:?}"),
        })
        .collect();

    assert_eq!(seqs, vec![1, 2]);
}

/// A compaction `Replace` cites the source seqs the durable
/// checkpoint resolved, and those come from the sink's own
/// numbering (header-excluded). The header therefore must not
/// shift the fold's `max_surface_seq`, or the provenance check
/// would be comparing two different seq spaces.
#[test]
fn compaction_provenance_resolves_identically_with_a_header() {
    let events = vec![
        json!({"type": "UserInput", "data": {"text": "hello"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "working"}}),
        json!({
            "type": "compaction",
            "ts": "2026-09-25T00:00:00Z",
            "data": {"summary": "condensed"},
            "surface_op": {
                "start": 1,
                "end": 2,
                "source_event_seqs": [2],
            },
        }),
    ];

    let bare = fold_log_surface(&events);
    let with_header = fold_log_surface(&headered(&events));

    assert_eq!(
        with_header.surface_seqs, bare.surface_seqs,
        "the header must not shift the compaction's own seq either",
    );
    assert_eq!(
        with_header.surface_seqs,
        vec![1, 3],
        "the replace leaves the user row at 1 and lands at 3",
    );
    assert_eq!(with_header.messages, bare.messages);
}

/// A stamped log carries each row's own ordinal, and the fold
/// must read it: two writers' rows can interleave in the file,
/// so the row's position is not its ordinal.
#[test]
fn a_stamped_log_uses_the_rows_own_ordinals() {
    let rows = vec![
        json!({"type": "Model", "seq": 7, "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "seq": 9, "data": {"type": "text", "text": "b"}}),
    ];
    let folded = fold_log_surface(&rows);
    assert_eq!(folded.surface_seqs, vec![7, 9]);
}

/// An old log carries no `seq` field at all: its fold stays
/// exactly what it was — the row's position among the event rows.
#[test]
fn an_unstamped_log_keeps_positional_ordinals() {
    let rows = vec![
        json!({"type": "Model", "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "b"}}),
    ];
    let folded = fold_log_surface(&rows);
    assert_eq!(folded.surface_seqs, vec![1, 2]);
}

/// A log that mixes stamped and unstamped rows falls back to
/// positional counting for every row: mixing the two schemes
/// row by row would produce duplicate ordinals.
#[test]
fn a_mixed_log_falls_back_to_positional_ordinals() {
    let rows = vec![
        json!({"type": "Model", "seq": 7, "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "b"}}),
    ];
    let folded = fold_log_surface(&rows);
    assert_eq!(
        folded.surface_seqs,
        vec![1, 2],
        "no mixing: one log, one numbering"
    );
}

/// The strict fold reads the same ordinals as the lenient one: a
/// caller that switches between them must not see a different
/// sequence space.
#[test]
fn the_strict_fold_reads_the_same_ordinals() {
    let rows = vec![
        json!({"type": "Model", "seq": 7, "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "seq": 9, "data": {"type": "text", "text": "b"}}),
    ];
    let strict = try_fold_log_surface(&rows).expect("the log folds");
    assert_eq!(strict.surface_seqs, vec![7, 9]);
    assert_eq!(strict.surface_seqs, fold_log_surface(&rows).surface_seqs);
}

/// The metadata header is not an event row: the stamped-log
/// decision skips it, so a headered stamped log is stamped and
/// the header consumes no ordinal.
#[test]
fn a_metadata_header_does_not_shift_a_stamped_log() {
    let events = vec![
        json!({"type": "Model", "seq": 7, "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "seq": 9, "data": {"type": "text", "text": "b"}}),
    ];
    let with_header = fold_log_surface(&headered(&events));
    assert_eq!(with_header.surface_seqs, vec![7, 9]);
}

/// The decision is per log, not per row — one log, one numbering.
#[test]
fn the_stamped_decision_is_per_log() {
    let stamped = json!({
        "type": "Model",
        "seq": 7,
        "data": {"type": "text", "text": "a"},
    });
    let unstamped = json!({
        "type": "Model",
        "data": {"type": "text", "text": "a"},
    });

    assert!(log_is_stamped(std::slice::from_ref(&stamped)));
    assert!(!log_is_stamped(std::slice::from_ref(&unstamped)));
    assert!(
        !log_is_stamped(&[stamped.clone(), unstamped.clone()]),
        "one unstamped row makes the whole log unstamped"
    );
    assert!(
        log_is_stamped(&headered(std::slice::from_ref(&stamped))),
        "the header is not an event row"
    );
    assert!(!log_is_stamped(&headered(std::slice::from_ref(&unstamped))));
}

#[test]
fn log_tail_seq_numbers_rows_like_the_fold() {
    let first = json!({"type": "UserInput", "data": {"text": "a"}, "seq": 1});
    let second = json!({"type": "Model", "data": {"text": "b"}, "seq": 2});
    assert_eq!(log_tail_seq(&[]), 0, "an empty log has tail 0");
    assert_eq!(log_tail_seq(std::slice::from_ref(&first)), 1);
    assert_eq!(log_tail_seq(&[first, second]), 2);
    assert_eq!(
        log_tail_seq(&headered(&[json!({
            "type": "UserInput",
            "data": {"text": "a"},
        })])),
        1,
        "an unstamped log counts event rows and the header is not one"
    );
}

/// The source of the event vocabulary — the file the doc markers
/// live in.
const EVENTS_SOURCE: &str = include_str!("../events/mod.rs");

/// The role each doc marker spells, in the words the markers use.
///
/// The markers are the human-readable half of the role; this table
/// is the parser's half of the same vocabulary, and the test below
/// is what holds the two together.
const MARKERS: &[(&str, SurfaceRole)] = &[
    ("surface message", SurfaceRole::Message),
    ("surface op only", SurfaceRole::OpOnly),
    ("log-only", SurfaceRole::LogOnly),
];

/// A provider message in its wire shape.
fn message(message: uva_provider::Message) -> Value {
    serde_json::to_value(message).expect("a provider message serialises")
}

/// One raw row per `SessionEvent` variant: the variant's name, a
/// row of that kind, and the kind the row must fold to.
///
/// The row is the fixture three times over: it deserialises into the
/// typed event (so the declared role can be read), it is the raw row
/// [`classify_row`] reads, and it carries the wire tag the two must
/// agree on. The expected kind is the fourth fact this table states:
/// which kind of surface row the variant is — `None` for a log-only
/// row, which reaches no surface and so has no kind.
fn variant_rows() -> Vec<(&'static str, Value, Option<RowKind>)> {
    let user = message(uva_provider::Message::user("hi"));
    let assistant = message(uva_provider::Message::assistant("hi"));
    let tool = message(uva_provider::Message::tool(
        uva_provider::Content::text("done"),
        "call-1",
    ));
    vec![
        (
            "UserMessage",
            json!({"type": "user_message", "seq": 1, "ts": "t", "data": user}),
            Some(RowKind::User),
        ),
        (
            "AssistantMessage",
            json!({
                "type": "assistant_message",
                "seq": 1,
                "ts": "t",
                "data": assistant,
            }),
            Some(RowKind::Assistant),
        ),
        (
            "AssistantChunk",
            json!({
                "type": "assistant_chunk",
                "seq": 1,
                "ts": "t",
                "chunk_seq": 0,
                "data": {},
            }),
            None,
        ),
        (
            "ToolResult",
            json!({"type": "tool_result", "seq": 1, "ts": "t", "data": tool}),
            Some(RowKind::Tool),
        ),
        (
            "Compaction",
            json!({
                "type": "compaction",
                "seq": 1,
                "ts": "t",
                "data": {"summary": "s"},
                "surface_op": {"start": 0, "end": 0, "source_event_seqs": []},
            }),
            Some(RowKind::Compaction),
        ),
        (
            "CompactionStart",
            json!({
                "type": "compaction_start",
                "seq": 1,
                "ts": "t",
                "token": "tok",
                "data": {},
            }),
            None,
        ),
        (
            "CompactionSummary",
            json!({
                "type": "compaction_summary",
                "seq": 1,
                "ts": "t",
                "token": "tok",
                "data": {},
            }),
            None,
        ),
        (
            "CompactionEnd",
            json!({
                "type": "compaction_end",
                "seq": 1,
                "ts": "t",
                "token": "tok",
                "outcome": "committed",
                "data": {},
            }),
            None,
        ),
        (
            "ToolCall",
            json!({"type": "tool_call", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "ToolOutput",
            json!({
                "type": "tool_output",
                "seq": 1,
                "ts": "t",
                "call_id": "c",
                "tool": "shell",
                "data": {"output": "half"},
            }),
            None,
        ),
        (
            "Step",
            json!({"type": "step", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "Turn",
            json!({"type": "turn", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "Iteration",
            json!({"type": "iteration", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "Warning",
            json!({"type": "warning", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "SteeringGuard",
            json!({"type": "steering_guard", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "SteeringHint",
            json!({"type": "steering_hint", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "HookBlock",
            json!({"type": "hook_block", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "SubagentEnter",
            json!({"type": "subagent_enter", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "SubagentExit",
            json!({"type": "subagent_exit", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "RequestHeader",
            json!({"type": "request_header", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "ApprovalAsked",
            json!({
                "type": "approval_asked",
                "seq": 1,
                "ts": "t",
                "id": "i",
                "call_id": "c",
                "tool": "t",
                "arguments_hash": "h",
                "risk": "low",
                "agent": "a",
            }),
            None,
        ),
        (
            "ApprovalDecided",
            json!({
                "type": "approval_decided",
                "seq": 1,
                "ts": "t",
                "id": "i",
                "call_id": "c",
                "status": "approved",
                "by": "operator",
            }),
            None,
        ),
        (
            "ApprovalGranted",
            json!({
                "type": "approval_granted",
                "seq": 1,
                "ts": "t",
                "id": "i",
                "tool": "t",
                "scope": "session",
                "workspace_hash": "h",
            }),
            None,
        ),
        (
            "ToolDispatch",
            json!({
                "type": "tool_dispatch",
                "seq": 1,
                "ts": "t",
                "call_id": "c",
                "tool": "t",
                "arguments_hash": "h",
            }),
            None,
        ),
        (
            "RequestSnapshot",
            json!({
                "type": "request_snapshot",
                "seq": 1,
                "ts": "t",
                "provider": "p",
                "model": "m",
                "params_hash": "h",
                "tools_hash": "h",
                "system_hash": "h",
                "sections": [],
                "policy_hash": "h",
            }),
            None,
        ),
        (
            "Usage",
            json!({"type": "usage", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "SandboxMode",
            json!({"type": "sandbox_mode", "seq": 1, "ts": "t", "mode": "read-only"}),
            None,
        ),
        (
            "SessionShutdown",
            json!({"type": "session_shutdown", "seq": 1, "ts": "t", "data": {}}),
            None,
        ),
        (
            "ToolResultTransformed",
            json!({
                "type": "tool_result_transformed",
                "seq": 1,
                "ts": "t",
                "transformers": [],
                "tool_name": "t",
                "call_id": "c",
                "before": "b",
                "after": "a",
            }),
            None,
        ),
        (
            "ParkedPrompt",
            json!({"type": "parked_prompt", "seq": 1, "ts": "t", "text": "hi"}),
            None,
        ),
        (
            "ParkConsumed",
            json!({"type": "park_consumed", "seq": 1, "ts": "t", "park_id": "p"}),
            None,
        ),
        (
            "Feedback",
            json!({
                "type": "feedback",
                "seq": 1,
                "ts": "t",
                "message_id": "m",
                "thumbs_up": true,
            }),
            None,
        ),
        (
            "Plugin",
            json!({"type": "plugin", "seq": 1, "ts": "t", "kind": "k", "payload": {}}),
            None,
        ),
    ]
}

/// The lines of the `SessionEvent` enum body.
fn enum_body(source: &str) -> Vec<&str> {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim_start() == "pub enum SessionEvent {")
        .expect("the event vocabulary declares its enum");
    let rest = &lines[start + 1..];
    let end = rest
        .iter()
        .position(|line| *line == "}")
        .expect("the event vocabulary's enum closes");
    rest[..end].to_vec()
}

/// The name of the variant a line declares, if it declares one.
fn variant_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("    ")?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() || !rest[end..].starts_with(" {") {
        return None;
    }
    Some(name)
}

/// The wire tag a variant's name derives.
///
/// The enum-level `rename_all = "snake_case"` is what turns a
/// variant's name into the tag it writes on the wire — the tag
/// [`SessionEvent::type_tag`] also states, and the tag every
/// hand-written list of the vocabulary has to agree with. Every
/// variant name in the enum is plain PascalCase (no digit or
/// acronym runs), so the per-character rule below is exactly
/// serde's.
fn variant_wire_tag(variant: &str) -> String {
    let mut tag = String::with_capacity(variant.len());
    for (i, ch) in variant.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                tag.push('_');
            }
            tag.push(ch.to_ascii_lowercase());
        } else {
            tag.push(ch);
        }
    }
    tag
}

/// The doc block that precedes one variant, in source order.
fn variant_doc<'a>(body: &'a [&'a str], variant: &str) -> Option<Vec<&'a str>> {
    let index = body
        .iter()
        .position(|line| variant_name(line) == Some(variant))?;
    let mut cursor = index;
    // The attributes sit between the doc block and the declaration.
    while cursor > 0 && body[cursor - 1].trim_start().starts_with("#[") {
        cursor -= 1;
    }
    let mut doc = Vec::new();
    while cursor > 0 && body[cursor - 1].trim_start().starts_with("///") {
        cursor -= 1;
        doc.push(body[cursor]);
    }
    doc.reverse();
    Some(doc)
}

/// The role a variant's doc marker spells.
fn doc_role(body: &[&str], variant: &str) -> Option<SurfaceRole> {
    let doc = variant_doc(body, variant)?;
    let summary = doc.first()?.trim_start();
    let marker = summary
        .rsplit_once("— ")
        .map(|(_, tail)| tail.trim().trim_end_matches('.'))?;
    MARKERS
        .iter()
        .find(|(text, _)| *text == marker)
        .map(|(_, role)| *role)
}

/// The role the raw-log classifier gives a row of that kind.
fn classified_role(row: &Value) -> SurfaceRole {
    match classify_row(row) {
        LogRow::Ignored => SurfaceRole::LogOnly,
        LogRow::Surface {
            kind: RowKind::Compaction,
            ..
        } => SurfaceRole::OpOnly,
        LogRow::Surface { .. } => SurfaceRole::Message,
    }
}

/// Every variant declares one surface role, and the places that
/// state it agree: the declared role, the raw-log classifier's
/// treatment of a row of that kind, the variant's doc marker, and
/// the kind the fold gives the row.
///
/// Those are several hand-written statements of one fact, and a
/// list that drifts is silent: a variant added to one of them and
/// not the others either hides a row the model should see or shows
/// it one it should not. The declared role is the one the compiler
/// enforces — a new variant does not compile until its role is
/// declared — so this test is what holds the other three to it.
///
/// The kind is checked first, because it is the one that decides
/// what the model sees: the fold skips a row it cannot place, so a
/// variant that claims a surface role but carries no kind is a row
/// the model never sees — the one failure a reader of the log
/// cannot notice. The kind's *identity* is pinned too, against the
/// table above: a user turn that folded to an assistant turn would
/// change the order the model sees, and the order is what the kind
/// decides.
#[test]
fn every_variant_declares_one_surface_role() {
    let body = enum_body(EVENTS_SOURCE);
    let rows = variant_rows();
    let declared: Vec<&str> =
        body.iter().filter_map(|line| variant_name(line)).collect();
    let covered: Vec<&str> = rows.iter().map(|(name, _, _)| *name).collect();
    assert_eq!(
        declared, covered,
        "every variant needs a fixture row: a variant with no row is a \
         variant nobody checked"
    );

    for (name, row, expected_kind) in &rows {
        let event = SessionEvent::from_value(row)
            .expect("the fixture row deserialises into its variant");
        assert_eq!(
            event.type_tag(),
            row.get("type")
                .and_then(Value::as_str)
                .expect("the fixture row names a type"),
            "{name}: the fixture row must carry the variant's wire tag"
        );
        let declared = event.surface_role();
        let data = row.get("data").cloned().unwrap_or(Value::Null);
        let kind = row_view(&event, &data).map(|(kind, _)| kind);
        match declared {
            SurfaceRole::Message | SurfaceRole::OpOnly => assert!(
                kind.is_some(),
                "{name}: a surface-eligible row must carry a kind — the \
                 fold skips a row it cannot place, so the model would \
                 never see it"
            ),
            SurfaceRole::LogOnly => assert!(
                kind.is_none(),
                "{name}: a log-only row must not carry a kind — it never \
                 reaches the model, and a kind is what puts a row there"
            ),
        }
        assert_eq!(
            kind, *expected_kind,
            "{name}: the fold's kind and the expected kind disagree — the \
             kind decides where the row lands on the model-visible surface"
        );
        // The payload and the op are decided by one match over the
        // variants; the role says which group a variant belongs to.
        match declared {
            SurfaceRole::Message | SurfaceRole::OpOnly => assert!(
                event.surface_payload().is_some(),
                "{name}: a surface-eligible row must carry a payload — the \
                 fold skips a row with no payload, so the model would never \
                 see it"
            ),
            SurfaceRole::LogOnly => {
                assert!(
                    event.surface_payload().is_none(),
                    "{name}: a log-only row must carry no payload"
                );
                assert!(
                    event.surface_op().is_none(),
                    "{name}: a log-only row must carry no op"
                );
            }
        }
        assert_eq!(
            declared,
            classified_role(row),
            "{name}: the raw-log classifier and the declared role disagree"
        );
        assert_eq!(
            declared,
            doc_role(&body, name).unwrap_or_else(|| {
                panic!("{name}: the variant states no surface role")
            }),
            "{name}: the doc marker and the declared role disagree"
        );
    }
}

/// The const names exactly the wire tags the enum's variants
/// produce — as sets, both ways.
///
/// `KNOWN_SESSION_EVENT_TYPES` is the public vocabulary: the doc
/// on it holds it out to the front-end codegen as the source of
/// truth, and `empty_event_of` builds a skeleton from it. Unlike
/// the other lists of the vocabulary, a variant added to the enum
/// and forgotten in the const compiled and passed every test —
/// the const's own test counted the entries, and a count does not
/// move when the enum grows. So this test compares the two *sets*:
/// the tags the variants' names derive under the enum's
/// `rename_all = "snake_case"` rule (the tags the rows carry
/// on the wire, and the tags
/// [`SessionEvent::type_tag`] states, held together by the
/// round-trip above) and the tags the const lists. A tag the const
/// forgets and a tag it invents both fail, and the message names
/// the tag — which is what makes the failure actionable rather
/// than a red count.
#[test]
fn the_known_event_types_are_exactly_the_enum_s_wire_tags() {
    let body = enum_body(EVENTS_SOURCE);
    let mut declared: Vec<String> = Vec::new();
    for line in &body {
        let Some(variant) = variant_name(line) else {
            continue;
        };
        let tag = variant_wire_tag(variant);
        assert!(
            !declared.contains(&tag),
            "{variant}: two variants derive the wire tag {tag}, so the \
             set below would collapse them"
        );
        declared.push(tag);
    }

    let known: Vec<&str> = KNOWN_SESSION_EVENT_TYPES.to_vec();
    let missing: Vec<&str> = declared
        .iter()
        .filter(|tag| !known.contains(&tag.as_str()))
        .map(String::as_str)
        .collect();
    assert!(
        missing.is_empty(),
        "KNOWN_SESSION_EVENT_TYPES is missing the wire tag(s) {} — a \
         tag the const does not name is a tag `empty_event_of` cannot \
         build and no consumer of the const can discover",
        missing.join(", ")
    );
    let extra: Vec<&str> = known
        .iter()
        .filter(|tag| !declared.iter().any(|d| *d == **tag))
        .copied()
        .collect();
    assert!(
        extra.is_empty(),
        "KNOWN_SESSION_EVENT_TYPES names the wire tag(s) {} that no \
         variant produces — a tag the enum cannot read back",
        extra.join(", ")
    );
}
