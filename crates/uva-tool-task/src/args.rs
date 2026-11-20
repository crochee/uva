//! The declaration of the `task` tool's arguments — and of the tool's
//! own description.
//!
//! One place owns every argument: the wire name, the JSON shapes it
//! takes, the bounds those shapes put on a value, and what the
//! argument means. The model-facing schema and the tool's parser are
//! two projections of this table — neither of them hand-writes a
//! shape any more, so the two cannot drift apart by accident.
//!
//! The same place owns what the model is told about the tool as a
//! whole: [`DESCRIPTION`] is the one recorded copy of the tool's
//! description, and [`tool_definition`] publishes it. The crate
//! publishes no second definition — the mounted tool carries these
//! words to the model, and a test compares what the model receives
//! with what is written here.
//!
//! # Two sides of one field
//!
//! Every field declares two shape lists:
//!
//! - `advertised` — the shapes the model is told about. They become
//!   the JSON Schema.
//! - `accepted` — the shapes the parser takes. They become the parse
//!   rules, the bounds and the refusals.
//!
//! The two lists are the drift ledger: a shape the model is told
//! about but the parser refuses is a hole in the contract, and a
//! shape the parser takes but the model never heard of is a secret
//! one. The alignment test in `tests.rs` walks every field and fails
//! when an advertised shape is not among the accepted ones.
//!
//! # What the declaration says today
//!
//! The table states what the tool accepts **today**, the loose shapes
//! included: a bare string for `gate`, an object for `isolation`. Those
//! are accepted, so they are declared — and the model is told about
//! them too: with `worktree_isolation` on, the schema names every
//! shape the parser takes. With that feature off the `isolation` knob
//! stays parseable while the schema drops it — the tool takes a shape
//! the model never heard of.
//!
//! Every field in the table is one the model is told about. A field
//! the model never hears of has no reader, and a field with no reader
//! is gone: the arguments that were removed are recorded in `REMOVED`
//! with the reason they are gone, and a call that still carries one
//! is refused with that reason instead of being dropped in silence.
//!
//! The **bounds** are the other half of the contract, and they are
//! aligned: the two sides state the same bound. A bound the parser
//! keeps and the schema does not state is a hole in the contract; a
//! bound the schema states and the parser does not keep is a broken
//! promise. There is no exception any more — the one promise that
//! was broken left the declaration together with the field it
//! belonged to.

use serde_json::{Map, Value, json};
use uva_provider::ToolDefinition;
use uva_tool::{ToolFeatures, ToolSchemaBuilder};

use crate::spec::TASK_TOOL_NAME;

/// The description the model reads about the `task` tool.
///
/// This is the one recorded copy of the tool's description: the
/// published definition carries these words verbatim, and the crate
/// publishes no second definition that could say something else. A
/// reader of this repository finds here exactly what the model is
/// told.
pub(crate) const DESCRIPTION: &str = "Delegate a sub-task to a registered \
    peer agent and return its final answer. Use for work that belongs \
    to a specialist (per each agent's handoff hint) or that should run \
    in isolation.";

/// One JSON shape, with the bound it puts on a value.
///
/// The bound lives inside the shape so the schema can state it and
/// the parser can enforce it from the same declaration.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Shape {
    /// A JSON string.
    String {
        /// Fewest characters the value may have.
        min_len: Option<usize>,
    },
    /// A JSON boolean.
    Boolean,
    /// JSON `null`.
    Null,
    /// A JSON object whose members are declared as fields.
    Object {
        /// The members the object carries.
        fields: &'static [Field],
    },
    /// A JSON array of strings.
    StringArray {
        /// Fewest items the array may have.
        min_items: Option<usize>,
    },
}

/// The JSON type of a shape, without its bounds.
///
/// The bounds are the promise the two sides state about a value; the
/// type is the shape they must agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A JSON string.
    String,
    /// A JSON boolean.
    Boolean,
    /// JSON `null`.
    Null,
    /// A JSON object.
    Object,
    /// A JSON array of strings.
    StringArray,
}

/// One shape the parser accepts, with the diagnostics it reports.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Accepted {
    /// The shape, with the bound the parser enforces.
    pub shape: Shape,
    /// The message when the value breaks the shape's bound.
    pub bound: Option<&'static str>,
    /// The message when an array holds an item that is not a string.
    pub item: Option<&'static str>,
}

/// What the parser does when a field is absent.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Absent {
    /// The field stays absent — the caller reads `None`.
    None,
    /// The call is refused with this message.
    Reject(&'static str),
}

/// One declared argument of the `task` tool.
///
/// A field is declared once and read twice: the schema renders the
/// `advertised` shapes, the parser reads the `accepted` ones.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Field {
    /// The wire name the model sends.
    pub name: &'static str,
    /// The model's sentence about the field — `None` when the field
    /// carries no sentence of its own (a member of an object may
    /// have none).
    pub meaning: Option<&'static str>,
    /// Whether the model must send the field.
    pub required: bool,
    /// The feature that gates the field's visibility, if any. The
    /// field stays parseable when the feature is off — only the
    /// schema drops it.
    pub feature: Option<&'static str>,
    /// The shapes the model is told about.
    pub advertised: &'static [Shape],
    /// The shapes the parser accepts.
    pub accepted: &'static [Accepted],
    /// What the parser does when the field is absent.
    pub absent: Absent,
    /// The message when the value matches none of the accepted
    /// shapes.
    pub mismatch: &'static str,
}

/// `agent` — the registered peer to run.
pub(crate) const AGENT: Field = Field {
    name: "agent",
    meaning: Some("Name of the registered peer agent to run."),
    required: true,
    feature: None,
    // The model is told the name is not empty, because the parser
    // refuses an empty one.
    advertised: &[Shape::String { min_len: Some(1) }],
    accepted: &[Accepted {
        shape: Shape::String { min_len: Some(1) },
        bound: Some("`agent` must be a non-empty agent name"),
        item: None,
    }],
    absent: Absent::Reject("task tool requires a string `agent` field"),
    mismatch: "task tool requires a string `agent` field",
};

/// `prompt` — the task the peer carries out.
pub(crate) const PROMPT: Field = Field {
    name: "prompt",
    meaning: Some("The complete, self-contained task for the sub-agent."),
    required: true,
    feature: None,
    advertised: &[Shape::String { min_len: None }],
    accepted: &[Accepted {
        shape: Shape::String { min_len: None },
        bound: None,
        item: None,
    }],
    // The schema says the prompt is a required string, so the parser
    // refuses a call that leaves it out or sends something else.
    absent: Absent::Reject("task tool requires a string `prompt` field"),
    mismatch: "task tool requires a string `prompt` field",
};

/// `gate.command` — the verification command to run against the
/// child's working tree: a program with its arguments, or a bare
/// program name.
pub(crate) const GATE_COMMAND: Field = Field {
    name: "command",
    meaning: None,
    required: true,
    feature: None,
    advertised: &[
        Shape::String { min_len: None },
        // The model is told the array carries at least one argument,
        // because the parser refuses an empty one.
        Shape::StringArray { min_items: Some(1) },
    ],
    accepted: &[
        Accepted {
            shape: Shape::String { min_len: None },
            bound: None,
            item: None,
        },
        Accepted {
            shape: Shape::StringArray { min_items: Some(1) },
            bound: Some("gate.command must contain at least one argument"),
            item: Some("gate command entries must be strings"),
        },
    ],
    absent: Absent::Reject("gate object requires a `command` field"),
    mismatch: "gate.command must be string or array",
};

/// The members of the `gate` object.
pub(crate) const GATE_FIELDS: &[Field] = &[GATE_COMMAND];

/// `gate` — the verification command run against the child's working
/// tree once it finishes.
///
/// The gate is read in three shapes: the command object, a bare
/// command string, and null (no gate at all). The model is told about
/// all three — the two that carry a command and the one that says
/// there is none.
pub(crate) const GATE: Field = Field {
    name: "gate",
    meaning: Some(
        "Optional verification command run against the child's working \
         tree after it finishes. Either an object with a `command`, or \
         the command itself as a string — one program name, no shell \
         splitting.",
    ),
    required: false,
    feature: None,
    advertised: &[
        Shape::Object {
            fields: GATE_FIELDS,
        },
        Shape::String { min_len: None },
        Shape::Null,
    ],
    accepted: &[
        Accepted {
            shape: Shape::Object {
                fields: GATE_FIELDS,
            },
            bound: None,
            item: None,
        },
        Accepted {
            shape: Shape::String { min_len: None },
            bound: None,
            item: None,
        },
        Accepted {
            shape: Shape::Null,
            bound: None,
            item: None,
        },
    ],
    absent: Absent::None,
    mismatch: "gate must be a string or object",
};

/// `isolation` — whether the child runs in a worktree of its own.
///
/// The knob is read in three shapes: the flag, an object (the
/// worktree spec carries no members of its own), and null (no
/// isolation at all). The model is told about all three.
pub(crate) const ISOLATION: Field = Field {
    name: "isolation",
    meaning: Some(
        "Run the child in a fresh detached git worktree. Edits are \
         committed to `uva/agent-<ulid>` on cleanup when the worktree \
         is dirty. Either `true` or an object — the object carries no \
         members of its own.",
    ),
    required: false,
    feature: Some("worktree_isolation"),
    advertised: &[Shape::Boolean, Shape::Object { fields: &[] }, Shape::Null],
    accepted: &[
        Accepted {
            shape: Shape::Boolean,
            bound: None,
            item: None,
        },
        Accepted {
            // Any object: the worktree spec carries no members of
            // its own.
            shape: Shape::Object { fields: &[] },
            bound: None,
            item: None,
        },
        Accepted {
            shape: Shape::Null,
            bound: None,
            item: None,
        },
    ],
    absent: Absent::None,
    // The gap before `got` is part of the message the model reads
    // today; the string is kept verbatim.
    mismatch: concat!(
        "`isolation` must be `true`, `false`, null or an object;",
        "              got {}",
    ),
};

/// Every argument of the `task` tool, in the order the model reads
/// them.
pub(crate) const ARGUMENTS: &[Field] = &[AGENT, PROMPT, GATE, ISOLATION];

/// A field that used to be an argument of the `task` tool and is
/// gone.
///
/// The record names the wire path the model used to send and says
/// why the field does not exist any more: a call that still carries
/// one is refused with that reason, instead of being dropped like
/// any other unknown key.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Removed {
    /// The wire path the model used to send: the top-level
    /// argument, and — for a member of an object — the argument it
    /// belonged to.
    pub path: &'static [&'static str],
    /// Why the field is gone.
    pub why: &'static str,
}

impl Removed {
    /// The name the model used to send the field, as the model
    /// writes it: `gate.timeout_secs`.
    fn name(&self) -> String {
        self.path.join(".")
    }

    /// The message a call that still carries the field is refused
    /// with.
    fn refusal(&self) -> String {
        format!(
            "`{}` is no longer an argument of the `task` tool: {}",
            self.name(),
            self.why,
        )
    }
}

/// The arguments that are gone, and why each one is gone.
///
/// A removed field leaves a record here, so the parser can refuse a
/// call that still carries one with a reason. The two fields below
/// were parsed for as long as they existed, and nothing read them:
/// a model that sent one got no error and no effect — it was told
/// the argument worked, and it did not.
pub(crate) const REMOVED: &[Removed] = &[
    Removed {
        path: &["trace_context"],
        why: "the tool never read it, so a trace context sent with a \
              call was parsed and then dropped",
    },
    Removed {
        path: &["gate", "timeout_secs"],
        why: "the gate never applied it, so a wall-clock budget sent \
              with a call was parsed and then dropped",
    },
];

/// The value of a field, read through its declaration.
#[derive(Clone, Debug)]
pub(crate) enum Read<'a> {
    /// The field is absent.
    Absent,
    /// A JSON string.
    Str(&'a str),
    /// A JSON boolean.
    Bool(bool),
    /// A JSON object.
    Object(&'a Map<String, Value>),
    /// An array of strings.
    Strings(Vec<&'a str>),
    /// JSON `null`.
    Null,
}

/// How a value fares against one shape.
enum Match<'a> {
    /// The value is not of this shape at all.
    No,
    /// The value is of this shape but breaks its bound.
    Broken { size: usize },
    /// The value is an array holding an item that is not a string.
    Item,
    /// The value is of this shape.
    Yes(Read<'a>),
}

impl Shape {
    /// The JSON type of the shape, without its bounds.
    pub(crate) const fn kind(self) -> Kind {
        match self {
            Shape::String { .. } => Kind::String,
            Shape::Boolean => Kind::Boolean,
            Shape::Null => Kind::Null,
            Shape::Object { .. } => Kind::Object,
            Shape::StringArray { .. } => Kind::StringArray,
        }
    }

    /// How `value` fares against this shape.
    fn matches<'a>(self, value: &'a Value) -> Match<'a> {
        match self {
            Shape::String { min_len } => {
                let Value::String(text) = value else {
                    return Match::No;
                };
                // The bound counts characters, the way the schema's
                // `minLength` counts them.
                let size = text.chars().count();
                if min_len.is_some_and(|min| size < min) {
                    return Match::Broken { size };
                }
                Match::Yes(Read::Str(text))
            }
            Shape::Boolean => match value {
                Value::Bool(flag) => Match::Yes(Read::Bool(*flag)),
                _ => Match::No,
            },
            Shape::Null => match value {
                Value::Null => Match::Yes(Read::Null),
                _ => Match::No,
            },
            Shape::Object { .. } => match value {
                Value::Object(members) => Match::Yes(Read::Object(members)),
                _ => Match::No,
            },
            Shape::StringArray { min_items } => match value {
                Value::Array(items) => {
                    let mut texts = Vec::with_capacity(items.len());
                    for item in items {
                        let Value::String(text) = item else {
                            return Match::Item;
                        };
                        texts.push(text.as_str());
                    }
                    if min_items.is_some_and(|min| texts.len() < min) {
                        return Match::Broken { size: texts.len() };
                    }
                    Match::Yes(Read::Strings(texts))
                }
                _ => Match::No,
            },
        }
    }
}

/// Read `field` out of a tool-call payload, following the
/// declaration: the accepted shapes decide what fits, the bounds
/// decide what is refused, and the field's own diagnostics decide
/// what the refusal says.
pub(crate) fn read<'a>(
    field: &Field,
    value: Option<&'a Value>,
) -> Result<Read<'a>, String> {
    let Some(value) = value else {
        return absent(field);
    };
    for accepted in field.accepted {
        match accepted.shape.matches(value) {
            Match::No => {}
            Match::Broken { size } => {
                return Err(match accepted.bound {
                    Some(template) => template.replace("{}", &size.to_string()),
                    None => refusal(field, Some(value)),
                });
            }
            Match::Item => {
                return Err(match accepted.item {
                    Some(template) => template.to_string(),
                    None => refusal(field, Some(value)),
                });
            }
            Match::Yes(read) => return Ok(read),
        }
    }
    Err(field.mismatch.replace("{}", &value.to_string()))
}

/// Read a field whose accepted shapes are a string or an array of
/// strings — the argv a command is made of.
pub(crate) fn read_argv(
    field: &Field,
    value: Option<&Value>,
) -> Result<Vec<String>, String> {
    match read(field, value)? {
        Read::Str(text) => Ok(vec![text.to_string()]),
        Read::Strings(items) => {
            Ok(items.into_iter().map(str::to_string).collect())
        }
        _ => Err(refusal(field, value)),
    }
}

/// Read `field` out of a tool-call payload, by the field's own wire
/// name.
///
/// The key comes from the declaration, so the parser never writes a
/// wire name down itself: a rename in the table moves the parser
/// with the schema.
pub(crate) fn read_named<'a>(
    field: &Field,
    payload: &'a Value,
) -> Result<Read<'a>, String> {
    read(field, payload.get(field.name))
}

/// Read `field` out of a tool-call payload, by the field's own wire
/// name, as text.
pub(crate) fn read_text_named(
    field: &Field,
    payload: &Value,
) -> Result<String, String> {
    match read_named(field, payload)? {
        Read::Str(text) => Ok(text.to_string()),
        _ => Err(refusal_named(field, payload)),
    }
}

/// Read `field` out of an object's members, by the member's own wire
/// name, as an argv.
pub(crate) fn read_argv_named(
    field: &Field,
    members: &Map<String, Value>,
) -> Result<Vec<String>, String> {
    read_argv(field, members.get(field.name))
}

/// The message the parser refuses with when the value of `field`
/// does not fit it, read out of the payload by the field's own wire
/// name.
pub(crate) fn refusal_named(field: &Field, payload: &Value) -> String {
    refusal(field, payload.get(field.name))
}

/// Refuse a call that still carries a field that is gone.
///
/// The unknown keys are dropped as they always were — a model may
/// send anything. A removed field is different: the model was told
/// about it, so a call that still carries one is refused with the
/// reason the field is gone, not dropped in silence.
pub(crate) fn refuse_removed(payload: &Value) -> Result<(), String> {
    let Some(members) = payload.as_object() else {
        return Ok(());
    };
    for removed in REMOVED {
        if carries(members, removed.path) {
            return Err(removed.refusal());
        }
    }
    Ok(())
}

/// Whether the payload carries the field at `path`.
///
/// An explicit `null` carries nothing: a model that sends the field
/// as null has not sent it, so the value never turns a call into a
/// refusal.
fn carries(members: &Map<String, Value>, path: &[&str]) -> bool {
    let Some((head, rest)) = path.split_first() else {
        return false;
    };
    let Some(value) = members.get(*head) else {
        return false;
    };
    if value.is_null() {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    let Some(inner) = value.as_object() else {
        return false;
    };
    carries(inner, rest)
}

/// What the parser reads when the field is absent.
fn absent<'a>(field: &Field) -> Result<Read<'a>, String> {
    match field.absent {
        Absent::None => Ok(Read::Absent),
        Absent::Reject(message) => Err(message.to_string()),
    }
}

/// The message the parser refuses with when `value` does not fit
/// `field`.
pub(crate) fn refusal(field: &Field, value: Option<&Value>) -> String {
    field
        .mismatch
        .replace("{}", &value.map(Value::to_string).unwrap_or_default())
}

/// The JSON Schema the model reads: the declared arguments, in the
/// declared order, with the shapes the model is told about.
pub(crate) fn input_schema(features: &ToolFeatures) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in ARGUMENTS {
        if !visible(field, features) {
            // A field behind a feature the deployment does not have
            // is not in the schema at all.
            continue;
        }
        debug_assert!(
            aligned(field),
            "the model is told a shape or a bound of `{}` the parser \
             does not keep",
            field.name,
        );
        properties.insert(field.name.to_string(), render_field(field));
        if field.required {
            required.push(Value::String(field.name.to_string()));
        }
    }
    ToolSchemaBuilder::new()
        .with("type", json!("object"))
        .with("properties", Value::Object(properties))
        .with("required", Value::Array(required))
        .build()
}

/// The `task` tool definition the model is given: the tool's name,
/// the description recorded in [`DESCRIPTION`] and the schema the
/// declared arguments render.
///
/// This is the only definition the crate publishes. The mounted tool
/// carries it, and no other builder hands out a second one — so the
/// words the model reads are the words this repository records.
pub(crate) fn tool_definition(features: &ToolFeatures) -> ToolDefinition {
    ToolDefinition {
        name: TASK_TOOL_NAME.to_string(),
        description: DESCRIPTION.to_string(),
        input_schema: input_schema(features),
        cache_control: None,
        annotations: None,
    }
}

/// Whether the two sides of a field agree: every shape the model is
/// told about is one the parser takes, both sides state the same
/// bound, and the schema's `required` is the parser's refusal to run
/// without the field.
///
/// There is no exception: a shape the model is told about and the
/// parser refuses is a hole in the contract, a bound the schema
/// states and the parser does not keep is a broken promise, and a
/// field the schema calls required that the parser reads as optional
/// is a promise the parser never made.
pub(crate) fn aligned(field: &Field) -> bool {
    let shapes_agree = field.advertised.iter().all(|advertised| {
        field
            .accepted
            .iter()
            .any(|accepted| accepted.shape.kind() == advertised.kind())
    });
    let absence_agrees =
        field.required == matches!(field.absent, Absent::Reject(_));
    shapes_agree && absence_agrees && bounds_agree(field)
}

/// Whether the two sides of `field` state the same bound, on the
/// field itself and on the members it carries.
fn bounds_agree(field: &Field) -> bool {
    let own = field.advertised.iter().all(|advertised| {
        field.accepted.iter().any(|accepted| {
            accepted.shape.kind() == advertised.kind()
                && same_bound(*advertised, accepted.shape)
        })
    });
    own && members_agree(field)
}

/// Whether the two sides state the same bound for the members of
/// `field`.
fn members_agree(field: &Field) -> bool {
    field.advertised.iter().all(|advertised| match advertised {
        Shape::Object { fields } => fields.iter().all(bounds_agree),
        _ => true,
    })
}

/// Whether the two sides state the same bound.
///
/// A bound the parser keeps and the schema does not state is a hole
/// in the contract; a bound the schema states and the parser does not
/// keep is a broken promise. Only the same bound on both sides closes
/// either one.
fn same_bound(advertised: Shape, accepted: Shape) -> bool {
    match (advertised, accepted) {
        (Shape::String { min_len: want }, Shape::String { min_len: have }) => {
            want == have
        }
        (
            Shape::StringArray { min_items: want },
            Shape::StringArray { min_items: have },
        ) => want == have,
        // The members of an object carry their own bounds; they are
        // checked where the member is read.
        _ => true,
    }
}

/// Whether the model is told about the field at all.
fn visible(field: &Field, features: &ToolFeatures) -> bool {
    match field.feature {
        Some(feature) => features.is_enabled(feature),
        None => true,
    }
}

/// The JSON Schema fragment for one declared field.
fn render_field(field: &Field) -> Value {
    match field.advertised {
        [shape] => render_shape(shape, field.meaning),
        shapes => {
            let mut fragment = Map::new();
            fragment.insert(
                "oneOf".to_string(),
                Value::Array(
                    shapes
                        .iter()
                        .map(|shape| render_shape(shape, None))
                        .collect(),
                ),
            );
            if let Some(meaning) = field.meaning {
                fragment.insert("description".to_string(), json!(meaning));
            }
            Value::Object(fragment)
        }
    }
}

/// The JSON Schema fragment for one shape, with the model's sentence
/// right after the type it names.
fn render_shape(shape: &Shape, meaning: Option<&str>) -> Value {
    let mut fragment = Map::new();
    match shape {
        Shape::String { min_len } => {
            fragment.insert("type".to_string(), json!("string"));
            describe(meaning, &mut fragment);
            if let Some(min) = min_len {
                fragment.insert("minLength".to_string(), json!(min));
            }
        }
        Shape::Boolean => {
            fragment.insert("type".to_string(), json!("boolean"));
            describe(meaning, &mut fragment);
        }
        Shape::Null => {
            fragment.insert("type".to_string(), json!("null"));
            describe(meaning, &mut fragment);
        }
        Shape::Object { fields } => {
            fragment.insert("type".to_string(), json!("object"));
            describe(meaning, &mut fragment);
            if !fields.is_empty() {
                let mut properties = Map::new();
                for field in fields.iter() {
                    properties
                        .insert(field.name.to_string(), render_field(field));
                }
                fragment.insert(
                    "properties".to_string(),
                    Value::Object(properties),
                );
                let required: Vec<Value> = fields
                    .iter()
                    .filter(|field| field.required)
                    .map(|field| Value::String(field.name.to_string()))
                    .collect();
                if !required.is_empty() {
                    fragment
                        .insert("required".to_string(), Value::Array(required));
                }
            }
        }
        Shape::StringArray { min_items } => {
            fragment.insert("type".to_string(), json!("array"));
            describe(meaning, &mut fragment);
            let mut items = Map::new();
            items.insert("type".to_string(), json!("string"));
            fragment.insert("items".to_string(), Value::Object(items));
            if let Some(min) = min_items {
                fragment.insert("minItems".to_string(), json!(min));
            }
        }
    }
    Value::Object(fragment)
}

/// Put the model's sentence into the fragment, right after the type.
fn describe(meaning: Option<&str>, fragment: &mut Map<String, Value>) {
    if let Some(meaning) = meaning {
        fragment.insert("description".to_string(), json!(meaning));
    }
}

#[cfg(test)]
mod tests;
