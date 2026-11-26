//! Profile / Bundle / Patch assembly.
//!
//! A deployment describes its plugin graph in a profile TOML document.
//! The row shape is the dsh entry model: every row states its identity
//! (`id`), the plugin it loads (`name`), its configuration, its
//! enablement (`disabled`), its dependency declaration (`inject`), its
//! interception config (`intercept`), and its service-scope isolation
//! (`isolate`):
//!
//! ```toml
//! hmr = true
//!
//! [[core.entries]]
//! id = "agent-loop"
//! name = "agent-loop"
//! inject = ["llm.default", "tools", "sessions", "events"]
//!
//! [[core.entries]]
//! id = "batch"
//! group = true
//! config = [
//!   { id = "llm-openai", name = "model-adapter", config = { default_provider = "openai" } },
//! ]
//! isolate = { tools = "batch" }
//! ```
//!
//! [`resolve`] stacks the layers (empty → bundle patch → profile patch →
//! home patch → CLI overlay); a later layer replaces a row's `config`
//! **whole**, never deep-merges it. [`effective`] filters `disabled`
//! subtrees. [`build_plugins`] maps each assembled core row through the
//! [`PluginFactories`] directory (keyed by plugin `name`), resolves the
//! row's **effective** dependency set — the plugin manifest's `inject`
//! unioned with the row's, the row winning on a shared key — wraps
//! group rows in the runtime group plugin, and returns mountable
//! plugins with their per-row context derivations (isolation labels,
//! intercept configs) for [`uva::kernel::mount_all_with`].
//! [`diff_profiles`] compares two resolutions so only affected fibers
//! are remounted.
//!
//! Deployment basics — bind address, workspace root, logging — stay in
//! `config.toml` (`ServerConfig`). Plugin assembly lives only in
//! profile documents; `uva-app-server translate` migrates a legacy
//! `config.yaml`'s provider sections into a profile row.
//!
//! This module is deliberately generic over the factory directory and
//! [`BootContext`], so it can be tested without naming a concrete
//! plugin.

use std::{
    any::Any,
    fmt::{self, Formatter},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::Value;
pub use uva::kernel::Realms;
use uva::{
    core::SharedClock,
    kernel::{Context, EffectHandle, IsolateSpec, MountRow, mount_all_with},
};

mod merge;
mod model;
mod store;
#[cfg(test)]
mod store_tests;
mod watch;

pub use merge::{
    HOME_PATCH_ENV,
    discover_home_patch,
    project_document,
    resolve,
    resolve_files,
};
pub use model::{
    Capability,
    EntryPatch,
    IsolateLabel,
    Manifest,
    Patch,
    PatchInsert,
    Plugin,
    PluginError,
    PluginFactories,
    PluginManifest,
    Profile,
    ProfileError,
    ProfileSource,
    RuntimeKind,
    Segment,
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

/// The rows that will actually mount and render.
///
/// A `disabled` bit anywhere on the ancestor chain keeps the subtree
/// out — dsh's `Entry#disabled` walk, decided before `build_plugins` /
/// `mount_all` / `load_ui_tree`, so a disabled row never builds a
/// plugin and a disabled UI row never reaches the frontend schema.
/// Core group rows stay in (they mount as the runtime group plugin);
/// UI group rows flatten away (the frontend projects rows and has no
/// group plugin).
#[must_use]
pub fn effective(profile: &Profile) -> Profile {
    Profile {
        core: profile.core_rows().into_iter().cloned().collect(),
        ui: profile.ui_rows().into_iter().cloned().collect(),
        hmr: profile.hmr,
    }
}

/// The host objects a factory may need while constructing a plugin.
///
/// The concrete boot state (workspace root, provider catalogue, session
/// registry, agent registry) lands here. The context carries the
/// workspace root and an optional opaque host handle, which is enough
/// for the factory directory and its tests to stay independent of the
/// concrete plugin set.
pub struct BootContext {
    workspace_root: PathBuf,
    host: Option<Arc<dyn Any + Send + Sync>>,
    clock: SharedClock,
    blocking: Option<Arc<dyn uva::core::blocking::Blocking>>,
    compaction: Option<uva::session::CompactionPluginConfig>,
    sessions_dir: Option<PathBuf>,
    approval: Option<uva::approval::ApprovalPolicy>,
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
            approval: None,
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

    /// Attach the process's one clock, so a factory building a clocked
    /// plugin samples the same time source every other component reads.
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
        blocking: Option<Arc<dyn uva::core::blocking::Blocking>>,
    ) -> Self {
        self.blocking = blocking;
        self
    }

    /// Attach the deployment's compaction policy (the model and
    /// settings the `context-compactor` row builds its engine from).
    #[must_use]
    pub fn with_compaction(
        mut self,
        compaction: Option<uva::session::CompactionPluginConfig>,
    ) -> Self {
        self.compaction = compaction;
        self
    }

    /// The blocking-pool seam, when the deployment attaches one.
    #[must_use]
    pub fn blocking(&self) -> Option<Arc<dyn uva::core::blocking::Blocking>> {
        self.blocking.clone()
    }

    /// The compaction policy, when the profile supplies one.
    #[must_use]
    pub fn compaction(&self) -> Option<&uva::session::CompactionPluginConfig> {
        self.compaction.as_ref()
    }

    /// Attach the deployment's approval stance, which the
    /// `approval` row publishes as its service.
    #[must_use]
    pub fn with_approval(
        mut self,
        approval: Option<uva::approval::ApprovalPolicy>,
    ) -> Self {
        self.approval = approval;
        self
    }

    /// The deployment's approval stance, when the boot resolved one.
    #[must_use]
    pub fn approval(&self) -> Option<&uva::approval::ApprovalPolicy> {
        self.approval.as_ref()
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

/// The app-server's factory for one plugin name.
///
/// A factory owns the knowledge the kernel must not have: how to turn a
/// profile row into a concrete [`Plugin`]. The manifest it returns is
/// the same value the kernel will read at mount time, so the profile
/// loader and the kernel cannot disagree about `provides`.
///
/// A factory may also mark top-level config fields as **volatile**: a
/// change to only those fields updates the value the plugin reads next
/// time without a remount.
pub trait PluginFactory: Send + Sync {
    /// The plugin name this factory builds (the directory key rows
    /// reference through `name`).
    fn id(&self) -> &'static str;

    /// The manifest of the plugin this factory builds.
    fn manifest(&self) -> PluginManifest;

    /// Construct the plugin for `config` under the row's declared
    /// `capabilities`.
    ///
    /// The capabilities are the profile row's grants, already parsed
    /// into kernel capability names; a factory whose plugin confines
    /// itself by them copies them into the plugin's manifest.
    ///
    /// # Errors
    ///
    /// A construction failure is reported as [`PluginError`]; the
    /// profile loader wraps it in [`ProfileError::BuildFailed`] naming
    /// the row.
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

/// One plugin ready for the kernel, with its row's context derivation.
pub struct Mounted {
    /// The shell-wrapped plugin.
    pub plugin: Arc<dyn Plugin>,
    /// The per-row isolation and intercept derivation.
    pub row: MountRow,
}

/// Build every assembled core row in `profile`, in document order.
///
/// For each row: look the plugin `name` up in `factories`, check the
/// manifest's runtime fits the core segment, reject a core-side `ui.`
/// service key (whether declared by the manifest, the row's `inject`,
/// its `isolate` keys, or its `intercept` keys), resolve the
/// **effective** dependency set — the manifest's `inject` unioned with
/// the row's, the row winning on a shared key — then validate `config`
/// against the manifest's `config_schema` and call
/// [`PluginFactory::build`]. A group row skips the factory and mounts
/// as the runtime group plugin with its recursively built children.
///
/// The returned vector is ready for
/// [`uva::kernel::mount_all_with`]; dependency
/// ordering, cycle detection, and rollback stay the kernel's job.
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
    realms: &Arc<Realms>,
) -> Result<Vec<Mounted>, ProfileError> {
    profile.validate_unique()?;
    let mut built = Vec::new();
    for row in profile.core_rows() {
        built.push(build_one(row, factories, boot, realms)?);
    }
    Ok(built)
}

fn build_one(
    row: &Manifest,
    factories: &PluginFactories,
    boot: &BootContext,
    realms: &Arc<Realms>,
) -> Result<Mounted, ProfileError> {
    let context = row_context(row);
    if row.group {
        let mut children = Vec::new();
        for child in &row.children {
            // A disabled bit on the group or the child keeps the child
            // out; the group itself still mounts (an empty group is
            // observable nothing, dsh `Entry#disabled` walk).
            for assembled in child.assembled(row.disabled) {
                children.push(build_one(assembled, factories, boot, realms)?);
            }
        }
        let plugin = GroupShell {
            manifest: PluginManifest::new(row.id.clone(), "1.0"),
            children,
            realms: Arc::clone(realms),
            children_handle: Mutex::new(None),
        };
        return Ok(Mounted {
            plugin: Arc::new(plugin),
            row: context,
        });
    }
    let factory = factories.get(&row.name).ok_or_else(|| {
        ProfileError::UnknownPluginId {
            id: row.id.clone(),
            name: row.name.clone(),
            known: factories.ids(),
        }
    })?;
    let mut manifest = factory.manifest();
    if manifest.runtime != RuntimeKind::Native {
        return Err(ProfileError::SegmentMismatch {
            id: row.id.clone(),
            segment: "core",
            manifest: runtime_name(manifest.runtime),
        });
    }
    // The shell's manifest id is the ROW id: fibers are correlated with
    // document rows by id (the runtime write-back depends on it).
    manifest.id = row.id.clone();
    manifest.inject = effective_inject(&manifest.inject, row.inject.as_deref());
    let mut service_keys: Vec<&String> = manifest
        .provides
        .iter()
        .chain(manifest.inject.iter())
        .collect();
    if let Some(isolate) = &row.isolate {
        service_keys.extend(isolate.keys());
    }
    if let Some(intercept) = &row.intercept {
        service_keys.extend(intercept.keys());
    }
    for key in service_keys {
        if key.starts_with(uva::kernel::UI_PREFIX) {
            return Err(ProfileError::NamespaceViolation {
                id: row.id.clone(),
                key: key.clone(),
            });
        }
    }
    if let Some(schema) = &manifest.config_schema {
        validate_config(&row.config, schema).map_err(|message| {
            ProfileError::InvalidConfig {
                id: row.id.clone(),
                message,
            }
        })?;
    }
    let inner = factory
        .build(&row.config, &row.capabilities, boot)
        .map_err(|error| ProfileError::BuildFailed {
            id: row.id.clone(),
            message: error.to_string(),
        })?;
    Ok(Mounted {
        plugin: Arc::new(ManifestShell { inner, manifest }),
        row: context,
    })
}

/// The per-row context derivation: isolation scopes and intercept
/// overlays, in kernel terms.
fn row_context(row: &Manifest) -> MountRow {
    MountRow {
        isolate: row
            .isolate
            .as_ref()
            .map(|map| {
                map.iter()
                    .map(|(service, label)| {
                        let spec = match label {
                            IsolateLabel::Local => IsolateSpec::Local,
                            IsolateLabel::Shared(label) => {
                                IsolateSpec::Shared(label.clone())
                            }
                        };
                        (service.clone(), spec)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        intercept: row
            .intercept
            .as_ref()
            .map(|map| {
                map.iter()
                    .map(|(service, config)| (service.clone(), config.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Resolve a row's effective dependency set.
///
/// The plugin manifest's own `inject` comes first, then the row's extra
/// keys in declaration order; a key present in both keeps the row's
/// position last. Nothing is ever removed — the document can only
/// widen a plugin's dependencies, dsh's `Inject.resolve` union.
#[must_use]
pub(crate) fn effective_inject(
    factory: &[String],
    row: Option<&[String]>,
) -> Vec<String> {
    let mut merged = factory.to_vec();
    if let Some(row) = row {
        for key in row {
            if let Some(position) = merged.iter().position(|seen| seen == key) {
                merged.remove(position);
            }
            merged.push(key.clone());
        }
    }
    merged
}

/// A built plugin wrapped with the document's dependency declaration.
///
/// The kernel reads everything — validation, ordering, the fiber's
/// inject map — through [`Plugin::manifest`], so the shell is the one
/// place the resolved manifest exists; `apply` delegates to the inner
/// plugin untouched.
struct ManifestShell {
    /// The factory-built plugin.
    inner: Arc<dyn Plugin>,
    /// The factory manifest with the row's `inject` unioned in and the
    /// row id as its id.
    manifest: PluginManifest,
}

impl Plugin for ManifestShell {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        self.inner.apply(ctx)
    }
}

/// A group row's runtime plugin: mounts its children beneath its own
/// fiber, dsh's `Group`.
///
/// The children were built through the same factory pipeline (shells
/// included), so every kernel guarantee — manifests, validation,
/// rollback, parking — applies to them exactly as it does to top-level
/// rows. Unloading the group's fiber unloads the subtree.
struct GroupShell {
    /// Minimal group manifest: the group itself provides and requires
    /// nothing; its children declare their own edges.
    manifest: PluginManifest,
    /// The recursively built children with their context derivations.
    children: Vec<Mounted>,
    /// The process-wide label registry, so shared isolation labels
    /// stay stable across mounts and remounts.
    realms: Arc<Realms>,
    /// The children's mount ownership. Held here so the subtree lives
    /// exactly as long as the group's plugin (and therefore its
    /// fiber): when the fiber disposes, the shell drops with it and
    /// the children unload in reverse.
    children_handle: Mutex<Option<uva::kernel::MountHandle>>,
}

impl Plugin for GroupShell {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let rows: Vec<(Arc<dyn Plugin>, MountRow)> = self
            .children
            .iter()
            .map(|mounted| (Arc::clone(&mounted.plugin), mounted.row.clone()))
            .collect();
        let children_handle = mount_all_with(ctx, &self.realms, &rows)
            .map_err(|error| PluginError::new(error.to_string()))?;
        self.children_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(children_handle);
        Ok(Vec::new())
    }
}

/// The wire name of a runtime.
#[must_use]
pub(crate) fn runtime_name(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Native => "native",
        RuntimeKind::UiBundle => "ui-bundle",
    }
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
/// This is the manifest-free view: each row's `id`, `name`,
/// `capabilities`, final `config`, `disabled`, `inject`, `intercept`,
/// and `isolate`. It is byte-identical for the same input: rows keep
/// their document order and object keys are sorted at every depth.
///
/// [`dump_with_manifests`] adds the effective `provides` / `inject`
/// lists; use it for `--dump-config` once the factory directory is
/// populated.
#[must_use]
pub fn dump(profile: &Profile) -> String {
    render(profile, None)
}

/// Render the resolved tree with each core row's effective services.
///
/// `provides` comes from the factory manifest and `inject` is the
/// resolved union (factory manifest ∪ row declaration), so the dump
/// shows what the kernel will actually order and bind. A core row
/// whose name has no registered factory prints explicit empty lists
/// rather than omitting the fields; UI rows have no core factory and
/// also print empty lists.
pub fn dump_with_manifests(
    profile: &Profile,
    factories: &PluginFactories,
) -> String {
    render(profile, Some(factories))
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
    entries: &[Manifest],
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
        // no factory": print the row's own declaration. `None` means
        // the manifest-free dump, which omits the fields entirely.
        let services = factories
            .map(|factories| factories.get(&entry.name).map(|f| f.manifest()));
        out.push_str(&entry_pad);
        out.push_str("{\n");
        let mut first = true;
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"id\": ");
        out.push_str(&json_string(&entry.id));
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"name\": ");
        out.push_str(&json_string(&entry.name));
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"group\": ");
        out.push_str(if entry.group { "true" } else { "false" });
        begin_field(out, &field_pad, &mut first);
        out.push_str("\"disabled\": ");
        out.push_str(if entry.disabled { "true" } else { "false" });
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
        if entry.group {
            dump_entries(out, &entry.children, indent + 4, factories);
        } else {
            write_json(out, &entry.config, indent + 4);
        }
        // With factories available the effective union subsumes the
        // row's declaration and is printed once, inside the services
        // block below; the manifest-free dump prints the row's own.
        if services.is_none()
            && let Some(inject) = &entry.inject
        {
            begin_field(out, &field_pad, &mut first);
            out.push_str("\"inject\": ");
            write_json(
                out,
                &Value::Array(
                    inject
                        .iter()
                        .map(|key| Value::String(key.clone()))
                        .collect(),
                ),
                indent + 4,
            );
        }
        if let Some(intercept) = &entry.intercept {
            begin_field(out, &field_pad, &mut first);
            out.push_str("\"intercept\": ");
            write_json(
                out,
                &serde_json::to_value(intercept).unwrap_or(Value::Null),
                indent + 4,
            );
        }
        if let Some(isolate) = &entry.isolate {
            begin_field(out, &field_pad, &mut first);
            out.push_str("\"isolate\": ");
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
            write_json(out, &Value::Object(wire), indent + 4);
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
            let inject = effective_inject(&inject, entry.inject.as_deref());
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
