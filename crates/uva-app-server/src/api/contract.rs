//! The frontend's wire contract: one explicit catalog, one JSON Schema
//! document, one generated TypeScript file.
//!
//! `uva-web/src/api/types.ts` is generated, not written: for every type
//! the catalog names, the browser's definition is derived from the type
//! this server serializes, so the two cannot disagree about a field
//! name, a nullability, or a closed set of wire values. Types the
//! catalog does not name are not claimed — see
//! [What the catalog cannot say](#what-the-catalog-cannot-say).
//!
//! # The catalog
//!
//! [`catalog`] is the single place the mapping lives — one row per
//! exported TypeScript symbol, naming
//!
//! - the RPC method (or HTTP route) the type answers on,
//! - the TypeScript export name, and
//! - the Rust type it is derived from.
//!
//! A row is built by [`response`] (the type is *serialized* by the
//! server) or [`request`] (the type is *deserialized* by the server).
//! The two differ in the presence rules [`schemars`] derives: a
//! response's optional field is one the writer omits
//! (`skip_serializing_if`), a request's optional field is one the
//! reader can live without (`default`). Stating the direction per row
//! is what keeps `compact`'s always-present `start: null` and
//! `schedule/create`'s omittable `payload` apart.
//!
//! # The document
//!
//! [`document`] emits JSON Schema draft-07 — the dialect
//! `json-schema-to-typescript` compiles — with one `definitions` entry
//! per catalog row (plus every type those rows reach), keyed by the
//! **TypeScript** name. Definitions are rewritten to the TypeScript
//! name when the Rust one differs (`SessionHeader` → `FollowHeader`),
//! and every `$ref` follows the rename, so the generated file has no
//! `SessionHeader` alias hanging off a `FollowHeader` the frontend can
//! see.
//!
//! The emitted document is deterministic: `serde_json`'s `Map` is a
//! `BTreeMap` (keys sorted), no timestamps are emitted, and the only
//! paths in it are `#/definitions/…` pointers. Regenerating therefore
//! yields the same bytes, which is what the `check-web-contract` gate
//! diffs.
//!
//! # Regenerating
//!
//! ```text
//! cargo run -q -p uva-app-server --example emit-contract   # the schema
//! cd uva-web && npm run generate:types                     # both steps
//! ```
//!
//! # What the catalog cannot say
//!
//! JSON Schema has no type parameters, so the generic list envelope
//! ([`super::List<T>`]) is the one row whose TypeScript form the
//! generator renders itself: its entry carries
//! [`GENERIC_PARAMETER`] and the generator turns the unconstrained
//! element type into that parameter. Every other row is emitted
//! verbatim.
//!
//! Four rows are *views* of untyped JSON — a session log row, a legacy
//! attachment, and its parts — declared in
//! [`super::v1::session_view`] because the server writes those arrays as
//! `serde_json::Value`. The `#[schemars(with = …)]` attributes on the
//! three response fields are the join.
//!
//! Two kinds of frontend type are deliberately outside the catalog, and
//! the header above does not claim otherwise: RPC *request envelopes*
//! the browser sends (`ApprovalRequestParams`, `SendChatMessageOptions`,
//! …) and *streaming frame* shapes (`chat-stream.ts`,
//! `follow-frames.ts`). Both describe the transport protocol rather than
//! a value the server serializes as a response, so there is no Rust
//! type to derive them from; they stay hand-written in the domain
//! clients. `check-web-contract` compares only the types this catalog
//! names — it is blind to those modules, by design.

use std::collections::BTreeMap;

use schemars::{
    JsonSchema,
    generate::{Contract, SchemaSettings},
};
use serde_json::{Map, Value, json};

use super::List;

/// The TypeScript type parameter the generated list envelope is
/// generic over.
pub const GENERIC_PARAMETER: &str = "T";

/// The `title` of the emitted document, and the name the generator
/// strips its synthetic root alias by.
pub const CONTRACT_TITLE: &str = "UvaWebContract";

/// The draft-07 meta-schema URI the document declares.
const META_SCHEMA: &str = "http://json-schema.org/draft-07/schema#";

/// One row of the catalog.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// The RPC method or HTTP route that answers with (or accepts)
    /// the type.
    pub method: &'static str,
    /// The TypeScript export name.
    pub ts_name: &'static str,
    /// The Rust type's name, for the catalog's documentation half.
    pub rust_type: &'static str,
    /// The TypeScript type parameter, for the one generic row.
    pub generic: Option<&'static str>,
    /// The type's draft-07 schema, with its `definitions` split out.
    schema: Value,
    /// The schemas `schema` referenced, keyed by Rust type name.
    deps: Map<String, Value>,
}

/// A catalog row for a type the server **serializes**.
#[must_use]
pub fn response<T: JsonSchema>(
    method: &'static str,
    ts_name: &'static str,
    rust_type: &'static str,
) -> CatalogEntry {
    build::<T>(method, ts_name, rust_type, Contract::Serialize, None)
}

/// A catalog row for a type the server **deserializes**.
#[must_use]
pub fn request<T: JsonSchema>(
    method: &'static str,
    ts_name: &'static str,
    rust_type: &'static str,
) -> CatalogEntry {
    build::<T>(method, ts_name, rust_type, Contract::Deserialize, None)
}

/// A catalog row whose TypeScript form is generic over
/// [`GENERIC_PARAMETER`].
#[must_use]
pub fn generic_response<T: JsonSchema>(
    method: &'static str,
    ts_name: &'static str,
    rust_type: &'static str,
) -> CatalogEntry {
    build::<T>(
        method,
        ts_name,
        rust_type,
        Contract::Serialize,
        Some(GENERIC_PARAMETER),
    )
}

fn build<T: JsonSchema>(
    method: &'static str,
    ts_name: &'static str,
    rust_type: &'static str,
    contract: Contract,
    generic: Option<&'static str>,
) -> CatalogEntry {
    let mut settings = SchemaSettings::draft07();
    settings.contract = contract;
    let root = settings.into_generator().into_root_schema_for::<T>();
    let mut object = match root.to_value() {
        Value::Object(object) => object,
        other => {
            // A boolean schema (`true`) has no members to split; keep
            // it whole so the catalog still describes the row.
            let mut object = Map::new();
            object.insert("enum".to_owned(), other);
            object
        }
    };
    // One document, one `$schema`: the outer document states the
    // dialect. A per-definition title would name the *Rust* type, and
    // the generator names interfaces by their definition key.
    object.remove("$schema");
    object.remove("title");
    let deps = match object.remove("definitions") {
        Some(Value::Object(deps)) => deps,
        _ => Map::new(),
    };
    CatalogEntry {
        method,
        ts_name,
        rust_type,
        generic,
        schema: Value::Object(object),
        deps,
    }
}

/// Every type `uva-web/src/api/types.ts` exports, in documentation
/// order: resources, then the session surface, then the domains.
#[must_use]
pub fn catalog() -> Vec<CatalogEntry> {
    use uva::{
        approval::{
            ApprovalRequest,
            ResumeDecision,
            ResumeStatus,
            ResumeToken,
        },
        eval::{EvalReport, EvalSuite},
        scheduler::{JobKind, JobStatus},
    };

    use crate::{
        api::v1::{
            profile::{
                ProfileChange,
                ProfileChangeState,
                ProfileRow,
                ProfileSegment,
            },
            session_view::{
                SessionArtifact,
                SessionEventType,
                SessionPart,
                SessionTurn,
            },
        },
        routes::{
            agents::AgentDetail,
            chat::{Delivery, SendMessageResponse},
            follow::{
                CompactResponse,
                ContextMeter,
                FollowSnapshot,
                SessionHeader,
                SessionUsageResponse,
            },
            health::{ModelEntry, ModelsResponse},
            schedules::{
                CreateScheduleRequest,
                ScheduleDetail,
                ScheduleInfo,
                ScheduleTickResponse,
                UpdateScheduleRequest,
            },
            search::SearchHit,
            sessions::{ForkSessionResponse, SessionDetail, SessionSummary},
            skills::{
                CreateSkillResponse,
                ReloadSkillsResponse,
                SkillDetail,
                SkillInfo,
            },
            tasks::TaskDelegation,
            tool::{ToolDetail, ToolInfo},
            ui::UiSchemaResponse,
        },
        session::controller::{LiveToolCall, LiveTurn},
        ui::{
            schema::{UiComponent, UiSchema},
            tree::{Cardinality, SlotInfo},
        },
    };

    vec![
        // ---- resources ------------------------------------------
        response::<SkillInfo>("skills/list", "Skill", "SkillInfo"),
        response::<SkillDetail>("skills/get", "SkillDetail", "SkillDetail"),
        response::<CreateSkillResponse>(
            "skills/create",
            "CreateSkillResponse",
            "CreateSkillResponse",
        ),
        response::<ReloadSkillsResponse>(
            "skills/reload",
            "ReloadSkillsResponse",
            "ReloadSkillsResponse",
        ),
        response::<ToolInfo>("tools/list", "Tool", "ToolInfo"),
        response::<ToolDetail>("tools/get", "ToolDetail", "ToolDetail"),
        response::<AgentDetail>("agents/get", "AgentDetail", "AgentDetail"),
        response::<SearchHit>("search/all", "SearchHit", "SearchHit"),
        // ---- sessions -------------------------------------------
        generic_response::<List<Value>>("*/list", "List", "List"),
        response::<SessionSummary>(
            "sessions/list",
            "SessionSummary",
            "SessionSummary",
        ),
        response::<SessionDetail>(
            "sessions/get",
            "SessionDetail",
            "SessionDetail",
        ),
        response::<SessionTurn>("sessions/get", "SessionTurn", "SessionTurn"),
        response::<SessionEventType>(
            "sessions/get",
            "SessionEventType",
            "SessionEventType",
        ),
        response::<SessionArtifact>(
            "sessions/get",
            "SessionArtifact",
            "SessionArtifact",
        ),
        response::<SessionPart>("sessions/get", "SessionPart", "SessionPart"),
        response::<ForkSessionResponse>(
            "sessions/fork",
            "ForkSessionResponse",
            "ForkSessionResponse",
        ),
        response::<SessionHeader>(
            "sessions/follow",
            "FollowHeader",
            "SessionHeader",
        ),
        response::<ContextMeter>(
            "sessions/follow",
            "SessionContextMeter",
            "ContextMeter",
        ),
        response::<LiveTurn>("sessions/follow", "LiveTurn", "LiveTurn"),
        response::<LiveToolCall>(
            "sessions/follow",
            "LiveToolCall",
            "LiveToolCall",
        ),
        response::<ApprovalRequest>(
            "sessions/follow",
            "ApprovalRequest",
            "ApprovalRequest",
        ),
        response::<FollowSnapshot>(
            "sessions/follow",
            "FollowSnapshot",
            "FollowSnapshot",
        ),
        response::<ResumeToken>(
            "sessions/resume",
            "ResumeToken",
            "ResumeToken",
        ),
        response::<ResumeStatus>(
            "sessions/resume",
            "ResumeStatus",
            "ResumeStatus",
        ),
        response::<ResumeDecision>(
            "sessions/resume",
            "ResumeDecision",
            "ResumeDecision",
        ),
        response::<SessionUsageResponse>(
            "sessions/usage",
            "SessionUsage",
            "SessionUsageResponse",
        ),
        response::<CompactResponse>(
            "sessions/compact",
            "CompactResult",
            "CompactResponse",
        ),
        // ---- catalogue + ui schema -----------------------------
        response::<ModelsResponse>(
            "models/list",
            "ModelsResponse",
            "ModelsResponse",
        ),
        response::<ModelEntry>("models/list", "ModelEntry", "ModelEntry"),
        response::<UiSchemaResponse>(
            "ui/schema",
            "UiSchemaResponse",
            "UiSchemaResponse",
        ),
        response::<SlotInfo>("ui/schema", "UiSlot", "SlotInfo"),
        response::<UiComponent>("ui/schema", "UiComponent", "UiComponent"),
        response::<UiSchema>("ui/schema", "UiSchema", "UiSchema"),
        response::<Cardinality>("ui/schema", "Cardinality", "Cardinality"),
        // ---- chat ----------------------------------------------
        response::<Delivery>("chat/message/send", "SteerDelivery", "Delivery"),
        response::<SendMessageResponse>(
            "chat/message/send",
            "SendChatMessageResponse",
            "SendMessageResponse",
        ),
        // ---- schedules -----------------------------------------
        response::<ScheduleInfo>(
            "schedules/list",
            "ScheduleSummary",
            "ScheduleInfo",
        ),
        response::<ScheduleDetail>(
            "schedules/get",
            "Schedule",
            "ScheduleDetail",
        ),
        response::<JobKind>("schedules/*", "JobKind", "JobKind"),
        response::<JobStatus>("schedules/*", "JobStatus", "JobStatus"),
        response::<ScheduleTickResponse>(
            "schedules/tick",
            "ScheduleTick",
            "ScheduleTickResponse",
        ),
        request::<CreateScheduleRequest>(
            "schedules/create",
            "CreateScheduleRequest",
            "CreateScheduleRequest",
        ),
        request::<UpdateScheduleRequest>(
            "schedules/patch",
            "UpdateScheduleRequest",
            "UpdateScheduleRequest",
        ),
        // ---- evals ---------------------------------------------
        response::<EvalSuite>("evals/get", "EvalSuite", "EvalSuite"),
        response::<EvalReport>("evals/run", "EvalReport", "EvalReport"),
        // ---- tasks ---------------------------------------------
        response::<TaskDelegation>(
            "tasks/list",
            "TaskDelegation",
            "TaskDelegation",
        ),
        // ---- profile -------------------------------------------
        response::<ProfileRow>("profile/list", "ProfileRow", "ProfileRow"),
        response::<ProfileSegment>(
            "profile/list",
            "ProfileSegment",
            "ProfileSegment",
        ),
        response::<ProfileChange>(
            "profile/set",
            "ProfileChange",
            "ProfileChange",
        ),
        response::<ProfileChangeState>(
            "profile/set",
            "ProfileChangeState",
            "ProfileChangeState",
        ),
        response::<crate::api::v1::profile::ProfileChangedEvent>(
            "profile/changed",
            "ProfileChangedEvent",
            "ProfileChangedEvent",
        ),
    ]
}

/// The catalog as the JSON Schema document the TypeScript generator
/// compiles.
///
/// The document carries the catalog itself under `x-uva-catalog`
/// (`method`, `tsName`, `rustType`, `generic`) so a reader of the
/// emitted file can see where each interface came from; the generator
/// drops the extension before compiling, because it is not schema
/// vocabulary.
#[must_use]
pub fn document() -> Value {
    let entries = catalog();
    let renames = renames(&entries);

    let mut definitions = BTreeMap::new();
    for entry in &entries {
        for (rust_name, schema) in &entry.deps {
            let name = renames
                .get(rust_name.as_str())
                .map(|ts| (*ts).to_owned())
                .unwrap_or_else(|| rust_name.clone());
            definitions.insert(name, schema.clone());
        }
        definitions.insert(entry.ts_name.to_owned(), entry.schema.clone());
    }

    let mut definitions = definitions
        .into_iter()
        .map(|(name, schema)| (name, rewrite_refs(schema, &renames)))
        .collect::<BTreeMap<_, _>>();

    let one_of = definitions
        .keys()
        .map(|name| json!({ "$ref": format!("#/definitions/{name}") }))
        .collect::<Vec<_>>();
    let mut meta = Vec::with_capacity(entries.len());
    for entry in &entries {
        meta.push(json!({
            "method": entry.method,
            "tsName": entry.ts_name,
            "rustType": entry.rust_type,
            "generic": entry.generic,
        }));
    }

    let mut root = Map::new();
    root.insert("$schema".to_owned(), json!(META_SCHEMA));
    root.insert("title".to_owned(), json!(CONTRACT_TITLE));
    root.insert("oneOf".to_owned(), Value::Array(one_of));
    root.insert(
        "definitions".to_owned(),
        Value::Object(std::mem::take(&mut definitions).into_iter().collect()),
    );
    root.insert("x-uva-catalog".to_owned(), Value::Array(meta));
    Value::Object(root)
}

/// The document serialized as the example writes it: pretty-printed,
/// key-sorted (`serde_json::Map` is a `BTreeMap`), trailing newline.
///
/// # Panics
///
/// Panics only if `serde_json` cannot serialize its own `Value`, which
/// cannot happen.
#[must_use]
pub fn document_json() -> String {
    let mut text = serde_json::to_string_pretty(&document())
        .expect("a Value serializes to JSON");
    text.push('\n');
    text
}

/// `Rust type name -> TypeScript name`, for the rows whose two names
/// differ.
fn renames(entries: &[CatalogEntry]) -> BTreeMap<&str, &str> {
    entries
        .iter()
        .filter(|entry| entry.rust_type != entry.ts_name)
        .map(|entry| (entry.rust_type, entry.ts_name))
        .collect()
}

/// Rewrite every `$ref` under `value` through `renames`.
fn rewrite_refs(value: Value, renames: &BTreeMap<&str, &str>) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    if key == "$ref" {
                        (key, rewrite_ref(value, renames))
                    } else {
                        (key, rewrite_refs(value, renames))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| rewrite_refs(item, renames))
                .collect(),
        ),
        other => other,
    }
}

fn rewrite_ref(value: Value, renames: &BTreeMap<&str, &str>) -> Value {
    let Some(pointer) = value.as_str() else {
        return value;
    };
    let Some(name) = pointer.strip_prefix("#/definitions/") else {
        return value;
    };
    match renames.get(name) {
        Some(ts_name) => json!(format!("#/definitions/{ts_name}")),
        None => value,
    }
}

#[cfg(test)]
mod tests {
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
    fn the_catalogued_event_type_union_is_the_session_vocabulary() {
        // The union the browser's exhaustive row accounting dispatches
        // on must be exactly `KNOWN_SESSION_EVENT_TYPES` — the list
        // `uva-session` derives its own invariants from. A tag added
        // to one and not the other fails here.
        let entry = catalog()
            .into_iter()
            .find(|entry| entry.ts_name == "SessionEventType")
            .expect("the vocabulary is catalogued");
        let emitted: BTreeSet<&str> = entry.schema["oneOf"]
            .as_array()
            .expect("the union is a closed string vocabulary")
            .iter()
            .filter_map(|variant| variant["const"].as_str())
            .collect();
        let known: BTreeSet<&str> = uva::session::KNOWN_SESSION_EVENT_TYPES
            .iter()
            .copied()
            .collect();
        assert_eq!(
            emitted, known,
            "SessionEventType drifted from KNOWN_SESSION_EVENT_TYPES"
        );
    }

    #[test]
    fn a_session_turn_types_its_row_over_the_catalogued_union() {
        // `SessionTurn.type` must be the union, not an open string:
        // the whole point of cataloguing the vocabulary is that the
        // generated `SessionTurn` closes the field the renderer
        // dispatches on.
        let document = document();
        let reference =
            document["definitions"]["SessionTurn"]["properties"]["type"]
                .to_string();
        assert!(
            reference.contains("\"$ref\":\"#/definitions/SessionEventType\""),
            "`type` does not reference the union: {reference}"
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
            .filter(|name| {
                !catalogued.contains(name) && !reached.contains(*name)
            })
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
                        && let Some(name) =
                            pointer.strip_prefix("#/definitions/")
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
}
