//! The profile wire surface: rows, receipts, and the changed event.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::profile::{
    ChangeOutcome,
    ChangeState,
    IsolateLabel,
    Manifest,
    ProfileDiff,
};

/// One row of the resolved profile, as `profile/list` returns them and
/// `profile/get` returns one of.
///
/// The row is the settings page's data source: the interface is the
/// resolved profile read backwards (D26). `inject` is the row's
/// **effective** dependency set — the plugin manifest's default unioned
/// with the row's declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileRow {
    /// The row's stable id.
    pub id: String,
    /// Which assembly segment the row sits in.
    pub segment: ProfileSegment,
    /// The plugin the row loads (empty for a group row).
    pub name: String,
    /// Whether the row is a nested group.
    pub group: bool,
    /// Whether the row (and its descendants, for a group) stays out of
    /// the assembly.
    pub disabled: bool,
    /// The row's final configuration (a group row carries none).
    pub config: Value,
    /// Host capabilities the row grants.
    pub capabilities: Vec<String>,
    /// The effective dependency set.
    pub inject: Vec<String>,
    /// Per-service intercept config, when the row declares any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intercept: Option<Value>,
    /// The row's isolation declarations, wire-shaped
    /// (`{ service: true | "label" }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolate: Option<Value>,
    /// `mounted`, or `off: <reason>` when the assembly skipped it.
    pub status: String,
    /// The kernel's plugin state, when the row is mounted.
    pub plugin_state: Option<String>,
}

impl ProfileRow {
    /// The wire row for one resolved profile row.
    ///
    /// `inject` is the effective set (factory manifest ∪ row
    /// declaration), as the kernel orders and binds it. `status` is
    /// the assembly's verdict (`mounted`, or `off: <reason>` for a row
    /// the assembly skipped); `plugin_state` is the kernel's state for
    /// the row, when it is mounted.
    #[must_use]
    pub fn new(
        segment: ProfileSegment,
        row: &Manifest,
        inject: Vec<String>,
        status: String,
        plugin_state: Option<String>,
    ) -> Self {
        Self {
            id: row.id.clone(),
            segment,
            name: row.name.clone(),
            group: row.group,
            disabled: row.disabled,
            config: row.config.clone(),
            capabilities: row
                .capabilities
                .iter()
                .map(|capability| capability.to_string())
                .collect(),
            inject,
            intercept: row.intercept.as_ref().map(|intercept| {
                serde_json::to_value(intercept).unwrap_or(Value::Null)
            }),
            isolate: row.isolate.as_ref().map(|isolate| {
                let wire: serde_json::Map<String, Value> = isolate
                    .iter()
                    .map(|(service, label)| {
                        let value = match label {
                            IsolateLabel::Local => Value::Bool(true),
                            IsolateLabel::Shared(label) => {
                                Value::String(label.clone())
                            }
                        };
                        (service.clone(), value)
                    })
                    .collect();
                Value::Object(wire)
            }),
            status,
            plugin_state,
        }
    }
}

/// Which assembly segment a profile row sits in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ProfileSegment {
    /// The core loader's segment.
    Core,
    /// The frontend loader's segment.
    Ui,
}

/// The four outcomes of one profile transaction (D37).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileChangeState {
    /// Committed and mounted; the next turn uses the new tree.
    Applied,
    /// Committed but supervision needs a restart (the `mcp` row).
    RestartRequired,
    /// A higher layer owns the requested field; nothing was written or
    /// mounted.
    Overridden,
    /// The transaction failed: file unchanged, tree unchanged.
    Failed,
}

/// The transaction's report — the answer of `profile/set`,
/// `profile/create`, and `profile/delete`.
///
/// A caller must render the receipt, never assume success: the four
/// states say what actually happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileChange {
    /// The outcome state.
    pub state: ProfileChangeState,
    /// Why: the diff's affected ids, the override layer, or the error.
    pub reasons: Vec<String>,
    /// The mount diff, when the change reached the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<ProfileDiff>,
}

impl From<ChangeState> for ProfileChangeState {
    fn from(state: ChangeState) -> Self {
        match state {
            ChangeState::Applied => Self::Applied,
            ChangeState::RestartRequired => Self::RestartRequired,
            ChangeState::Overridden => Self::Overridden,
            ChangeState::Failed => Self::Failed,
        }
    }
}

impl From<ChangeOutcome> for ProfileChange {
    fn from(outcome: ChangeOutcome) -> Self {
        Self {
            state: outcome.state.into(),
            reasons: outcome.reasons,
            diff: outcome.diff,
        }
    }
}

/// The `profile/changed` broadcast's payload.
///
/// A committed write (one that reached the tree) is announced on this
/// unscoped notification so every client re-reads the row it changed.
/// `state` is the transaction's outcome, `ids` the rows it touched, and
/// `at` the instant the broadcast was made.
///
/// The type exists so the notification has the same named shape the
/// five methods answer with: the emitter derives a schema from it and
/// the frontend subscribes with the generated type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileChangedEvent {
    /// Who asked for the write (`"ui"` on this transport, an agent
    /// name through the self-MCP tool).
    pub actor: String,
    /// The transaction's outcome.
    pub state: ProfileChangeState,
    /// The ids the write touched.
    pub ids: Vec<String>,
    /// When the write committed.
    pub at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(inject: Option<Vec<String>>) -> Manifest {
        Manifest {
            id: "skills".to_owned(),
            name: "skills".to_owned(),
            config: serde_json::json!({ "limit": 3 }),
            capabilities: Vec::new(),
            group: false,
            children: Vec::new(),
            disabled: false,
            inject,
            intercept: None,
            isolate: None,
        }
    }

    #[test]
    fn a_row_carries_the_domain_entry() {
        let row = ProfileRow::new(
            ProfileSegment::Core,
            &row(Some(vec!["tools".to_owned()])),
            vec!["tools".to_owned()],
            "mounted".to_owned(),
            Some("Mounted".to_owned()),
        );
        assert_eq!(row.id, "skills");
        assert_eq!(row.segment, ProfileSegment::Core);
        assert_eq!(row.name, "skills");
        assert!(!row.disabled);
        assert_eq!(row.config, serde_json::json!({ "limit": 3 }));
        assert_eq!(row.inject, vec!["tools".to_owned()]);
        assert_eq!(row.status, "mounted");
        assert_eq!(row.plugin_state.as_deref(), Some("Mounted"));
    }

    #[test]
    fn a_row_serializes_the_shape_the_frontend_reads() {
        let mut entry = row(None);
        entry.disabled = true;
        let wire = ProfileRow::new(
            ProfileSegment::Ui,
            &entry,
            Vec::new(),
            "off: disabled".to_owned(),
            None,
        );
        let json = serde_json::to_value(&wire).expect("the row serialises");
        assert_eq!(
            json,
            serde_json::json!({
                "id": "skills",
                "segment": "ui",
                "name": "skills",
                "group": false,
                "disabled": true,
                "config": { "limit": 3 },
                "capabilities": [],
                "inject": [],
                "status": "off: disabled",
                "plugin_state": null,
            })
        );
    }

    #[test]
    fn a_receipt_keeps_the_state_and_its_reasons() {
        let change = ProfileChange {
            state: ProfileChangeState::RestartRequired,
            reasons: vec!["the `mcp` row changed".to_owned()],
            diff: None,
        };
        let json =
            serde_json::to_value(&change).expect("the receipt serialises");
        assert_eq!(
            json,
            serde_json::json!({
                "state": "restart-required",
                "reasons": ["the `mcp` row changed"],
            })
        );
    }

    #[test]
    fn every_type_has_a_schema() {
        for schema in [
            schemars::schema_for!(ProfileRow),
            schemars::schema_for!(ProfileChange),
        ] {
            let json = serde_json::to_value(&schema).expect("a schema");
            assert!(
                json.get("properties").is_some(),
                "the schema names its properties: {json}"
            );
        }
    }
}
