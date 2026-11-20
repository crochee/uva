//! The declaration module's own tests: what the table says, and the
//! alignment between the two sides of every field.
//!
//! The seam is the declaration itself — the shape list the model is
//! told about and the shape list the parser takes. A field whose
//! advertised shape the parser refuses is a hole in the contract, and
//! this is where it shows.

use std::sync::Arc;

use serde_json::json;
use uva_agent::AgentRegistry;
use uva_tool::Tool;

use super::{
    AGENT,
    ARGUMENTS,
    Absent,
    Accepted,
    DESCRIPTION,
    Field,
    GATE_COMMAND,
    Kind,
    PROMPT,
    REMOVED,
    Shape,
    aligned,
    read,
    read_argv_named,
    read_text_named,
};
use crate::tool::TaskTool;

/// Every field the tool declares, with the members of the objects it
/// carries.
fn fields() -> Vec<&'static Field> {
    let mut all = Vec::new();
    for field in ARGUMENTS {
        all.push(field);
        for shape in field.advertised {
            if let Shape::Object { fields } = shape {
                all.extend(fields.iter());
            }
        }
    }
    all
}

/// The advertised shape kinds of a field.
fn advertised_kinds(field: &Field) -> Vec<Kind> {
    field.advertised.iter().map(|shape| shape.kind()).collect()
}

/// The accepted shape kinds of a field.
fn accepted_kinds(field: &Field) -> Vec<Kind> {
    field
        .accepted
        .iter()
        .map(|accepted| accepted.shape.kind())
        .collect()
}

#[test]
fn every_advertised_shape_is_accepted() {
    for field in fields() {
        for kind in advertised_kinds(field) {
            assert!(
                accepted_kinds(field).contains(&kind),
                "the model is told about `{}` as {kind:?}, but the parser \
                 accepts only {:?}",
                field.name,
                accepted_kinds(field),
            );
        }
    }
}

#[test]
fn every_shape_the_parser_takes_is_one_the_model_is_told_about() {
    for field in fields() {
        for kind in accepted_kinds(field) {
            assert!(
                advertised_kinds(field).contains(&kind),
                "the parser takes `{}` as {kind:?}, but the model is told \
                 only {:?}",
                field.name,
                advertised_kinds(field),
            );
        }
    }
}

#[test]
fn every_field_the_tool_declares_is_one_the_model_is_told_about() {
    for field in ARGUMENTS {
        assert!(
            !field.advertised.is_empty(),
            "`{}` is declared, so the model is told about it — a field \
             the model never hears of has no reader, and a field with \
             no reader is gone",
            field.name,
        );
    }
}

#[test]
fn the_tool_declares_the_arguments_the_model_sends() {
    let names: Vec<&str> = ARGUMENTS.iter().map(|field| field.name).collect();
    assert_eq!(
        names,
        vec!["agent", "prompt", "gate", "isolation"],
        "the declaration names every argument the tool reads",
    );
}

#[test]
fn the_required_arguments_are_the_two_the_schema_promises() {
    let required: Vec<&str> = ARGUMENTS
        .iter()
        .filter(|field| field.required)
        .map(|field| field.name)
        .collect();
    assert_eq!(required, vec!["agent", "prompt"]);
}

#[test]
fn every_field_carries_a_name_and_a_meaning() {
    for field in ARGUMENTS {
        assert!(!field.name.is_empty(), "a field has a wire name");
        assert!(
            field.meaning.is_some() || field.advertised.is_empty(),
            "`{}` is either described for the model or never shown to it",
            field.name,
        );
    }
}

#[test]
fn the_feature_gated_argument_is_isolation() {
    let gated: Vec<&str> = ARGUMENTS
        .iter()
        .filter(|field| field.feature.is_some())
        .map(|field| field.name)
        .collect();
    assert_eq!(
        gated,
        vec!["isolation"],
        "only the worktree knob is behind a feature",
    );
}

#[test]
fn the_loose_shapes_the_parser_takes_are_declared() {
    let gate = ARGUMENTS.iter().find(|field| field.name == "gate").unwrap();
    assert_eq!(
        accepted_kinds(gate),
        vec![Kind::Object, Kind::String, Kind::Null],
        "the gate is read as an object, as a bare command and as null",
    );
    let isolation = ARGUMENTS
        .iter()
        .find(|field| field.name == "isolation")
        .unwrap();
    assert_eq!(
        accepted_kinds(isolation),
        vec![Kind::Boolean, Kind::Object, Kind::Null],
        "the isolation knob is read as a flag, as an object and as null",
    );
}

#[test]
fn the_schema_and_the_parser_state_the_same_bound() {
    for field in fields() {
        assert!(
            aligned(field),
            "the two sides of `{}` state a different bound, and the \
             declaration does not say why",
            field.name,
        );
    }
}

#[test]
fn a_bound_the_parser_does_not_keep_is_never_aligned() {
    // The declaration has no exception any more: a promise the
    // parser does not keep is a hole in the contract, and the
    // alignment check says so.
    let broken = Field {
        name: "budget",
        meaning: None,
        required: false,
        feature: None,
        advertised: &[Shape::String { min_len: Some(1) }],
        accepted: &[Accepted {
            shape: Shape::String { min_len: None },
            bound: None,
            item: None,
        }],
        absent: Absent::None,
        mismatch: "a budget must be at least one second",
    };
    assert!(
        !aligned(&broken),
        "a promise the parser does not keep is a hole in the contract",
    );
}

#[test]
fn a_field_the_schema_requires_but_the_parser_tolerates_is_never_aligned() {
    // The schema's `required` and the parser's refusal to run without
    // the field are the same promise stated twice: a field that the
    // schema calls required while the parser reads it as optional is
    // a promise the parser never made.
    let lenient = Field {
        name: "agent",
        meaning: None,
        required: true,
        feature: None,
        advertised: &[Shape::String { min_len: None }],
        accepted: &[Accepted {
            shape: Shape::String { min_len: None },
            bound: None,
            item: None,
        }],
        absent: Absent::None,
        mismatch: "task tool requires a string `agent` field",
    };
    assert!(
        !aligned(&lenient),
        "the schema promises a required field the parser does not need",
    );
}

#[test]
fn the_string_bound_counts_characters_not_bytes() {
    // `minLength` in JSON Schema counts characters, and so does the
    // parser: a two-character name is two, however many bytes its
    // UTF-8 encoding takes.
    let field = Field {
        name: "agent",
        meaning: None,
        required: true,
        feature: None,
        advertised: &[Shape::String { min_len: Some(3) }],
        accepted: &[Accepted {
            shape: Shape::String { min_len: Some(3) },
            bound: Some("a name of at least three characters"),
            item: None,
        }],
        absent: Absent::Reject("a name is required"),
        mismatch: "a name must be a string",
    };
    assert!(
        aligned(&field),
        "the two sides of the probe field state the same bound",
    );
    let two_characters = json!("é米");
    assert_eq!(
        two_characters.as_str().map(str::len),
        Some(5),
        "the probe name is two characters in five bytes",
    );
    let error = read(&field, Some(&two_characters))
        .expect_err("two characters break a three-character bound");
    assert_eq!(error, "a name of at least three characters");
}

#[test]
fn every_removed_argument_is_recorded_with_a_reason() {
    for removed in REMOVED {
        assert!(
            !removed.why.is_empty(),
            "a removed argument says why it is gone",
        );
        let refusal = removed.refusal();
        assert!(
            refusal.contains(&removed.name()),
            "the refusal names the argument the model sent: {refusal}",
        );
    }
}

#[test]
fn the_description_the_model_reads_is_the_one_recorded_here() {
    let tool = TaskTool::new(Arc::new(AgentRegistry::new()));
    assert_eq!(
        tool.description(),
        DESCRIPTION,
        "the model reads the words the declaration records",
    );
}

#[test]
fn the_agent_name_is_refused_when_it_is_empty() {
    let error = read_text_named(&AGENT, &json!({"agent": ""}))
        .expect_err("an empty agent name is refused");
    assert_eq!(error, "`agent` must be a non-empty agent name");
}

#[test]
fn the_prompt_is_refused_when_it_is_missing_or_ill_typed() {
    let missing = read_text_named(&PROMPT, &json!({}))
        .expect_err("a call without a prompt is refused");
    assert_eq!(missing, "task tool requires a string `prompt` field");
    let ill_typed = read_text_named(&PROMPT, &json!({"prompt": 7}))
        .expect_err("a prompt that is not a string is refused");
    assert_eq!(ill_typed, "task tool requires a string `prompt` field");
}

#[test]
fn the_command_is_refused_when_the_array_is_empty() {
    let payload = json!({"command": []});
    let error = read_argv_named(
        &GATE_COMMAND,
        payload.as_object().expect("the payload is an object"),
    )
    .expect_err("a command with no arguments is refused");
    assert_eq!(error, "gate.command must contain at least one argument",);
}
