//! The `task` tool's arguments, from the model's side and the
//! tool's side: the schema the model reads, the shapes the tool
//! accepts, and the words it refuses with.
//!
//! The seam is the wire: a tool-call payload goes in, a
//! [`TaskSpec`] (or a refusal) comes out, and the schema the model
//! was shown is the other half of the same contract. Both halves are
//! pinned here, so a change to either one has to be a change to the
//! declaration they come from.

use std::{fs, path::PathBuf, sync::Arc};

use serde_json::{Map, Value, json};
use uva_agent::AgentRegistry;
use uva_tool::{Tool, ToolFeatures};
use uva_tool_task::{GateSpec, TaskSpec, TaskTool, WorktreeSpec};

/// The arguments every call needs.
fn base() -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert("agent".to_string(), json!("peer"));
    arguments.insert("prompt".to_string(), json!("do the thing"));
    arguments
}

/// The arguments of a call, plus one more field.
fn call_with(field: &str, value: Value) -> Value {
    let mut arguments = base();
    arguments.insert(field.to_string(), value);
    Value::Object(arguments)
}

/// The schema the model reads with every feature on.
fn schema_with_isolation() -> Value {
    TaskTool::new(Arc::new(AgentRegistry::new())).parameters()
}

/// The schema the model reads when the worktree capability is off.
fn schema_without_isolation() -> Value {
    TaskTool::new(Arc::new(AgentRegistry::new()))
        .with_features(&ToolFeatures::none())
        .parameters()
}

#[test]
fn the_schema_the_model_reads_is_the_one_the_declaration_renders() {
    let expected = json!({
        "type": "object",
        "properties": {
            "agent": {
                "type": "string",
                "description": "Name of the registered peer agent to run.",
                "minLength": 1
            },
            "prompt": {
                "type": "string",
                "description":
                    "The complete, self-contained task for the sub-agent."
            },
            "gate": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "command": {
                                "oneOf": [
                                    {"type": "string"},
                                    {
                                        "type": "array",
                                        "items": {"type": "string"},
                                        "minItems": 1
                                    }
                                ]
                            }
                        },
                        "required": ["command"]
                    },
                    {"type": "string"},
                    {"type": "null"}
                ],
                "description": "Optional verification command run against \
                                the child's working tree after it finishes. \
                                Either an object with a `command`, or the \
                                command itself as a string — one program \
                                name, no shell splitting."
            },
            "isolation": {
                "oneOf": [
                    {"type": "boolean"},
                    {"type": "object"},
                    {"type": "null"}
                ],
                "description": "Run the child in a fresh detached git \
                                worktree. Edits are committed to \
                                `uva/agent-<ulid>` on cleanup when the \
                                worktree is dirty. Either `true` or an \
                                object — the object carries no members of \
                                its own."
            }
        },
        "required": ["agent", "prompt"]
    });
    assert_eq!(
        serde_json::to_string(&schema_with_isolation()).unwrap(),
        serde_json::to_string(&expected).unwrap(),
        "the model reads the schema the declaration renders — key \
         order included",
    );
}

#[test]
fn a_deployment_without_the_capability_never_hears_about_it() {
    let schema = schema_without_isolation();
    assert!(
        schema["properties"].get("isolation").is_none(),
        "the worktree knob is gone when the capability is off: {schema}",
    );
    assert_eq!(
        schema["required"],
        json!(["agent", "prompt"]),
        "the two core arguments stay required either way",
    );
}

/// The JSON types a schema fragment names — one per alternative.
fn types_named_by(fragment: &Value) -> Vec<String> {
    fragment["oneOf"]
        .as_array()
        .expect("the fragment names several shapes")
        .iter()
        .map(|shape| shape["type"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn the_schema_names_the_command_object_and_the_bare_command_for_the_gate() {
    let schema = schema_with_isolation();
    let gate = &schema["properties"]["gate"];
    assert_eq!(
        types_named_by(gate),
        vec!["object", "string", "null"],
        "the model is told that the gate is a command object or a bare \
         command",
    );
    assert!(
        gate["oneOf"][0]["properties"]["command"].is_object(),
        "the command object keeps its members: {gate}",
    );
}

#[test]
fn the_schema_names_the_flag_and_the_object_for_the_isolation() {
    let schema = schema_with_isolation();
    let isolation = &schema["properties"]["isolation"];
    assert_eq!(
        types_named_by(isolation),
        vec!["boolean", "object", "null"],
        "the model is told that the knob is a flag or an object",
    );
}

/// A value that matches a schema fragment — the legal shape of what
/// the model was shown. A fragment that names several shapes has one
/// sample per shape: every shape the model was told about is a shape
/// the parser has to take.
fn samples_of(fragment: &Value) -> Vec<Value> {
    if let Some(alternatives) = fragment.get("oneOf").and_then(Value::as_array)
    {
        return alternatives.iter().flat_map(samples_of).collect();
    }
    vec![match fragment["type"].as_str() {
        Some("string") => json!("peer"),
        Some("boolean") => json!(true),
        Some("integer") => json!(1),
        Some("array") => json!(["cargo", "test"]),
        Some("null") => Value::Null,
        Some("object") => {
            let mut members = Map::new();
            let required =
                fragment["required"].as_array().cloned().unwrap_or_default();
            for name in required {
                let name = name.as_str().unwrap_or_default().to_string();
                let member = sample_of(&fragment["properties"][&name]);
                members.insert(name, member);
            }
            Value::Object(members)
        }
        other => panic!(
            "the schema names a shape this test cannot sample: {other:?}"
        ),
    }]
}

/// The first legal shape of a fragment — what one call carries.
fn sample_of(fragment: &Value) -> Value {
    samples_of(fragment).into_iter().next().unwrap_or_else(|| {
        panic!("the schema names no shape this test can sample")
    })
}

#[test]
fn every_shape_the_schema_declares_is_one_the_parser_accepts() {
    let schema = schema_with_isolation();
    let properties = schema["properties"].as_object().unwrap();
    assert!(!properties.is_empty(), "the schema declares arguments");
    for (name, fragment) in properties {
        for sample in samples_of(fragment) {
            let spec = TaskSpec::from_value(&call_with(name, sample.clone()))
                .unwrap_or_else(|error| {
                    panic!(
                        "the parser refuses the legal shape of `{name}` \
                         ({sample}): {error}"
                    )
                });
            assert!(
                !spec.agent.is_empty(),
                "the parsed call names the peer: {spec:?}",
            );
        }
    }
}

/// The payload the schema's own required arguments make: every
/// required name, holding the first shape the schema names for it.
fn required_only(schema: &Value) -> Map<String, Value> {
    let properties = schema["properties"]
        .as_object()
        .expect("the schema has properties");
    let required = schema["required"]
        .as_array()
        .expect("the schema names the required arguments");
    let mut arguments = Map::new();
    for name in required {
        let name = name.as_str().expect("a required argument is named");
        arguments.insert(name.to_string(), sample_of(&properties[name]));
    }
    arguments
}

/// The parser reads the wire names the declaration states.
///
/// Every name the schema publishes is a name the parser reads: the
/// call the schema promises parses, and the parse tells the value
/// under that name apart from a call without it. A parser reading
/// another name would refuse the call the schema promises — or read
/// nothing at all.
#[test]
fn the_parser_reads_the_wire_names_the_schema_states() {
    let schema = schema_with_isolation();
    let properties = schema["properties"]
        .as_object()
        .expect("the schema has properties");
    let required = schema["required"]
        .as_array()
        .expect("the schema names the required arguments")
        .clone();
    let base = required_only(&schema);
    for (name, fragment) in properties {
        let mut arguments = base.clone();
        arguments.insert(name.clone(), sample_of(fragment));
        let with = TaskSpec::from_value(&Value::Object(arguments))
            .unwrap_or_else(|error| {
                panic!(
                    "the parser refuses the call the schema promises \
                     (`{name}`): {error}"
                )
            });
        let mut without = base.clone();
        without.remove(name);
        let without = TaskSpec::from_value(&Value::Object(without));
        if required.contains(&json!(name)) {
            assert!(
                without.is_err(),
                "the parser accepts a call without `{name}`, which the \
                 schema says it requires",
            );
        } else {
            let without = without.expect("a call without an optional field");
            assert_ne!(
                with, without,
                "the parser does not read `{name}` — the schema publishes \
                 a field the parser ignores",
            );
        }
    }
}

/// The parser module — the one place that reads a tool-call payload.
const PARSER_SOURCE: &str = "src/spec.rs";

/// The string literals in a source, comments excluded: a name in a
/// comment is not a name the parser reads.
fn string_literals(source: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut chars = source.chars().peekable();
    let mut in_comment = false;
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    while let Some(ch) = chars.next() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
            }
        } else if in_string {
            if escaped {
                escaped = false;
                current.push(ch);
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                literals.push(std::mem::take(&mut current));
                in_string = false;
            } else {
                current.push(ch);
            }
        } else if ch == '/' && chars.peek() == Some(&'/') {
            in_comment = true;
            chars.next();
        } else if ch == '"' {
            in_string = true;
        }
    }
    literals
}

/// The wire names the declaration states: the arguments, and the
/// members of the objects they carry.
fn declared_wire_names(schema: &Value) -> Vec<String> {
    let properties = schema["properties"]
        .as_object()
        .expect("the schema has properties");
    let mut names: Vec<String> = properties.keys().cloned().collect();
    for fragment in properties.values() {
        // A field with several shapes keeps its object branch among
        // the alternatives; the members of that branch are wire
        // names too.
        let branch = match fragment["oneOf"].as_array() {
            Some(alternatives) => alternatives
                .iter()
                .find(|shape| shape["type"] == "object")
                .unwrap_or(&alternatives[0]),
            None => fragment,
        };
        if let Some(members) = branch["properties"].as_object() {
            names.extend(members.keys().cloned());
        }
    }
    names
}

/// The parser writes no wire name down itself.
///
/// The declaration owns every name the model sends: the parser reads
/// a key because the table names it, not because the parser repeats
/// it. A literal in the parser is a second copy of the name — the
/// copy that stays behind when the table is renamed and the schema
/// moves on without it.
#[test]
fn the_parser_writes_no_wire_name_down_itself() {
    let names = declared_wire_names(&schema_with_isolation());
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PARSER_SOURCE),
    )
    .unwrap_or_else(|error| panic!("{PARSER_SOURCE} is readable: {error}"));
    for literal in string_literals(&source) {
        assert!(
            !names.contains(&literal),
            "the parser writes the wire name `{literal}` down itself — \
             the declaration owns every name the model sends",
        );
    }
}

/// The gate each accepted shape yields.
fn gate_of(value: Value) -> Option<GateSpec> {
    TaskSpec::from_value(&call_with("gate", value))
        .expect("the gate shape is accepted")
        .gate
}

#[test]
fn the_gate_shapes_the_tool_accepts_are_still_accepted() {
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "a bare command",
            json!("make test"),
            vec!["make test".to_string()],
        ),
        (
            "an object with one command",
            json!({"command": "make test"}),
            vec!["make test".to_string()],
        ),
        (
            "an object with an argv",
            json!({"command": ["make", "test"]}),
            vec!["make".to_string(), "test".to_string()],
        ),
        ("no gate at all", json!(null), Vec::new()),
    ];
    for (label, value, command) in cases {
        let gate = gate_of(value);
        if command.is_empty() {
            assert!(
                gate.is_none(),
                "{label}: the call carries no gate: {gate:?}",
            );
            continue;
        }
        let gate = gate.expect("the gate shape is accepted");
        assert_eq!(gate.command, command, "{label}: the argv");
    }
}

#[test]
fn the_isolation_shapes_the_tool_accepts_are_still_accepted() {
    let cases: Vec<(&str, Value, bool)> = vec![
        ("the flag on", json!(true), true),
        ("the flag off", json!(false), false),
        ("null", json!(null), false),
        ("an object", json!({}), true),
        ("an object with members", json!({"whatever": 1}), true),
    ];
    for (label, value, isolated) in cases {
        let spec = TaskSpec::from_value(&call_with("isolation", value))
            .unwrap_or_else(|error| {
                panic!("the parser refuses the isolation shape: {error}")
            });
        assert_eq!(
            spec.isolation.is_some(),
            isolated,
            "{label}: the child runs isolated or not",
        );
    }
    let spec = TaskSpec::from_value(&base().into()).unwrap();
    assert!(
        spec.isolation.is_none(),
        "a call without the knob runs without a worktree",
    );
}

#[test]
fn the_agent_shape_the_tool_accepts_is_still_accepted() {
    let spec = TaskSpec::from_value(&base().into()).unwrap();
    assert_eq!(spec.agent, "peer");
}

/// The schema fragment of one argument.
fn property_of(schema: &Value, name: &str) -> Value {
    schema["properties"][name].clone()
}

/// The schema fragment of one member of an object argument.
///
/// A field with several shapes keeps its object branch in the first
/// alternative of `oneOf`; the member is looked up there.
fn member_of(schema: &Value, name: &str, member: &str) -> Value {
    let field = &schema["properties"][name];
    let object = match field["oneOf"].as_array() {
        Some(alternatives) => &alternatives[0],
        None => field,
    };
    object["properties"][member].clone()
}

#[test]
fn the_agent_name_the_schema_states_is_the_name_the_tool_takes() {
    let schema = schema_with_isolation();
    let fragment = property_of(&schema, "agent");
    let min = fragment["minLength"]
        .as_u64()
        .expect("the schema states how long a name may be");
    assert!(min >= 1, "the schema promises a name of {min} characters");

    let name = "a".repeat(min as usize);
    let spec = TaskSpec::from_value(&call_with("agent", json!(name.clone())))
        .expect("a name of the promised length is taken");
    assert_eq!(spec.agent, name, "the name the model sends is the name run");

    let short = "a".repeat(min as usize - 1);
    let error = TaskSpec::from_value(&call_with("agent", json!(short)))
        .expect_err("a name shorter than the promise is refused");
    assert!(
        error.contains("agent"),
        "the refusal names the field the model got wrong: {error}",
    );
}

#[test]
fn the_command_the_schema_states_is_the_command_the_tool_takes() {
    let schema = schema_with_isolation();
    let fragment = member_of(&schema, "gate", "command");
    let alternatives = fragment["oneOf"]
        .as_array()
        .expect("the command has the shapes the schema names");
    let array = alternatives
        .iter()
        .find(|shape| shape["type"] == "array")
        .expect("the array shape is one of them");
    let min = array["minItems"]
        .as_u64()
        .expect("the schema states how many arguments the command takes");
    assert!(min >= 1, "the schema promises at least {min} arguments");

    let arguments: Vec<String> =
        (0..min).map(|index| format!("arg{index}")).collect();
    let gate = gate_of(json!({"command": arguments.clone()}))
        .expect("a command of the promised length is taken");
    assert_eq!(
        gate.command, arguments,
        "the argv the model sends is the argv the tool runs",
    );

    let fewer = &arguments[..arguments.len() - 1];
    let error =
        TaskSpec::from_value(&call_with("gate", json!({"command": fewer})))
            .expect_err("a command with too few arguments is refused");
    assert!(
        error.contains("command"),
        "the refusal names the field the model got wrong: {error}",
    );
}

#[test]
fn the_prompt_the_schema_states_is_the_prompt_the_tool_takes() {
    let schema = schema_with_isolation();
    let required = schema["required"]
        .as_array()
        .expect("the schema names the required arguments");
    assert!(
        required.contains(&json!("prompt")),
        "the schema says the prompt is required: {required:?}",
    );

    let spec = TaskSpec::from_value(&base().into())
        .expect("a call with a prompt is taken");
    assert_eq!(
        spec.prompt, "do the thing",
        "the prompt the model sends is the prompt the child gets",
    );

    let mut without_prompt = base();
    without_prompt.remove("prompt");
    let error = TaskSpec::from_value(&Value::Object(without_prompt))
        .expect_err("a call without a prompt is refused");
    assert!(
        error.contains("prompt"),
        "the refusal names the field the model left out: {error}",
    );

    let error = TaskSpec::from_value(&call_with("prompt", json!(7)))
        .expect_err("a prompt that is not a string is refused");
    assert!(
        error.contains("prompt"),
        "the refusal names the field the model got wrong: {error}",
    );
}

/// The arguments the tool used to take and does not take any more.
///
/// Each one is a field the tool parsed and then nobody read: the
/// trace context was handed to nobody, and the gate's budget was
/// applied to nothing. Both are gone — and a call that still
/// carries one is refused, not dropped.
fn removed_arguments() -> Vec<(&'static str, Value)> {
    vec![
        (
            "trace_context",
            call_with(
                "trace_context",
                json!({
                    "trace_id_hex": "0af7651916cd43dd8448eb211c80319c",
                    "parent_span_id_hex": "b7ad6b7169203331",
                    "trace_flags_byte": 1,
                    "trace_state": "vendor1=value1"
                }),
            ),
        ),
        (
            "gate.timeout_secs",
            call_with("gate", json!({"command": ["make"], "timeout_secs": 60})),
        ),
    ]
}

#[test]
fn the_removed_arguments_are_not_in_the_schema_the_model_reads() {
    let schema = schema_with_isolation();
    assert!(
        schema["properties"].get("trace_context").is_none(),
        "the model is not told about a trace context: {schema}",
    );
    assert!(
        member_of(&schema, "gate", "timeout_secs").is_null(),
        "the model is not promised a budget the gate never applies: \
         {schema}",
    );
}

#[test]
fn a_call_with_a_removed_argument_is_refused_with_the_reason() {
    for (name, arguments) in removed_arguments() {
        let error = TaskSpec::from_value(&arguments)
            .expect_err("a removed argument is refused, not dropped");
        assert!(
            error.contains(name),
            "the refusal names the argument the model sent: {error}",
        );
    }
}

/// An explicit `null` carries nothing: a model that sends a removed
/// argument as null has not sent it, and the call parses the way a
/// call without it always did.
#[test]
fn a_removed_argument_sent_as_null_carries_nothing() {
    let cases: Vec<(&str, Value)> = vec![
        ("trace_context", call_with("trace_context", Value::Null)),
        (
            "gate.timeout_secs",
            call_with(
                "gate",
                json!({"command": ["make"], "timeout_secs": null}),
            ),
        ),
    ];
    for (name, arguments) in cases {
        let spec = TaskSpec::from_value(&arguments).unwrap_or_else(|error| {
            panic!("a null `{name}` carries nothing: {error}")
        });
        assert_eq!(spec.agent, "peer", "{name}: the call still parses");
    }
}

#[test]
fn the_words_the_tool_refuses_with_are_the_ones_it_used() {
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "an agent that is not a name",
            json!({"agent": 7}),
            "task tool requires a string `agent` field",
        ),
        (
            "an empty agent name",
            json!({"agent": ""}),
            "`agent` must be a non-empty agent name",
        ),
        (
            "a call without a prompt",
            json!({"agent": "peer"}),
            "task tool requires a string `prompt` field",
        ),
        (
            "a prompt that is not a string",
            json!({"agent": "peer", "prompt": 7}),
            "task tool requires a string `prompt` field",
        ),
        (
            "a gate that is neither",
            call_with("gate", json!(7)),
            "gate must be a string or object",
        ),
        (
            "a gate without a command",
            call_with("gate", json!({})),
            "gate object requires a `command` field",
        ),
        (
            "a command that is neither",
            call_with("gate", json!({"command": 7})),
            "gate.command must be string or array",
        ),
        (
            "a command with an ill-typed entry",
            call_with("gate", json!({"command": ["make", 7]})),
            "gate command entries must be strings",
        ),
        (
            "a command with no arguments",
            call_with("gate", json!({"command": []})),
            "gate.command must contain at least one argument",
        ),
        (
            "an isolation that is neither",
            call_with("isolation", json!(7)),
            "`isolation` must be `true`, `false`, null or an object;              got 7",
        ),
        (
            "a trace context",
            call_with("trace_context", json!({"trace_id_hex": "abc"})),
            "`trace_context` is no longer an argument of the `task` \
             tool: the tool never read it, so a trace context sent \
             with a call was parsed and then dropped",
        ),
        (
            "a gate budget",
            call_with("gate", json!({"command": ["make"], "timeout_secs": 60})),
            "`gate.timeout_secs` is no longer an argument of the \
             `task` tool: the gate never applied it, so a wall-clock \
             budget sent with a call was parsed and then dropped",
        ),
    ];
    for (label, arguments, words) in cases {
        let error =
            TaskSpec::from_value(&arguments).expect_err("the call is refused");
        assert_eq!(error, words, "{label}: the words the model reads");
    }
}

#[test]
fn an_unknown_argument_is_dropped_as_it_was() {
    // An argument the tool never had is still dropped: only the
    // arguments that were *removed* are refused, because a model
    // may still be sending the ones it was told about.
    let spec = TaskSpec::from_value(&call_with("whatever", json!(7)))
        .expect("an unknown argument is not a refusal");
    assert_eq!(spec.agent, "peer");
    assert_eq!(spec.prompt, "do the thing");
}

#[test]
fn the_worktree_spec_is_the_one_the_isolation_knob_names() {
    let spec =
        TaskSpec::from_value(&call_with("isolation", json!(true))).unwrap();
    assert_eq!(
        spec.isolation,
        Some(WorktreeSpec),
        "the flag on means a worktree of the child's own",
    );
}
