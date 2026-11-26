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
//! The row vocabulary is the one row type whose union is *not*
//! declared here: a row's `type` is `uva_session::RowTag` — the
//! vocabulary that owns the tag set, catalogued under the TypeScript
//! name `SessionEventType` — through the same Rust-name-to-
//! TypeScript-name rewrite the other renamed rows go through. The
//! server **publishes current rows only** (`uva_session::publish_row`
//! restores a dialect row before it reaches the wire), so a legacy
//! dialect spelling is not part of the wire's `type` at all: the
//! published union is the vocabulary's own schema, not a hand-written
//! mirror that has to be kept in step.
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
        provider::{Content, ContentPart, Message},
        scheduler::{JobKind, JobStatus},
        session::RowTag,
    };

    use crate::{
        api::v1::{
            profile::{
                ProfileChange,
                ProfileChangeState,
                ProfileRow,
                ProfileSegment,
            },
            session_view::{SessionArtifact, SessionPart, SessionTurn},
        },
        composition::kernel::RowAudit,
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
        session::{
            controller::{LiveToolCall, LiveTurn},
            status::SessionStatus,
        },
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
        // The AG-UI projection: the web front-end's protocol-shaped
        // door. The event travels on the `agui/event` notification,
        // and its type is the protocol's own — the browser reads the
        // same vocabulary the server publishes.
        response::<crate::adapters::agui::AguiEvent>(
            "agui/event",
            "AguiEvent",
            "AguiEvent",
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
        response::<SessionStatus>(
            "sessions/list",
            "SessionStatus",
            "SessionStatus",
        ),
        response::<SessionDetail>(
            "sessions/get",
            "SessionDetail",
            "SessionDetail",
        ),
        response::<SessionTurn>("sessions/get", "SessionTurn", "SessionTurn"),
        response::<RowTag>("sessions/get", "SessionEventType", "RowTag"),
        // The provider's own message shapes: a message row's payload
        // IS the provider `Message`, so the browser reads the same
        // types the provider serializes instead of re-guessing the
        // envelope. One declaration — generated into the contract,
        // not hand-written twice.
        response::<Message>("sessions/get", "Message", "Message"),
        response::<Content>("sessions/get", "Content", "Content"),
        response::<ContentPart>("sessions/get", "ContentPart", "ContentPart"),
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
        response::<RowAudit>("profile/audit", "RowAudit", "RowAudit"),
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
mod tests;
