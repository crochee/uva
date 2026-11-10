//! Transactional profile writes: document-layer editing, serialization,
//! and the in-memory layer-stack re-resolution.
//!
//! Everything here is a pure function over paths and profiles; the
//! `AppState::profile_apply` orchestration composes it with the live
//! mount. The write touches **only the document layer** (`doc`): the
//! bundle patch, home patch, and CLI overlay stay authoritative, which
//! is what makes the `overridden` outcome possible.
//!
//! # Known cost
//!
//! A write re-serialises the whole document from the resolved model, so
//! operator comments and hand formatting in `profile.toml` are lost.
//! The alternative — surgical text edits — cannot express a row's
//! whole-config replacement without a TOML-aware editor, and the
//! document is a generated artefact of the settings surface (D26).

use std::{
    path::{Path, PathBuf},
    process,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Enabled,
    Entry,
    Patch,
    Profile,
    ProfileDiff,
    ProfileError,
    ProfileSource,
    model::runtime_name,
    resolve,
};

/// The four layer paths a write must respect (from the CLI inputs).
#[derive(Debug, Clone)]
pub struct ProfileStore {
    /// The bundle patch layer, if the deployment declares one.
    pub bundle: Option<PathBuf>,
    /// The profile document layer — the only layer a write touches.
    pub doc: PathBuf,
    /// The home patch layer, if a home-level patch exists.
    pub home: Option<PathBuf>,
    /// The CLI overlay layer.
    pub overlay: Option<PathBuf>,
}

/// One requested row edit.
#[derive(Debug, Clone, PartialEq)]
pub enum RowEdit {
    /// Flip a row's enablement.
    SetEnabled {
        /// The row to address.
        id: String,
        /// The new enablement.
        enabled: Enabled,
    },
    /// Replace a row's configuration whole.
    SetConfig {
        /// The row to address.
        id: String,
        /// The new configuration object.
        config: Value,
    },
    /// Change either or both writable fields in one transaction — the
    /// shape the JSON-RPC / MCP `set` action takes.
    Set {
        /// The row to address.
        id: String,
        /// The new enablement, when supplied.
        enabled: Option<Enabled>,
        /// The new configuration object, when supplied.
        config: Option<Value>,
    },
}

impl RowEdit {
    /// The row this edit addresses.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::SetEnabled { id, .. }
            | Self::SetConfig { id, .. }
            | Self::Set { id, .. } => id,
        }
    }

    /// Whether `resolved` already carries the requested value for this
    /// edit's row. A mismatch after the layer stack is re-applied means
    /// a higher layer owns the field (`overridden`).
    #[must_use]
    pub fn is_satisfied_by(&self, resolved: &Profile) -> bool {
        let Some(entry) = resolved.find(self.id()) else {
            return false;
        };
        match self {
            Self::SetEnabled { enabled, .. } => entry.enabled == *enabled,
            Self::SetConfig { config, .. } => entry.config == *config,
            Self::Set {
                enabled, config, ..
            } => {
                enabled
                    .as_ref()
                    .is_none_or(|enabled| entry.enabled == *enabled)
                    && config
                        .as_ref()
                        .is_none_or(|config| entry.config == *config)
            }
        }
    }
}

/// A capability's domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityDomain {
    /// An agent definition (`.agents/agents/<name>/agent.md`).
    Agent,
    /// A skill definition (`.agents/skills/<name>/SKILL.md`).
    Skill,
}

/// One capability write.
#[derive(Debug, Clone)]
pub enum CapabilityAction {
    /// Write the layout data file, then bring the mounted capability
    /// up to date.
    Create {
        /// Which definition domain.
        domain: CapabilityDomain,
        /// The definition payload: an `AgentDescriptor` for
        /// [`CapabilityDomain::Agent`], `{ name, content }` for
        /// [`CapabilityDomain::Skill`].
        definition: Value,
    },
    /// Remove the layout data file and the live capability.
    Delete {
        /// Which definition domain.
        domain: CapabilityDomain,
        /// The definition's name.
        id: String,
    },
}

/// One requested profile action, from either write surface.
#[derive(Debug, Clone)]
pub enum ProfileAction {
    /// A row-level edit through the document transaction.
    Edit(RowEdit),
    /// A capability create/delete.
    Capability(CapabilityAction),
}

/// The four outcomes of one transaction (D37).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeState {
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

/// The transaction's report, serialized onto both the JSON-RPC result
/// and the self-MCP tool output.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeOutcome {
    /// The outcome state.
    pub state: ChangeState,
    /// Why: the diff's affected ids, the override layer, or the error.
    pub reasons: Vec<String>,
    /// The mount diff, when the change reached the tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<ProfileDiff>,
}

impl ChangeOutcome {
    /// The change landed and the tree follows.
    #[must_use]
    pub fn applied(reason: impl Into<String>) -> Self {
        Self {
            state: ChangeState::Applied,
            reasons: vec![reason.into()],
            diff: None,
        }
    }

    /// The change landed but supervision needs a restart.
    #[must_use]
    pub fn restart_required(reason: impl Into<String>) -> Self {
        Self {
            state: ChangeState::RestartRequired,
            reasons: vec![reason.into()],
            diff: None,
        }
    }

    /// A higher layer owns the requested field.
    #[must_use]
    pub fn overridden(reason: impl Into<String>) -> Self {
        Self {
            state: ChangeState::Overridden,
            reasons: vec![reason.into()],
            diff: None,
        }
    }

    /// The transaction failed.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            state: ChangeState::Failed,
            reasons: vec![reason.into()],
            diff: None,
        }
    }

    /// Attach the diff and mark the outcome applied.
    #[must_use]
    pub fn with_diff(mut self, diff: ProfileDiff) -> Self {
        self.diff = Some(diff);
        self
    }
}

impl ProfileStore {
    /// Read the document layer, materializing `fallback` when the file
    /// does not exist yet.
    ///
    /// # Errors
    ///
    /// A file that exists but does not parse is an error; a missing
    /// file is not.
    pub fn load_doc(
        &self,
        fallback: &Profile,
    ) -> Result<Profile, ProfileError> {
        if self.doc.exists() {
            Profile::from_path(&self.doc)
        } else {
            Ok(fallback.clone())
        }
    }

    /// Resolve the layer stack in memory, with `doc` as the document
    /// layer.
    ///
    /// # Errors
    ///
    /// The parse / resolution errors of the non-document layers.
    pub fn resolve_with(&self, doc: &Profile) -> Result<Profile, ProfileError> {
        let mut layers = Vec::new();
        if let Some(bundle) = &self.bundle {
            layers.push(ProfileSource::Patch(Patch::from_path(bundle)?));
        }
        layers.push(ProfileSource::Profile(doc.clone()));
        if let Some(home) = &self.home {
            layers.push(ProfileSource::Patch(Patch::from_path(home)?));
        }
        let overlay = self
            .overlay
            .as_ref()
            .map(|path| Patch::from_path(path))
            .transpose()?;
        resolve(&layers, overlay)
    }

    /// Apply one edit to a document.
    ///
    /// # Errors
    ///
    /// Returns a message when the row does not exist.
    pub fn apply_edit(doc: &mut Profile, edit: &RowEdit) -> Result<(), String> {
        let id = edit.id();
        let Some((segment, index)) = doc.position(id) else {
            return Err(format!("the profile has no row `{id}`"));
        };
        let target = match segment {
            super::model::Segment::Core => &mut doc.core[index],
            super::model::Segment::Ui => &mut doc.ui[index],
        };
        match edit {
            RowEdit::SetEnabled { enabled, .. } => {
                target.enabled = enabled.clone();
            }
            RowEdit::SetConfig { config, .. } => {
                target.config = config.clone();
            }
            RowEdit::Set {
                enabled, config, ..
            } => {
                if let Some(enabled) = enabled {
                    target.enabled = enabled.clone();
                }
                if let Some(config) = config {
                    target.config = config.clone();
                }
            }
        }
        Ok(())
    }

    /// Serialize a profile as a TOML document.
    ///
    /// `On` is omitted (the wire default) and null object members are
    /// dropped — TOML has no null. A null inside an array or a
    /// mixed-type array is an error: TOML cannot express either
    /// faithfully, and writing a lossy document would make the next
    /// boot read something the operator never wrote.
    ///
    /// # Errors
    ///
    /// Returns a message when a value has no TOML form.
    pub fn to_toml(profile: &Profile) -> Result<String, String> {
        let mut root = toml::value::Table::new();
        root.insert("hmr".to_string(), toml::Value::Boolean(profile.hmr));
        root.insert(
            "core".to_string(),
            toml::Value::Table(section(profile, &profile.core)?),
        );
        root.insert(
            "ui".to_string(),
            toml::Value::Table(section(profile, &profile.ui)?),
        );
        toml::to_string_pretty(&toml::Value::Table(root))
            .map_err(|error| format!("the profile does not serialize: {error}"))
    }

    /// Atomically replace the document layer.
    ///
    /// The text goes to a sibling temporary file and is renamed over
    /// `doc`, so a reader never sees a half-written document.
    ///
    /// # Errors
    ///
    /// Propagates the write / rename failure.
    pub fn commit(&self, text: &str) -> std::io::Result<()> {
        let parent = self.doc.parent().unwrap_or_else(|| Path::new("."));
        let name = self
            .doc
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "profile.toml".to_owned());
        let temporary = parent.join(format!(".{name}.tmp-{}", process::id()));
        std::fs::write(&temporary, text)?;
        if let Err(error) = std::fs::rename(&temporary, &self.doc) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
}

fn section(
    profile: &Profile,
    rows: &[Entry],
) -> Result<toml::value::Table, String> {
    let mut section = toml::value::Table::new();
    section.insert(
        "entries".to_string(),
        toml::Value::Array(
            rows.iter()
                .map(|entry| row_to_toml(profile, entry))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    );
    Ok(section)
}

fn row_to_toml(
    _profile: &Profile,
    entry: &Entry,
) -> Result<toml::Value, String> {
    let mut row = toml::value::Table::new();
    row.insert("id".to_string(), toml::Value::String(entry.id.clone()));
    row.insert(
        "runtime".to_string(),
        toml::Value::String(runtime_name(entry.runtime).to_owned()),
    );
    if !entry.config.is_null() {
        row.insert("config".to_string(), json_to_toml(&entry.config)?);
    }
    if !entry.capabilities.is_empty() {
        row.insert(
            "capabilities".to_string(),
            toml::Value::Array(
                entry
                    .capabilities
                    .iter()
                    .map(|capability| {
                        toml::Value::String(capability.to_string())
                    })
                    .collect(),
            ),
        );
    }
    match &entry.enabled {
        Enabled::On => {}
        Enabled::Off => {
            row.insert("enabled".to_string(), toml::Value::Boolean(false));
        }
        Enabled::Check(check) => {
            let mut table = toml::value::Table::new();
            table.insert(
                "service".to_string(),
                toml::Value::String(check.service.clone()),
            );
            table.insert(
                "check".to_string(),
                toml::Value::String(check.check.clone()),
            );
            if !check.arg.is_null() {
                table.insert("arg".to_string(), json_to_toml(&check.arg)?);
            }
            row.insert("enabled".to_string(), toml::Value::Table(table));
        }
    }
    Ok(toml::Value::Table(row))
}

/// Convert a JSON value into its TOML form.
///
/// Object members whose value is null are dropped (serde's optional
/// fields read that as absent); a null anywhere else, or an array whose
/// members do not share a JSON kind, is an error.
fn json_to_toml(value: &Value) -> Result<toml::Value, String> {
    Ok(match value {
        Value::Null => {
            return Err(
                "a null value has no TOML form; omit or replace it".to_owned()
            );
        }
        Value::Bool(value) => toml::Value::Boolean(*value),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                toml::Value::Integer(value)
            } else if let Some(value) = number.as_u64() {
                i64::try_from(value).map(toml::Value::Integer).map_err(
                    |_| format!("the integer {value} exceeds TOML's range"),
                )?
            } else {
                toml::Value::Float(number.as_f64().unwrap_or_default())
            }
        }
        Value::String(value) => toml::Value::String(value.clone()),
        Value::Array(items) => {
            let mut converted = Vec::with_capacity(items.len());
            let mut kind: Option<&'static str> = None;
            for item in items {
                let item_kind = json_kind(item);
                if item.is_null() {
                    return Err(
                        "a null array member has no TOML form".to_owned()
                    );
                }
                if let Some(previous) = kind
                    && previous != item_kind
                {
                    return Err("a mixed-type array has no faithful TOML form"
                        .to_owned());
                }
                kind = Some(item_kind);
                converted.push(json_to_toml(item)?);
            }
            toml::Value::Array(converted)
        }
        Value::Object(members) => {
            let mut table = toml::value::Table::new();
            for (key, member) in members {
                if member.is_null() {
                    continue;
                }
                table.insert(key.clone(), json_to_toml(member)?);
            }
            toml::Value::Table(table)
        }
    })
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
