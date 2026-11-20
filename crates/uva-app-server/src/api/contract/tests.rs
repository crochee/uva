use std::collections::BTreeSet;

use super::*;

fn document_entries() -> (Value, Vec<String>) {
    let document = document();
    let definitions = document["definitions"]
        .as_object()
        .expect("the document has definitions")
        .keys()
        .cloned()
        .collect();
    (document, definitions)
}

#[test]
fn every_row_is_exported_under_a_unique_typescript_name() {
    let mut seen = BTreeSet::new();
    for entry in catalog() {
        assert!(
            seen.insert(entry.ts_name),
            "two rows export `{}`",
            entry.ts_name
        );
        assert!(
            entry
                .ts_name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase()),
            "`{}` is not a TypeScript type name",
            entry.ts_name
        );
        assert!(
            !entry.method.is_empty(),
            "`{}` names no method",
            entry.ts_name
        );
    }
}

#[test]
fn every_row_reaches_a_definition_and_every_ref_resolves() {
    let (document, definitions) = document_entries();
    let known: BTreeSet<&str> =
        definitions.iter().map(String::as_str).collect();
    for entry in catalog() {
        assert!(
            known.contains(entry.ts_name),
            "`{}` is missing from definitions",
            entry.ts_name
        );
    }
    let mut dangling = Vec::new();
    collect_dangling(&document, &known, &mut dangling);
    assert!(dangling.is_empty(), "dangling $refs: {dangling:?}");
}

#[test]
fn a_renamed_rust_type_is_not_also_exported_under_its_rust_name() {
    let (_document, definitions) = document_entries();
    for name in [
        "SessionHeader",
        "ContextMeter",
        "SendMessageResponse",
        "CompactResponse",
        "SessionUsageResponse",
        "ScheduleInfo",
        "ScheduleDetail",
        "ScheduleTickResponse",
        "SkillInfo",
        "Delivery",
        "ChangeState",
        "RowTag",
        "RowDialect",
        "RowTagOrDialect",
    ] {
        assert!(
            !definitions.iter().any(|definition| definition == name),
            "`{name}` is exported beside its TypeScript name"
        );
    }
}

#[test]
fn the_list_row_is_the_one_generic_and_drops_the_phantom_field() {
    let entry = catalog()
        .into_iter()
        .find(|entry| entry.ts_name == "List")
        .expect("the list envelope is catalogued");
    assert_eq!(entry.generic, Some(GENERIC_PARAMETER));
    let properties = entry.schema["properties"]
        .as_object()
        .expect("the envelope names its properties");
    assert!(
        !properties.contains_key("has_more"),
        "the envelope never carried `has_more`: {properties:?}"
    );
    let required: Vec<&str> = entry.schema["required"]
        .as_array()
        .expect("the envelope requires its data field")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, vec!["data"]);
    assert!(
        catalog()
            .iter()
            .filter(|entry| entry.generic.is_some())
            .count()
            == 1,
        "only the list envelope is generic"
    );
}

#[test]
fn an_always_present_optional_is_required_and_nullable() {
    // `compact` always writes `start`, `null` when it commits
    // nothing: the schema must demand the field and allow the null.
    let document = document();
    let compact = &document["definitions"]["CompactResult"];
    let required: Vec<&str> = compact["required"]
        .as_array()
        .expect("the response requires its committed flag")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(required.contains(&"start"), "{compact}");
    assert!(required.contains(&"end"), "{compact}");
    assert!(
        compact["properties"]["start"].to_string().contains("null"),
        "`start` is nullable: {compact}"
    );
    // …while a field the writer omits is optional and not null.
    assert!(!required.contains(&"summary"), "{compact}");
}

#[test]
fn a_request_body_keeps_the_fields_its_reader_defaults() {
    let document = document();
    let create = &document["definitions"]["CreateScheduleRequest"];
    let required: Vec<&str> = create["required"]
        .as_array()
        .expect("the body requires its name and kind")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut required = required;
    required.sort_unstable();
    assert_eq!(required, vec!["kind", "name"]);
}

#[test]
fn the_document_is_deterministic_and_carries_no_environment() {
    assert_eq!(document_json(), document_json());
    let text = document_json();
    assert!(
        !text.contains(
            "$schema\": \"http://json-schema.org/draft-07/schema#\",\n \"x"
        ),
        "one meta-schema"
    );
    assert_eq!(text.matches(META_SCHEMA).count(), 1);
    assert!(!text.contains(env!("CARGO_MANIFEST_DIR")));
    assert!(!text.contains("/home/"));
    assert!(!text.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn the_catalogued_union_is_the_row_vocabulary_itself() {
    // The tag union the browser dispatches on is the vocabulary
    // itself — `uva_session::RowTag` — not a hand-written mirror
    // of it. The catalog publishes that type under the
    // TypeScript name `SessionEventType`, so the browser keeps
    // the name its row accounting already uses.
    let entry = catalog()
        .into_iter()
        .find(|entry| entry.ts_name == "SessionEventType")
        .expect("the vocabulary is catalogued");
    assert_eq!(
        entry.rust_type, "RowTag",
        "the published union is not sourced from the row vocabulary"
    );
    let vocabulary =
        serde_json::to_value(schemars::schema_for!(uva::session::RowTag))
            .expect("a schema serializes");
    assert_eq!(
        entry.schema["oneOf"], vocabulary["oneOf"],
        "the published union is not the vocabulary's own tag set"
    );
}

#[test]
fn the_wire_no_longer_publishes_a_dialect_union() {
    // The server publishes current rows only (`publish_row` restores
    // a dialect row at the wire boundary), so a dialect spelling is
    // not part of the wire's `type` at all. A catalogued dialect
    // union would advertise a shape the routes never send.
    let entries = catalog();
    assert!(
        !entries
            .into_iter()
            .any(|entry| entry.ts_name == "SessionRowDialect"
                || entry.ts_name == "SessionRowTag"),
        "a dialect union is still catalogued"
    );
}

#[test]
fn the_row_envelope_covers_every_member_the_vocabulary_can_write() {
    // The envelope describes the published row, and the published row
    // is the vocabulary's own row: a member a `SessionEvent` variant
    // declares is a member the wire carries. The envelope used to
    // name nine of them, so a variant that grew a field reached the
    // wire without the contract learning about it — and the browser,
    // unable to see the typed member, read the payload bag's second
    // spelling of the same fact instead.
    //
    // This assertion is the guard: the member list is derived from
    // the vocabulary's own schema, so there is no second hand-written
    // list to keep in step — only this one comparison.
    let vocabulary: Value =
        serde_json::to_value(schemars::schema_for!(uva::session::SessionEvent))
            .expect("the vocabulary schema serialises");
    let branches = vocabulary["oneOf"]
        .as_array()
        .expect("the vocabulary is a union over its variants");
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for branch in branches {
        let properties = branch["properties"]
            .as_object()
            .expect("a variant names its members");
        for member in properties.keys() {
            declared.insert(member.as_str());
        }
    }

    let document = document();
    let turn = &document["definitions"]["SessionTurn"];
    let envelope: BTreeSet<&str> = turn["oneOf"]
        .as_array()
        .expect("the row is a union over its tags")
        .iter()
        .flat_map(|branch| {
            branch["properties"]
                .as_object()
                .expect("a branch names its members")
                .keys()
                .map(String::as_str)
        })
        .collect();

    let missing: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|member| !envelope.contains(member))
        .collect();
    assert!(
        missing.is_empty(),
        "the row envelope does not describe {missing:?} — a variant \
         writes them, so the wire carries them and the contract must \
         name them"
    );

    // The writer stamps two members no variant declares, and the
    // envelope must name them too: a row's ordinal and instant ride
    // every row, and the `ignorable` marker rides the rows whose kind
    // a build does not know.
    for member in ["seq", "ts", "ignorable"] {
        assert!(
            envelope.contains(member),
            "the row envelope does not describe `{member}`"
        );
    }
}

#[test]
fn a_session_turn_discriminates_over_the_whole_tag_vocabulary() {
    // `SessionTurn` is a discriminated union on the row's tag: one
    // branch per tag of the closed vocabulary, so the browser cannot
    // account for a tag the vocabulary did not teach it. The branches
    // are read from the vocabulary itself, and this test pins that
    // the union covers every tag `SessionEventType` publishes — and
    // no other.
    let document = document();
    let turn = &document["definitions"]["SessionTurn"];
    let branches = turn["oneOf"]
        .as_array()
        .expect("the row is a union over its tags");
    let vocabulary: Value =
        serde_json::to_value(schemars::schema_for!(uva::session::RowTag))
            .expect("the vocabulary schema serialises");
    let tags: BTreeSet<&str> = vocabulary["oneOf"]
        .as_array()
        .expect("the vocabulary is a union")
        .iter()
        .filter_map(|branch| branch["const"].as_str())
        .collect();
    let named: BTreeSet<&str> = branches
        .iter()
        .filter_map(|branch| branch["properties"]["type"]["const"].as_str())
        .collect();
    assert_eq!(
        named, tags,
        "the union's branches are not the vocabulary's own tags"
    );

    // A message row's payload is the provider's own message — the
    // same type the provider serializes, not an open bag.
    for tag in ["user_message", "assistant_message"] {
        let branch = branches
            .iter()
            .find(|branch| branch["properties"]["type"]["const"] == json!(tag))
            .expect("the message row's branch exists");
        assert_eq!(
            branch["properties"]["data"],
            json!({ "$ref": "#/definitions/Message" }),
            "`{tag}`'s payload is not the provider message"
        );
    }
    // No other row's payload is narrowed: a `tool_result` row holds
    // the tool message on the run's own row and the crash-repair
    // closer's marker on the repair's — one tag, two payloads, so
    // the schema says neither.
    for branch in branches {
        let tag = branch["properties"]["type"]["const"]
            .as_str()
            .expect("the branch names its tag");
        if !matches!(tag, "user_message" | "assistant_message") {
            assert_ne!(
                branch["properties"]["data"],
                json!({ "$ref": "#/definitions/Message" }),
                "`{tag}`'s payload must stay open"
            );
        }
    }

    assert!(
        document["definitions"].get("SessionRowDialect").is_none()
            && document["definitions"].get("RowDialect").is_none(),
        "a dialect definition survives in the schema document"
    );
}

#[test]
fn no_definition_is_an_orphan() {
    // A definition either is a catalogued row (exported on purpose) or
    // is reached by a `$ref` from one. Anything else is a leftover the
    // catalog no longer names — dead weight in the generated file.
    let document = document();
    let catalogued: BTreeSet<&str> = document["x-uva-catalog"]
        .as_array()
        .expect("the document carries its catalog")
        .iter()
        .filter_map(|row| row["tsName"].as_str())
        .collect();
    let mut reached = BTreeSet::new();
    collect_refs(&document, &mut reached);
    let orphans: Vec<&str> = document["definitions"]
        .as_object()
        .expect("definitions")
        .keys()
        .map(String::as_str)
        .filter(|name| !catalogued.contains(name) && !reached.contains(*name))
        .collect();
    assert!(orphans.is_empty(), "unreferenced definitions: {orphans:?}");
}

fn collect_refs(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref"
                    && let Some(name) = value.as_str().and_then(|pointer| {
                        pointer.strip_prefix("#/definitions/")
                    })
                {
                    out.insert(name.to_owned());
                }
                collect_refs(value, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}

fn collect_dangling(
    value: &Value,
    known: &BTreeSet<&str>,
    out: &mut Vec<String>,
) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref"
                    && let Some(pointer) = value.as_str()
                    && let Some(name) = pointer.strip_prefix("#/definitions/")
                    && !known.contains(name)
                {
                    out.push(pointer.to_owned());
                }
                collect_dangling(value, known, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_dangling(item, known, out);
            }
        }
        _ => {}
    }
}
