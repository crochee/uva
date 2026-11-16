//! The profile data model: plugin manifests, assembly segments, patch
//! documents, and the error vocabulary shared by resolution and factory
//! binding.
//!
//! The row shape is the dsh (DeepSeek Harness) entry model translated
//! to TOML: every row is a [`Manifest`] carrying its own identity
//! (`id`), the plugin it loads (`name`), its configuration, its
//! enablement (`disabled`), its dependency declaration (`inject`), its
//! service-interception config (`intercept`), and its service-scope
//! isolation (`isolate`). A `group` row nests child rows in its
//! `config` array and mounts as a runtime group plugin; a `disabled`
//! bit anywhere on the ancestor chain keeps the subtree out of the
//! assembly.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde::Deserialize;
use serde_json::Value;
pub use uva::kernel::{Capability, Plugin, PluginError, PluginManifest, RuntimeKind};

use super::PluginFactory;

/// One service-scope isolation label on a row.
///
/// dsh's `Dict<true | string>`: `true` gives the row a fresh private
/// scope for that service (dsh `LocalRealm`); a string joins every row
/// carrying the same label to one shared implementation slot (dsh
/// `GlobalRealm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolateLabel {
    /// A fresh scope private to this row.
    Local,
    /// A scope shared by every row naming the same label.
    Shared(String),
}

/// One configured plugin row in a profile document.
///
/// This is the dsh `EntryOptions` shape: the document states the whole
/// tree membership, and the plugin's own manifest contributes only what
/// the document cannot know (its service keys, version, schema). At the
/// assembly boundary the loader resolves the **effective** dependency
/// set as the union of the plugin manifest's `inject` and this row's
/// `inject` — the row wins on a key present in both, and the document
/// can never remove the plugin's inherent minimum.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Stable row identity, unique within the document.
    pub id: String,
    /// The plugin this row loads: the factory-directory key.
    ///
    /// Empty only on `group` rows, whose children live in `children`
    /// (serialized as the row's `config` array, the dsh Group plugin
    /// shape).
    pub name: String,
    /// The row's configuration object.
    ///
    /// `Null` — the document states no opinion and the factory default
    /// applies. An explicit empty table is a statement ("empty
    /// configuration"); the two are different layer statements. For a
    /// group row this is always `Null`: children are held typed in
    /// [`Manifest::children`] and serialized back into `config`.
    pub config: Value,
    /// Host capabilities the row grants the plugin, if any.
    pub capabilities: Vec<Capability>,
    /// Whether this row is a nested group carrying children.
    ///
    /// A group mounts as a runtime group plugin owning its subtree's
    /// scope; `disabled` on a group disables every descendant while the
    /// group itself is always considered enabled (dsh
    /// `Entry#disabled`).
    pub group: bool,
    /// The row's children; only group rows carry them.
    pub children: Vec<Manifest>,
    /// Whether this row (and, for a group, its descendants) stays out
    /// of the assembly.
    ///
    /// The runtime writes this back as `true` when the row's fiber
    /// unloads because it lost a dependency or disposed itself — the
    /// document always reflects the real assembly (dsh loader
    /// write-back).
    pub disabled: bool,
    /// The kernel service keys this row additionally depends on.
    ///
    /// `None` — the row states no opinion, the plugin manifest's
    /// default applies. `Some(list)` — the keys join the plugin's own
    /// set (the row wins on a key present in both).
    pub inject: Option<Vec<String>>,
    /// Per-service interception config this row carries.
    ///
    /// `None` — no opinion. `Some(map)` — each entry overlays the
    /// service's intercept config for this row's subtree; children
    /// inherit their ancestors' entries and override same-named keys
    /// (dsh context intercept chain).
    pub intercept: Option<BTreeMap<String, Value>>,
    /// Which services this row reads through an isolated scope.
    ///
    /// `None` — no opinion. `Some(map)` — each entry names a service
    /// key and its scope label (dsh `isolate` entry option).
    pub isolate: Option<BTreeMap<String, IsolateLabel>>,
}

impl Manifest {
    /// A plain row: no config, no capabilities, no opinions beyond
    /// `id` and `name`.
    #[must_use]
    pub fn plain(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            config: Value::Null,
            capabilities: Vec::new(),
            group: false,
            children: Vec::new(),
            disabled: false,
            inject: None,
            intercept: None,
            isolate: None,
        }
    }

    /// The rows this one contributes to the core assembly.
    ///
    /// An enabled row contributes itself — a `group` row included: it
    /// mounts as the runtime group plugin and its children mount
    /// beneath it, they are **not** flattened into the parent's list.
    /// A `disabled` bit anywhere on the ancestor chain keeps the whole
    /// subtree out (dsh `Entry#disabled` walk); a disabled group is an
    /// empty group and mounts nothing observable either way.
    #[must_use]
    pub fn assembled(&self, inherited_disabled: bool) -> Vec<&Manifest> {
        if self.disabled || inherited_disabled {
            Vec::new()
        } else {
            vec![self]
        }
    }

    /// The rows at and under this row with `disabled` inheritance, for
    /// loaders that project rows instead of mounting them (the frontend
    /// has no group plugin): group rows contribute their children.
    #[must_use]
    pub fn projected(&self, inherited_disabled: bool) -> Vec<&Manifest> {
        let effective = self.disabled || inherited_disabled;
        if effective {
            return Vec::new();
        }
        if self.group {
            self.children
                .iter()
                .flat_map(|child| child.projected(effective))
                .collect()
        } else {
            vec![self]
        }
    }

    /// Every row at and under this row, disabled ones included.
    #[must_use]
    pub fn rows(&self) -> Vec<&Manifest> {
        let mut result = vec![self];
        for child in &self.children {
            result.extend(child.rows());
        }
        result
    }
}

/// A resolved profile: the ordered core assembly, the ordered UI
/// assembly, and whether the process hot-reloads on file changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// Rows the core loader reads, in document order.
    pub core: Vec<Manifest>,
    /// Rows the frontend loader reads, in document order.
    pub ui: Vec<Manifest>,
    /// Whether a profile file change is picked up by a running process.
    pub hmr: bool,
}

impl Profile {
    /// The core rows to mount: `disabled`-filtered with group
    /// inheritance, in document order. Group rows are included — they
    /// mount as the runtime group plugin carrying their children.
    #[must_use]
    pub fn core_rows(&self) -> Vec<&Manifest> {
        self.core
            .iter()
            .flat_map(|row| row.assembled(false))
            .collect()
    }

    /// The UI rows to assemble: flattened with group inheritance, in
    /// document order — the frontend loader projects rows and has no
    /// group plugin.
    #[must_use]
    pub fn ui_rows(&self) -> Vec<&Manifest> {
        self.ui
            .iter()
            .flat_map(|row| row.projected(false))
            .collect()
    }
}

/// One `[[replace]]` row of a patch.
///
/// A replacement is addressed by `id` and replaces each provided field
/// **whole** — `config` in particular is never deep-merged with the
/// existing object. An omitted field keeps its previous value.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryPatch {
    /// The row this replacement addresses.
    pub id: String,
    /// The new plugin name, when the replacement changes it.
    pub name: Option<String>,
    /// The new configuration object, replacing the old one whole. On a
    /// group row this replaces the child list.
    pub config: Option<Value>,
    /// The new enablement, when the replacement changes it.
    pub disabled: Option<bool>,
    /// The new dependency declaration, replacing the old one whole.
    ///
    /// `None` — this layer states no opinion about the edges; `Some`
    /// replaces the row's declaration (which itself unions with the
    /// plugin manifest's default at the assembly boundary).
    pub inject: Option<Vec<String>>,
}

/// A patch document.
///
/// `replace` addresses existing rows, `insert` appends new ones (with
/// the segment they join), and `disabled` removes rows at resolution
/// time. Patch rows are matched by `id`; an id that matches nothing is
/// an error, never a silent ignore.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    /// Field replacements addressed by id.
    pub replace: Vec<EntryPatch>,
    /// New rows appended at resolution time, with their segment.
    pub insert: Vec<PatchInsert>,
    /// Row ids removed at resolution time.
    pub disabled: Vec<String>,
}

/// One `[[insert]]` row: the row and the segment it joins.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchInsert {
    /// The segment the row joins.
    pub segment: Segment,
    /// The row itself.
    pub row: Manifest,
}

/// A profile document contributes rows and an `hmr` flag; a patch
/// document only addresses rows that already exist.
///
/// The resolver applies layers in slice order —
/// empty → bundle patch → profile patch → home patch — and applies the
/// CLI overlay (the `overlay` argument of [`resolve`](super::resolve))
/// last. A `Profile` layer contributes its entries (replacing a row with
/// the same id whole) and adopts its `hmr` flag; a `Patch` layer only
/// addresses rows that already exist.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileSource {
    /// A profile document.
    Profile(Profile),
    /// A patch document.
    Patch(Patch),
}

/// Why a profile could not be parsed, resolved, or bound to factories.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The TOML document did not parse.
    #[error("could not parse the profile document: {message}")]
    Parse {
        /// The parser's message.
        message: String,
    },
    /// A file could not be read.
    #[error("could not read `{path}`: {message}")]
    Io {
        /// The path that failed.
        path: String,
        /// The operating system's message.
        message: String,
    },
    /// A row had an empty id.
    #[error("an entry in {layer} has an empty id")]
    EmptyId {
        /// The layer being parsed.
        layer: String,
    },
    /// A non-group row omitted `name`.
    #[error("entry `{id}` does not declare a plugin name")]
    MissingName {
        /// The row without a plugin name.
        id: String,
    },
    /// A group row declared a plugin name.
    #[error("group entry `{id}` must not declare a plugin name")]
    NameOnGroup {
        /// The group row carrying a name.
        id: String,
    },
    /// A group row's `config` was not an array of child rows.
    #[error("group entry `{id}` config must be an array of child rows: {message}")]
    GroupConfig {
        /// The group row.
        id: String,
        /// What was wrong with the array.
        message: String,
    },
    /// One layer declared the same id twice.
    #[error("duplicate entry id `{id}` in one layer")]
    DuplicateId {
        /// The repeated id.
        id: String,
    },
    /// A patch addressed an id the assembly does not contain.
    #[error("patch references unknown id `{id}`")]
    UnknownPatchId {
        /// The id that matched nothing.
        id: String,
    },
    /// Two factories were registered for one name.
    #[error("duplicate plugin factory id `{id}`")]
    DuplicateFactoryId {
        /// The repeated factory id.
        id: String,
    },
    /// A profile row names a plugin with no registered factory.
    #[error("unknown plugin `{name}` (entry `{id}`); registered factories: {known:?}")]
    UnknownPluginId {
        /// The row that named it.
        id: String,
        /// The plugin name with no factory.
        name: String,
        /// Every registered factory id, in sorted order.
        known: Vec<String>,
    },
    /// The plugin's manifest declares the other runtime than the row's
    /// segment.
    #[error(
        "plugin `{id}` runs on the {manifest} runtime but its row sits in \
         the {segment} segment"
    )]
    SegmentMismatch {
        /// The row id.
        id: String,
        /// The segment the row sits in.
        segment: &'static str,
        /// The runtime the manifest declares.
        manifest: &'static str,
    },
    /// A core manifest declared a `ui.` service key.
    #[error(
        "plugin `{id}` declares core-side service `{key}`; `ui.` keys are \
         reserved for the frontend loader"
    )]
    NamespaceViolation {
        /// The plugin id.
        id: String,
        /// The out-of-namespace key.
        key: String,
    },
    /// The entry's configuration does not satisfy the manifest schema.
    #[error("plugin `{id}` config is invalid: {message}")]
    InvalidConfig {
        /// The plugin id.
        id: String,
        /// The failing path and reason.
        message: String,
    },
    /// The factory failed to build the plugin.
    #[error("plugin `{id}` failed to build: {message}")]
    BuildFailed {
        /// The plugin id.
        id: String,
        /// The factory's own message.
        message: String,
    },
}

// -- wire documents --------------------------------------------------

#[derive(Debug, Deserialize)]
struct WireProfile {
    #[serde(default)]
    hmr: bool,
    #[serde(default)]
    core: WireSection,
    #[serde(default)]
    ui: WireSection,
}

#[derive(Debug, Default, Deserialize)]
struct WireSection {
    #[serde(default)]
    entries: Vec<WireEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    capabilities: Vec<Capability>,
    #[serde(default)]
    group: bool,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    inject: Option<Vec<String>>,
    #[serde(default)]
    intercept: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    isolate: Option<BTreeMap<String, WireIsolateLabel>>,
}

/// The wire form of one isolation label: `true` or a label string.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WireIsolateLabel {
    /// `true` — a fresh row-private scope.
    Bool(bool),
    /// A label string — one shared scope per label.
    Shared(String),
}


#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntryPatch {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    inject: Option<Vec<String>>,
}

/// One `[[insert]]` row: a full row plus the segment it lands in.
#[derive(Debug, Deserialize)]
struct WireInsert {
    /// The segment the row joins: `core` (the default) or `ui`.
    #[serde(default)]
    segment: String,
    #[serde(flatten)]
    entry: WireEntry,
}

#[derive(Debug, Default, Deserialize)]
struct WirePatch {
    #[serde(default)]
    replace: Vec<WireEntryPatch>,
    #[serde(default)]
    insert: Vec<WireInsert>,
    #[serde(default)]
    disabled: Vec<String>,
}

/// Parse a group row's `config` array into child manifests.
///
/// Shared by document parsing and patch application (a replacement's
/// `config` on a group row replaces the child list whole).
pub(crate) fn children_from_config(
    id: &str,
    config: Option<&Value>,
    layer: &str,
) -> Result<Vec<Manifest>, ProfileError> {
    let Some(items) = config.map(Value::as_array) else {
        return Ok(Vec::new());
    };
    let Some(items) = items else {
        return Err(ProfileError::GroupConfig {
            id: id.to_owned(),
            message: "group config must be an array".to_owned(),
        });
    };
    items
        .iter()
        .map(|item| {
            serde_json::from_value::<WireEntry>(item.clone()).map_err(
                |error| ProfileError::GroupConfig {
                    id: id.to_owned(),
                    message: error.to_string(),
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|child| Manifest::from_wire(child, layer))
        .collect()
}


impl From<WireIsolateLabel> for IsolateLabel {
    fn from(wire: WireIsolateLabel) -> Self {
        match wire {
            WireIsolateLabel::Shared(label) => Self::Shared(label),
            WireIsolateLabel::Bool(_) => Self::Local,
        }
    }
}

impl Manifest {
    fn from_wire(wire: WireEntry, layer: &str) -> Result<Self, ProfileError> {
        if wire.id.trim().is_empty() {
            return Err(ProfileError::EmptyId {
                layer: layer.to_owned(),
            });
        }
        let (name, config, children) = if wire.group {
            if !wire.name.is_empty() {
                return Err(ProfileError::NameOnGroup { id: wire.id });
            }
            // dsh Group plugin shape: the group's children ARE its
            // config, an array of child rows.
            let children = children_from_config(
                &wire.id,
                wire.config.as_ref(),
                layer,
            )?;
            (String::new(), Value::Null, children)
        } else if wire.name.trim().is_empty() {
            return Err(ProfileError::MissingName { id: wire.id });
        } else {
            (wire.name, wire.config.unwrap_or(Value::Null), Vec::new())
        };
        Ok(Self {
            id: wire.id,
            name,
            // A document row without a `config` key has *no opinion*:
            // it inherits (`null`), the same value `to_toml` writes
            // back. An explicit empty table still means "empty
            // configuration" — the two are different layer statements,
            // and conflating them made the second `profile/set` after
            // a materialization remount the whole assembly.
            config,
            capabilities: wire.capabilities,
            group: wire.group,
            children,
            disabled: wire.disabled,
            // `inject` keeps its `None` when the row states no opinion:
            // the plugin manifest's default then applies. An explicit
            // empty list is a statement ("this row adds no edges").
            inject: wire.inject,
            intercept: wire.intercept,
            isolate: wire.isolate.map(|map| {
                map.into_iter().map(|(key, label)| (key, label.into())).collect()
            }),
        })
    }
}

impl Profile {
    /// Parse a profile TOML document.
    ///
    /// A missing `[core]` or `[ui]` section is valid and resolves to an
    /// empty segment. Empty ids, rows without a plugin name, groups
    /// with a name, a group config that is not an array of child rows,
    /// and duplicate ids inside this one document are rejected here.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError`] when the document is not valid TOML or
    /// violates the profile contract.
    pub fn from_toml_str(text: &str) -> Result<Self, ProfileError> {
        let wire: WireProfile =
            toml::from_str(text).map_err(|error| ProfileError::Parse {
                message: error.to_string(),
            })?;
        let core = wire
            .core
            .entries
            .into_iter()
            .map(|row| Manifest::from_wire(row, "the profile"))
            .collect::<Result<Vec<_>, _>>()?;
        let ui = wire
            .ui
            .entries
            .into_iter()
            .map(|row| Manifest::from_wire(row, "the profile"))
            .collect::<Result<Vec<_>, _>>()?;
        let profile = Self { core, ui, hmr: wire.hmr };
        profile.validate_unique()?;
        Ok(profile)
    }

    /// Read and parse a profile TOML file.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Io`] when the file cannot be read and the
    /// parse errors of [`Profile::from_toml_str`] otherwise.
    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ProfileError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            }
        })?;
        Self::from_toml_str(&text)
    }

    /// Whether the profile contains `id` in either segment.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.position(id).is_some()
    }

    /// Return the row named `id`, searching the core segment first.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Manifest> {
        self.all_rows().into_iter().find(|row| row.id == id)
    }

    /// Reject a duplicate id inside this document.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::DuplicateId`] naming the repeated id.
    pub fn validate_unique(&self) -> Result<(), ProfileError> {
        let mut seen = BTreeSet::new();
        for row in self.all_rows() {
            if !seen.insert(row.id.as_str()) {
                return Err(ProfileError::DuplicateId { id: row.id.clone() });
            }
        }
        Ok(())
    }

    /// Every row in the document, disabled ones and group rows
    /// included, core segment first.
    #[must_use]
    pub fn all_rows(&self) -> Vec<&Manifest> {
        self.core
            .iter()
            .chain(self.ui.iter())
            .flat_map(|row| row.rows())
            .collect()
    }

    pub(crate) fn position(&self, id: &str) -> Option<Segment> {
        if self.core.iter().any(|row| contains_id(row, id)) {
            return Some(Segment::Core);
        }
        if self.ui.iter().any(|row| contains_id(row, id)) {
            return Some(Segment::Ui);
        }
        None
    }
}

/// Whether `row` or any descendant carries `id`.
fn contains_id(row: &Manifest, id: &str) -> bool {
    row.rows().iter().any(|row| row.id == id)
}

impl Patch {
    /// Parse a patch TOML document.
    ///
    /// The document uses `[[replace]]` and `[[insert]]` tables plus a
    /// `disabled = [...]` array. Empty ids and a repeated id inside
    /// this one document are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError`] when the document is not valid TOML or
    /// violates the patch contract.
    pub fn from_toml_str(text: &str) -> Result<Self, ProfileError> {
        let wire: WirePatch =
            toml::from_str(text).map_err(|error| ProfileError::Parse {
                message: error.to_string(),
            })?;
        let mut replace = Vec::with_capacity(wire.replace.len());
        for row in wire.replace {
            if row.id.trim().is_empty() {
                return Err(ProfileError::EmptyId {
                    layer: "the patch".to_owned(),
                });
            }
            if let Some(name) = &row.name
                && name.trim().is_empty()
            {
                return Err(ProfileError::MissingName { id: row.id });
            }
            replace.push(EntryPatch {
                id: row.id,
                name: row.name,
                config: row.config,
                disabled: row.disabled,
                inject: row.inject,
            });
        }
        let mut insert = Vec::with_capacity(wire.insert.len());
        for row in wire.insert {
            let segment = match row.segment.as_str() {
                "" | "core" => Segment::Core,
                "ui" => Segment::Ui,
                other => {
                    return Err(ProfileError::Parse {
                        message: format!(
                            "unknown insert segment `{other}`; accepted: \
                             core, ui"
                        ),
                    })
                }
            };
            insert.push(PatchInsert {
                segment,
                row: Manifest::from_wire(row.entry, "the patch")?,
            });
        }
        let patch = Self {
            replace,
            insert,
            disabled: wire.disabled,
        };
        patch.validate_unique()?;
        Ok(patch)
    }

    /// Read and parse a patch TOML file.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Io`] when the file cannot be read and the
    /// parse errors of [`Patch::from_toml_str`] otherwise.
    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ProfileError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            }
        })?;
        Self::from_toml_str(&text)
    }

    /// Reject a duplicate id across this document's `replace` and
    /// `insert` rows.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::DuplicateId`] naming the repeated id.
    pub fn validate_unique(&self) -> Result<(), ProfileError> {
        let mut seen = BTreeSet::new();
        for id in self
            .replace
            .iter()
            .map(|row| row.id.as_str())
            .chain(self.insert.iter().flat_map(|insertion| {
                insertion.row.rows().into_iter().map(|row| row.id.as_str())
            }))
        {
            if !seen.insert(id) {
                return Err(ProfileError::DuplicateId { id: id.to_owned() });
            }
        }
        Ok(())
    }
}

/// Which profile segment a row lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// The core loader's segment.
    Core,
    /// The frontend loader's segment.
    Ui,
}

impl Segment {
    /// The segment's wire name.
    #[must_use]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Ui => "ui",
        }
    }
}

/// A factory directory keyed by plugin name.
#[derive(Default)]
pub struct PluginFactories {
    by_id: BTreeMap<String, std::sync::Arc<dyn PluginFactory>>,
}

impl PluginFactories {
    /// Create an empty directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one factory.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::DuplicateFactoryId`] when the id is
    /// already registered.
    pub fn register(
        &mut self,
        factory: std::sync::Arc<dyn PluginFactory>,
    ) -> Result<(), ProfileError> {
        let id = factory.id().to_owned();
        if self.by_id.contains_key(&id) {
            return Err(ProfileError::DuplicateFactoryId { id });
        }
        self.by_id.insert(id, factory);
        Ok(())
    }

    /// Look a factory up by plugin name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&std::sync::Arc<dyn PluginFactory>> {
        self.by_id.get(name)
    }

    /// Every registered factory id, in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.by_id.keys().cloned().collect()
    }

    /// The top-level config fields the factory marks volatile for
    /// `name`.
    ///
    /// A change to only these fields updates the value the plugin reads
    /// next time but does not remount its fiber. An unknown name has no
    /// volatile fields.
    #[must_use]
    pub fn volatile_fields(&self, name: &str) -> &'static [&'static str] {
        self.by_id
            .get(name)
            .map(|factory| factory.volatile_fields())
            .unwrap_or(&[])
    }

    /// Number of registered factories.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether no factory is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}
