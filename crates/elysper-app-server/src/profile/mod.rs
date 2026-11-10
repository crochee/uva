//! Profile / Bundle / Patch assembly.
//!
//! A deployment describes its plugin graph in a profile TOML document:
//!
//! ```toml
//! hmr = true
//!
//! [core]
//! entries = [
//!   { id = "agent-loop", runtime = "native", config = {} },
//!   { id = "llm-openai", runtime = "native", config = { model = "gpt-4o" } },
//! ]
//!
//! [ui]
//! entries = [
//!   { id = "theme", runtime = "ui-bundle", config = {} },
//! ]
//! ```
//!
//! [`resolve`] stacks the layers (empty → bundle patch → profile patch →
//! home patch → CLI overlay); a later layer replaces a row's `config`
//! **whole**, never deep-merges it. [`dump`] renders the resolved tree
//! deterministically, [`build_plugins`] maps each core row through the
//! [`PluginFactories`] directory after validating its configuration
//! against the factory manifest's `config_schema`, and
//! [`diff_profiles`] compares two resolutions so only affected fibers
//! are remounted.
//!
//! Deployment basics — bind address, workspace root, logging — stay in
//! `config.toml` (`ServerConfig`). Plugin assembly lives only in
//! profile documents; `elysper-app-server translate` migrates a legacy
//! `config.yaml`'s provider sections into a profile row.
//!
//! This module is deliberately generic over the factory directory and
//! [`BootContext`], so it can be tested without naming a concrete
//! plugin.

use std::{
    any::Any,
    fmt::{self, Formatter},
    path::{Path, PathBuf},
    sync::Arc,
};

use elysper::core::SharedClock;
use serde_json::Value;

mod enabled;
#[cfg(test)]
mod enabled_tests;
mod merge;
mod model;
mod store;
#[cfg(test)]
mod store_tests;
mod watch;

pub(crate) use enabled::KernelChecks;
pub use enabled::{
    CheckAnswer,
    CheckSource,
    EffectiveAssembly,
    SkippedRow,
    effective,
    partition_waves,
};
pub use merge::{HOME_PATCH_ENV, discover_home_patch, resolve, resolve_files};
pub use model::{
    Capability,
    EnableCheck,
    Enabled,
    Entry,
    EntryPatch,
    Patch,
    Plugin,
    PluginError,
    PluginFactories,
    PluginManifest,
    Profile,
    ProfileError,
    ProfileSource,
    RuntimeKind,
};
pub use store::{
    CapabilityAction,
    CapabilityDomain,
    ChangeOutcome,
    ChangeState,
    ProfileAction,
    ProfileStore,
    RowEdit,
};
pub use watch::{ProfileDiff, ProfileWatcher, diff_profiles};

/// The host objects a factory may need while constructing a plugin.
///
/// The concrete boot state (workspace root, provider catalogue, session
/// registry, agent registry) lands here in Task 12. Until then the
/// context carries the workspace root and an optional opaque host
/// handle, which is enough for the factory directory and its tests to
/// stay independent of the concrete plugin set.
pub struct BootContext {
    workspace_root: PathBuf,
    host: Option<Arc<dyn Any + Send + Sync>>,
    clock: SharedClock,
    blocking: Option<Arc<dyn elysper::core::blocking::Blocking>>,
    compaction: Option<elysper::session::CompactionPluginConfig>,
    sessions_dir: Option<PathBuf>,
}

impl BootContext {
    /// A context rooted at `workspace_root` with no host handle and
    /// the system clock.
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            host: None,
            clock: SharedClock::system(),
            blocking: None,
            compaction: None,
            sessions_dir: None,
        }
    }

    /// Attach the opaque host object the real boot later fills in.
    #[must_use]
    pub fn with_host<T>(mut self, host: Arc<T>) -> Self
    where
        T: Any + Send + Sync,
    {
        self.host = Some(host);
        self
    }

    /// Attach the process's one clock, so a factory building a
    /// clocked plugin samples the same time source every other
    /// component reads.
    #[must_use]
    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Attach the deployment's blocking-pool seam, so a plugin that
    /// appends to disk hands the work to the runtime instead of a
    /// fresh OS thread.
    #[must_use]
    pub fn with_blocking(
        mut self,
        blocking: Option<Arc<dyn elysper::core::blocking::Blocking>>,
    ) -> Self {
        self.blocking = blocking;
        self
    }

    /// Attach the deployment's compaction policy (the model and
    /// settings the `context-compactor` row builds its engine from).
    #[must_use]
    pub fn with_compaction(
        mut self,
        compaction: Option<elysper::session::CompactionPluginConfig>,
    ) -> Self {
        self.compaction = compaction;
        self
    }

    /// The blocking-pool seam, when the deployment attaches one.
    #[must_use]
    pub fn blocking(
        &self,
    ) -> Option<Arc<dyn elysper::core::blocking::Blocking>> {
        self.blocking.clone()
    }

    /// The compaction policy, when the profile supplies one.
    #[must_use]
    pub fn compaction(
        &self,
    ) -> Option<&elysper::session::CompactionPluginConfig> {
        self.compaction.as_ref()
    }

    /// Attach the session corpus root, when it differs from
    /// `<workspace_root>/sessions`.
    #[must_use]
    pub fn with_sessions_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.sessions_dir = dir;
        self
    }

    /// The session corpus root override, if the deployment set one.
    #[must_use]
    pub fn sessions_dir(&self) -> Option<&Path> {
        self.sessions_dir.as_deref()
    }

    /// The workspace the runtime owns.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// The host object, when one is attached and has type `T`.
    #[must_use]
    pub fn host<T: Any>(&self) -> Option<&T> {
        self.host.as_ref()?.downcast_ref::<T>()
    }

    /// The process clock a clocked plugin is built with.
    #[must_use]
    pub fn clock(&self) -> &SharedClock {
        &self.clock
    }
}

impl Default for BootContext {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

impl fmt::Debug for BootContext {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootContext")
            .field("workspace_root", &self.workspace_root)
            .field("has_host", &self.host.is_some())
            .finish()
    }
}

/// The app-server's factory for one plugin id.
///
/// A factory owns the knowledge the kernel must not have: how to turn a
/// profile row into a concrete [`Plugin`]. The manifest it returns is
/// the same value the kernel will read at mount time, so the profile
/// loader and the kernel cannot disagree about `provides` / `inject`.
///
/// A factory may also mark top-level config fields as **volatile**: a
/// change to only those fields updates the value the plugin reads next
/// time without a remount.
pub trait PluginFactory: Send + Sync {
    /// The plugin id this factory builds.
    fn id(&self) -> &'static str;

    /// The manifest of the plugin this factory builds.
    fn manifest(&self) -> PluginManifest;

    /// Construct the plugin for `config` under the row's declared
    /// `capabilities`.
    ///
    /// The capabilities are the profile row's grants, already parsed
    /// into kernel capability names; a factory whose plugin confines
    /// itself by them (the Wasm loader) copies them into the plugin's
    /// manifest.
    ///
    /// # Errors
    ///
    /// A construction failure is reported as [`PluginError`]; the
    /// profile loader wraps it in [`ProfileError::BuildFailed`] naming
    /// the id.
    fn build(
        &self,
        config: &Value,
        capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn Plugin>, PluginError>;

    /// Top-level config fields that do not require a remount.
    ///
    /// Defaults to none: every configuration change remounts the fiber.
    fn volatile_fields(&self) -> &'static [&'static str] {
        &[]
    }
}

/// Build every core entry in `profile`, in assembly order.
///
/// For each row: look the id up in `factories`, check the manifest's
/// runtime and id agree with the row, reject a core-side `ui.` service
/// key, validate `config` against the manifest's `config_schema`, then
/// call [`PluginFactory::build`]. UI rows are not built here — the
/// frontend loader reads them.
///
/// The returned vector is ready for
/// [`mount_all`](elysper::kernel::mount_all); dependency ordering,
/// cycle detection, and rollback stay the kernel's job.
///
/// # Errors
///
/// Returns [`ProfileError`] naming the first row that cannot be built.
/// A configuration that violates the schema is rejected before the
/// factory runs.
pub fn build_plugins(
    profile: &Profile,
    factories: &PluginFactories,
    boot: &BootContext,
) -> Result<Vec<Arc<dyn Plugin>>, ProfileError> {
    profile.validate_unique()?;
    let mut built = Vec::with_capacity(profile.core.len());
    for entry in &profile.core {
        built.push(build_one(entry, factories, boot)?);
    }
    Ok(built)
}

fn build_one(
    entry: &Entry,
    factories: &PluginFactories,
    boot: &BootContext,
) -> Result<Arc<dyn Plugin>, ProfileError> {
    let factory = factories.get(&entry.id).ok_or_else(|| {
        ProfileError::UnknownPluginId {
            id: entry.id.clone(),
            known: factories.ids(),
        }
    })?;
    let manifest = factory.manifest();
    if manifest.id != entry.id {
        return Err(ProfileError::ManifestIdMismatch {
            entry: entry.id.clone(),
            manifest: manifest.id,
        });
    }
    if manifest.runtime != entry.runtime {
        return Err(ProfileError::RuntimeMismatch {
            id: entry.id.clone(),
            entry: model::runtime_name(entry.runtime).to_owned(),
            manifest: model::runtime_name(manifest.runtime).to_owned(),
        });
    }
    for key in manifest.provides.iter().chain(manifest.inject.iter()) {
        if key.starts_with(elysper::kernel::UI_PREFIX) {
            return Err(ProfileError::NamespaceViolation {
                id: entry.id.clone(),
                key: key.clone(),
            });
        }
    }
    if let Some(schema) = &manifest.config_schema {
        validate_config(&entry.config, schema).map_err(|message| {
            ProfileError::InvalidConfig {
                id: entry.id.clone(),
                message,
            }
        })?;
    }
    factory
        .build(&entry.config, &entry.capabilities, boot)
        .map_err(|error| ProfileError::BuildFailed {
            id: entry.id.clone(),
            message: error.to_string(),
        })
}

/// Validate `config` against the supported subset of JSON Schema.
///
/// Supported keywords: `type` (string or array), `required`,
/// `properties`, `enum`, and `additionalProperties` when it is `false`.
/// Every other keyword is ignored. The subset covers the manifest
/// schemas the built-in factories publish; a factory needing more can
/// validate further inside [`PluginFactory::build`].
///
/// # Errors
///
/// Returns a message naming the JSON path that failed.
pub fn validate_config(config: &Value, schema: &Value) -> Result<(), String> {
    validate_value(config, schema, "config")
}

fn validate_value(
    value: &Value,
    schema: &Value,
    path: &str,
) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    if let Some(types) = object.get("type") {
        check_type(value, types, path)?;
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array)
        && !values.contains(value)
    {
        return Err(format!("{path} must be one of {values:?}, got {value}"));
    }
    if let Some(value_object) = value.as_object() {
        check_required(value_object, object, path)?;
        check_properties(value_object, object, path)?;
        check_additional(value_object, object, path)?;
    }
    Ok(())
}

fn check_type(value: &Value, types: &Value, path: &str) -> Result<(), String> {
    let matches = match types {
        Value::String(name) => type_matches(value, name),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| type_matches(value, name)),
        _ => true,
    };
    if matches {
        Ok(())
    } else {
        Err(format!("{path} must be of type {types}, got {value}"))
    }
}

fn type_matches(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn check_required(
    value: &serde_json::Map<String, Value>,
    schema: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let Some(required) = schema.get("required").and_then(Value::as_array)
    else {
        return Ok(());
    };
    for field in required.iter().filter_map(Value::as_str) {
        if !value.contains_key(field) {
            return Err(format!("{path} is missing required field `{field}`"));
        }
    }
    Ok(())
}

fn check_properties(
    value: &serde_json::Map<String, Value>,
    schema: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object)
    else {
        return Ok(());
    };
    for (field, subschema) in properties {
        if let Some(field_value) = value.get(field) {
            validate_value(field_value, subschema, &format!("{path}.{field}"))?;
        }
    }
    Ok(())
}

fn check_additional(
    value: &serde_json::Map<String, Value>,
    schema: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Ok(());
    }
    let allowed = schema.get("properties").and_then(Value::as_object);
    for field in value.keys() {
        let known = allowed.is_some_and(|allowed| allowed.contains_key(field));
        if !known {
            return Err(format!(
                "{path}.{field} is not allowed (additionalProperties = false)"
            ));
        }
    }
    Ok(())
}

/// Render a resolved profile as stable, pretty JSON.
///
/// This is the manifest-free view: each row's `id`, `runtime`,
/// `capabilities`, final `config`, and optional `source`. It is
/// byte-identical for the same input: entries keep their assembly order
/// and object keys are sorted at every depth.
///
/// [`dump_with_manifests`] adds the manifest-level `provides` / `inject`
/// lists; use it for `--dump-config` once the factory directory is
/// populated.
#[must_use]
pub fn dump(profile: &Profile) -> String {
    render(profile, None)
}

/// Render the resolved tree with each core row's manifest services.
///
/// `provides` and `inject` come from the factory manifest, so the dump
/// shows what the kernel will actually order and bind. A core row whose
/// id has no registered factory prints explicit empty lists rather than
/// omitting the fields (the boot registers factories in Task 12); UI
/// rows have no core factory and also print empty lists.
///
/// # Errors
///
/// Returns [`ProfileError::ManifestIdMismatch`] when a registered
/// factory's manifest names a different id than the row it is bound to.
pub fn dump_with_manifests(
    profile: &Profile,
    factories: &PluginFactories,
) -> Result<String, ProfileError> {
    for entry in &profile.core {
        if let Some(factory) = factories.get(&entry.id) {
            let manifest = factory.manifest();
            if manifest.id != entry.id {
                return Err(ProfileError::ManifestIdMismatch {
                    entry: entry.id.clone(),
                    manifest: manifest.id,
                });
            }
        }
    }
    Ok(render(profile, Some(factories)))
}

fn render(profile: &Profile, factories: Option<&PluginFactories>) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"hmr\": ");
    out.push_str(if profile.hmr { "true" } else { "false" });
    out.push_str(",\n  \"core\": ");
    dump_entries(&mut out, &profile.core, 2, factories);
    out.push_str(",\n  \"ui\": ");
    dump_entries(&mut out, &profile.ui, 2, factories);
    out.push_str("\n}\n");
    out
}

fn dump_entries(
    out: &mut String,
    entries: &[Entry],
    indent: usize,
    factories: Option<&PluginFactories>,
) {
    if entries.is_empty() {
        out.push_str("[]");
        return;
    }
    let entry_pad = " ".repeat(indent + 2);
    let field_pad = " ".repeat(indent + 4);
    out.push_str("[\n");
    for (index, entry) in entries.iter().enumerate() {
        // `Some(None)` means "manifests were requested and this row has
        // no factory": print explicit empty lists. `None` means the
        // manifest-free dump, which omits the fields entirely.
        let services = factories.map(|factories| {
            factories.get(&entry.id).map(|factory| factory.manifest())
        });
        out.push_str(&entry_pad);
        out.push_str("{\n");
        let mut first = true;
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"id\": ");
        out.push_str(&json_string(&entry.id));
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"runtime\": ");
        out.push_str(&json_string(model::runtime_name(entry.runtime)));
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"capabilities\": ");
        write_json(
            out,
            &Value::Array(
                entry
                    .capabilities
                    .iter()
                    .map(|capability| Value::String(capability.to_string()))
                    .collect(),
            ),
            indent + 4,
        );
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"config\": ");
        write_json(out, &entry.config, indent + 4);
        if entry.enabled != crate::profile::Enabled::On {
            begin_field(out, &field_pad, &mut first);
            out.push_str("\"enabled\": ");
            write_json(
                out,
                &serde_json::to_value(&entry.enabled)
                    .unwrap_or(Value::Bool(true)),
                indent + 4,
            );
        }
        if let Some(manifest) = &services {
            let provides = manifest
                .as_ref()
                .map(|manifest| manifest.provides.clone())
                .unwrap_or_default();
            begin_field(out, &field_pad, &mut first);
            out.push_str("\"provides\": ");
            write_json(
                out,
                &Value::Array(
                    provides.into_iter().map(Value::String).collect(),
                ),
                indent + 4,
            );
            let inject = manifest
                .as_ref()
                .map(|manifest| manifest.inject.clone())
                .unwrap_or_default();
            begin_field(out, &field_pad, &mut first);
            out.push_str("\"inject\": ");
            write_json(
                out,
                &Value::Array(inject.into_iter().map(Value::String).collect()),
                indent + 4,
            );
        }
        out.push('\n');
        out.push_str(&entry_pad);
        out.push('}');
        if index + 1 < entries.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(&" ".repeat(indent));
    out.push(']');
}

/// Start a field: a comma and newline between fields, nothing before the
/// first one.
fn begin_field(out: &mut String, pad: &str, first: &mut bool) {
    if *first {
        *first = false;
    } else {
        out.push_str(",\n");
    }
    out.push_str(pad);
}

/// Write one JSON value with object keys sorted at every depth.
fn write_json(out: &mut String, value: &Value, indent: usize) {
    match value {
        Value::Object(object) => {
            if object.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let pad = " ".repeat(indent + 2);
            out.push_str("{\n");
            for (index, key) in keys.iter().enumerate() {
                out.push_str(&pad);
                out.push_str(&json_string(key));
                out.push_str(": ");
                write_json(out, &object[*key], indent + 2);
                if index + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            let pad = " ".repeat(indent + 2);
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&pad);
                write_json(out, item, indent + 2);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}
