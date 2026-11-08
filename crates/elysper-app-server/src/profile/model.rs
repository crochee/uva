//! The profile data model: entries, assembly segments, patch documents,
//! and the error vocabulary shared by resolution and factory binding.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub use elysper::kernel::{
    Capability,
    Plugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::PluginFactory;

/// One assembled plugin row.
///
/// `runtime` selects the loader (`native` and `wasm` rows are handed to
/// the core loader, `ui-bundle` rows to the frontend loader), `config`
/// is the final configuration after every layer has been applied and
/// `capabilities` is the row's declared host-capability set.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Stable plugin identity, unique within a profile.
    pub id: String,
    /// Where the plugin executes.
    pub runtime: RuntimeKind,
    /// The final configuration object for this row.
    pub config: Value,
    /// Host capabilities the row declares, if any.
    pub capabilities: Vec<Capability>,
}

/// A resolved profile: the ordered core assembly, the ordered UI
/// assembly, and whether the process hot-reloads on file changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// Rows the core loader mounts, in assembly order.
    pub core: Vec<Entry>,
    /// Rows the frontend loader reads, in assembly order.
    pub ui: Vec<Entry>,
    /// Whether a profile file change is picked up by a running process.
    pub hmr: bool,
}

/// One `[[replace]]` row of a patch.
///
/// A replacement is addressed by `id` and replaces each provided field
/// **whole** — `config` in particular is never deep-merged with the
/// existing object. An omitted field keeps its previous value.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryPatch {
    /// The entry this replacement addresses.
    pub id: String,
    /// The new runtime, when the replacement changes it.
    pub runtime: Option<RuntimeKind>,
    /// The new configuration object, replacing the old one whole.
    pub config: Option<Value>,
}

/// A patch document.
///
/// `replace` addresses existing rows, `insert` appends new ones, and
/// `disabled` removes rows at resolution time. Patch rows are matched by
/// `id`; an id that matches nothing is an error, never a silent ignore.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    /// Rows that replace existing entries.
    pub replace: Vec<EntryPatch>,
    /// Rows appended to the assembly.
    pub insert: Vec<Entry>,
    /// Ids excluded from the resolved assembly.
    pub disabled: Vec<String>,
}

/// One ordered assembly layer.
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
    /// A row omitted `runtime`.
    #[error("entry `{id}` does not declare a runtime")]
    MissingRuntime {
        /// The row without a runtime.
        id: String,
    },
    /// A row named a runtime outside the three the host speaks.
    #[error(
        "unknown runtime `{value}`; accepted runtimes are native, wasm, \
         and ui-bundle"
    )]
    UnknownRuntime {
        /// The rejected wire value.
        value: String,
    },
    /// A row had an empty id.
    #[error("an entry in {layer} has an empty id")]
    EmptyId {
        /// The layer being parsed.
        layer: String,
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
    /// Two factories were registered for one id.
    #[error("duplicate plugin factory id `{id}`")]
    DuplicateFactoryId {
        /// The repeated factory id.
        id: String,
    },
    /// A profile row has no registered factory.
    #[error("unknown plugin id `{id}`; registered factories: {known:?}")]
    UnknownPluginId {
        /// The id with no factory.
        id: String,
        /// Every registered factory id, in sorted order.
        known: Vec<String>,
    },
    /// The factory's manifest and the profile row disagree about the
    /// runtime.
    #[error(
        "plugin `{id}` manifest declares runtime `{manifest}`, but the \
         entry declares `{entry}`"
    )]
    RuntimeMismatch {
        /// The plugin id.
        id: String,
        /// The runtime from the profile row.
        entry: String,
        /// The runtime from the manifest.
        manifest: String,
    },
    /// The factory's manifest id differs from the requested id.
    #[error(
        "factory registered as `{entry}` returned a manifest for `{manifest}`"
    )]
    ManifestIdMismatch {
        /// The id the profile asked for.
        entry: String,
        /// The id the manifest declares.
        manifest: String,
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

fn default_config() -> Value {
    Value::Object(Map::new())
}

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
struct WireEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    runtime: String,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    capabilities: Vec<Capability>,
}

#[derive(Debug, Deserialize)]
struct WireEntryPatch {
    #[serde(default)]
    id: String,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    config: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct WirePatch {
    #[serde(default)]
    replace: Vec<WireEntryPatch>,
    #[serde(default)]
    insert: Vec<WireEntry>,
    #[serde(default)]
    disabled: Vec<String>,
}

impl Entry {
    fn from_wire(wire: WireEntry, layer: &str) -> Result<Self, ProfileError> {
        if wire.id.trim().is_empty() {
            return Err(ProfileError::EmptyId {
                layer: layer.to_owned(),
            });
        }
        if wire.runtime.is_empty() {
            return Err(ProfileError::MissingRuntime { id: wire.id });
        }
        Ok(Self {
            id: wire.id,
            runtime: parse_runtime(&wire.runtime)?,
            config: wire.config.unwrap_or_else(default_config),
            capabilities: wire.capabilities,
        })
    }
}

impl Profile {
    /// Parse a profile TOML document.
    ///
    /// A missing `[core]` or `[ui]` section is valid and resolves to an
    /// empty segment. Empty ids, missing or unknown runtimes, and
    /// duplicate ids inside this one document are rejected here.
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
            .map(|entry| Entry::from_wire(entry, "the profile"))
            .collect::<Result<Vec<_>, _>>()?;
        let ui = wire
            .ui
            .entries
            .into_iter()
            .map(|entry| Entry::from_wire(entry, "the profile"))
            .collect::<Result<Vec<_>, _>>()?;
        let profile = Self {
            core,
            ui,
            hmr: wire.hmr,
        };
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

    /// Return the entry named `id`, searching the core segment first.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.core
            .iter()
            .chain(self.ui.iter())
            .find(|entry| entry.id == id)
    }

    /// Reject a duplicate id inside this document.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::DuplicateId`] naming the repeated id.
    pub fn validate_unique(&self) -> Result<(), ProfileError> {
        let mut seen = BTreeSet::new();
        for entry in self.core.iter().chain(self.ui.iter()) {
            if !seen.insert(entry.id.as_str()) {
                return Err(ProfileError::DuplicateId {
                    id: entry.id.clone(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn position(&self, id: &str) -> Option<(Segment, usize)> {
        if let Some(index) = self.core.iter().position(|entry| entry.id == id) {
            return Some((Segment::Core, index));
        }
        self.ui
            .iter()
            .position(|entry| entry.id == id)
            .map(|index| (Segment::Ui, index))
    }
}

impl Patch {
    /// Parse a patch TOML document.
    ///
    /// The document uses `[[replace]]` and `[[insert]]` tables plus a
    /// `disabled = [...]` array. Empty ids and a repeated id inside this
    /// one document are rejected.
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
            let runtime = match row.runtime {
                Some(runtime) if runtime.is_empty() => {
                    return Err(ProfileError::MissingRuntime { id: row.id });
                }
                Some(runtime) => Some(parse_runtime(&runtime)?),
                None => None,
            };
            replace.push(EntryPatch {
                id: row.id,
                runtime,
                config: row.config,
            });
        }
        let insert = wire
            .insert
            .into_iter()
            .map(|entry| Entry::from_wire(entry, "the patch"))
            .collect::<Result<Vec<_>, _>>()?;
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
            .chain(self.insert.iter().map(|row| row.id.as_str()))
        {
            if !seen.insert(id) {
                return Err(ProfileError::DuplicateId { id: id.to_owned() });
            }
        }
        Ok(())
    }
}

/// Which profile segment an entry lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Segment {
    /// The core loader's segment.
    Core,
    /// The frontend loader's segment.
    Ui,
}

/// The wire name of a runtime.
#[must_use]
pub(crate) fn runtime_name(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Native => "native",
        RuntimeKind::Wasm => "wasm",
        RuntimeKind::UiBundle => "ui-bundle",
    }
}

/// Parse a runtime's wire name.
///
/// # Errors
///
/// Returns [`ProfileError::UnknownRuntime`] naming the value and the
/// accepted set.
pub(crate) fn parse_runtime(value: &str) -> Result<RuntimeKind, ProfileError> {
    match value {
        "native" => Ok(RuntimeKind::Native),
        "wasm" => Ok(RuntimeKind::Wasm),
        "ui-bundle" => Ok(RuntimeKind::UiBundle),
        other => Err(ProfileError::UnknownRuntime {
            value: other.to_owned(),
        }),
    }
}

/// Where a plugin row belongs, by runtime.
#[must_use]
pub(crate) fn segment_of(entry: &Entry) -> Segment {
    if entry.runtime == RuntimeKind::UiBundle {
        Segment::Ui
    } else {
        Segment::Core
    }
}

/// A factory directory keyed by plugin id.
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

    /// Look a factory up by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&std::sync::Arc<dyn PluginFactory>> {
        self.by_id.get(id)
    }

    /// Every registered factory id, in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.by_id.keys().cloned().collect()
    }

    /// The top-level config fields the factory marks volatile for `id`.
    ///
    /// A change to only these fields updates the value the plugin reads
    /// next time but does not remount its fiber. An unknown id has no
    /// volatile fields.
    #[must_use]
    pub fn volatile_fields(&self, id: &str) -> &'static [&'static str] {
        self.by_id
            .get(id)
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
