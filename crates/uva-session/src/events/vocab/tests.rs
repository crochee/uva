//! Interface tests for the row vocabulary. Everything here goes
//! through the module's public face: the two closed vocabularies
//! and what they declare — never the events enum's private
//! restatements. The 归一 (restore) side has its own suite, in
//! [`restore::tests`](super::restore::tests).

use serde_json::json;

use super::*;

/// Every tag writes to the wire and reads back: `as_wire` is the
/// derived (snake_case) spelling, and `from_wire` accepts exactly
/// that. The two hand-written accessors cannot drift from the serde
/// naming rule without this failing for the drifted tag.
#[test]
fn every_tag_writes_to_the_wire_and_reads_back() {
    for tag in RowTag::ALL {
        let wire = serde_json::to_value(tag).expect("a tag serialises");
        assert_eq!(wire, json!(tag.as_wire()), "{tag:?}");
        assert_eq!(
            RowTag::from_wire(tag.as_wire()),
            Some(tag),
            "{} reads back",
            tag.as_wire(),
        );
    }
}

/// The published schema is the closed union the wire contract
/// derives from: one `oneOf` arm per tag, each a `const` of the
/// wire string. A tag without its const here is a tag the contract
/// silently drops.
#[test]
fn the_schema_publishes_the_tags_as_a_one_of_of_consts() {
    let settings = schemars::generate::SchemaSettings::draft07();
    let root = settings.into_generator().into_root_schema_for::<RowTag>();
    let schema = serde_json::to_value(root).expect("the schema is JSON");
    let arms = schema["oneOf"]
        .as_array()
        .expect("a closed vocabulary is a oneOf of consts");
    let consts: Vec<&str> = arms
        .iter()
        .filter_map(|arm| arm["const"].as_str())
        .collect();
    let expected: Vec<&str> =
        RowTag::ALL.iter().map(|tag| tag.as_wire()).collect();
    assert_eq!(
        consts.len(),
        expected.len(),
        "one arm per tag, none carrying more than a const"
    );
    assert_eq!(consts, expected, "the wire tags, in declaration order");
}

/// Every dialect round-trips, and the two vocabularies never
/// collide: a dialect's wire string is never a tag and a tag's wire
/// string is never a dialect.
#[test]
fn every_dialect_round_trips_and_never_collides_with_a_tag() {
    for dialect in [
        RowDialect::UserInput,
        RowDialect::Model,
        RowDialect::Message,
        RowDialect::Agent,
    ] {
        let wire = serde_json::to_value(dialect).expect("a dialect serialises");
        assert_eq!(wire, json!(dialect.as_wire()), "{dialect:?}");
        assert_eq!(
            RowDialect::from_wire(dialect.as_wire()),
            Some(dialect),
            "{} reads back",
            dialect.as_wire(),
        );
        assert_eq!(
            RowTag::from_wire(dialect.as_wire()),
            None,
            "{} is a dialect, not a tag",
            dialect.as_wire(),
        );
    }
}

/// Strings outside both vocabularies read as neither — including
/// the near-misses a hand-spelled match silently accepts
/// (`UserMessage`, the CamelCase spelling; `user_input`, the
/// dialect mistaken for a tag).
#[test]
fn strings_outside_both_vocabularies_read_as_neither() {
    let strays = [
        "UserMessage",
        "user_input",
        "USER_MESSAGE",
        "user-message",
        "user message",
        "",
        "Agent2",
        "user_message ",
    ];
    for stray in strays {
        assert_eq!(RowTag::from_wire(stray), None, "{stray:?}");
        assert_eq!(RowDialect::from_wire(stray), None, "{stray:?}");
    }
}

/// The vocabulary's size is a deliberate contract: adding or
/// removing a row tag is a wire change, never silent growth.
#[test]
fn the_wire_vocabulary_has_thirty_three_tags() {
    assert_eq!(RowTag::ALL.len(), 33, "thirty-three tags, no drift");
}

/// THE tie that pins both directions of the correspondence: for
/// every tag, the tag-side constructor builds the event whose
/// event-side answer is that same tag, on the wire and through
/// both accessors. Swap one pairing in either exhaustive match —
/// `SessionEvent::tag` or `RowTag::empty_event` — and this fails
/// for exactly the swapped tag.
#[test]
fn event_and_tag_declare_the_same_correspondence() {
    for tag in RowTag::ALL {
        let skeleton = tag.empty_event();
        assert_eq!(skeleton.tag(), tag, "{}", tag.as_wire());
        assert_eq!(
            skeleton.tag().as_wire(),
            tag.as_wire(),
            "{}",
            tag.as_wire()
        );
        let written =
            serde_json::to_value(&skeleton).expect("a skeleton serialises");
        assert_eq!(written["type"], json!(tag.as_wire()), "{}", tag.as_wire());
    }
}

/// Real events — not skeletons — answer with their own tag: the
/// correspondence is about rows on the wire, not just the
/// constructors.
#[test]
fn a_built_event_answers_with_its_own_tag() {
    use crate::typed_event_builders::{sandbox_mode, step_start, tool_output};
    assert_eq!(step_start(0, 0).tag(), RowTag::Step);
    assert_eq!(tool_output("c1", "shell", "x").tag(), RowTag::ToolOutput);
    assert_eq!(sandbox_mode("workspace-write").tag(), RowTag::SandboxMode,);
}

/// The role is declared once, on the tag, and the event reads it
/// back: three message rows, one op-only row, every other row
/// log-only — the literal the fold and the docs have always
/// carried.
#[test]
fn the_role_is_declared_once_and_read_back_by_the_event() {
    use crate::events::SurfaceRole;
    for tag in RowTag::ALL {
        let expected = match tag {
            RowTag::UserMessage
            | RowTag::AssistantMessage
            | RowTag::ToolResult => SurfaceRole::Message,
            RowTag::Compaction => SurfaceRole::OpOnly,
            _ => SurfaceRole::LogOnly,
        };
        assert_eq!(tag.surface_role(), expected, "{}", tag.as_wire());
        assert_eq!(
            tag.empty_event().surface_role(),
            expected,
            "{}",
            tag.as_wire(),
        );
    }
}

/// The row kind is declared once, on the tag, and the fold's
/// placement reads it back: the four surface rows fold as their
/// kinds, and a log-only row places nowhere.
#[test]
fn the_row_kind_is_declared_once_and_read_back_by_the_fold() {
    use crate::surface::fold::{RowKind, row_view};
    let kinds = [
        (RowTag::UserMessage, RowKind::User),
        (RowTag::AssistantMessage, RowKind::Assistant),
        (RowTag::ToolResult, RowKind::Tool),
        (RowTag::Compaction, RowKind::Compaction),
    ];
    for tag in RowTag::ALL {
        let expected = kinds
            .iter()
            .find(|(kinds_tag, _)| *kinds_tag == tag)
            .map(|(_, kind)| *kind);
        let event = tag.empty_event();
        let placed =
            row_view(&event, &serde_json::Value::Null).map(|(kind, _)| kind);
        assert_eq!(placed, expected, "{}", tag.as_wire());
    }
}

/// No tag is ignorable today: every one of them is something
/// rebuilding a session has to read.
///
/// A tag declared ignorable is skipped by a build that does not
/// know it, so the declaration is a promise that losing the row
/// changes nothing.
#[test]
fn no_tag_is_ignorable_today() {
    for tag in RowTag::ALL {
        assert!(!tag.ignorable(), "{}", tag.as_wire());
    }
}
