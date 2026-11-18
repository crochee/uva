//! Profile / Bundle / Patch assembly and hot-reload contract.
//!
//! The tests use temporary TOML documents and fake [`PluginFactory`]s:
//! no provider credentials and no live kernel process are involved. The
//! two `mount_all` tests build real (fake) kernel plugins from the
//! profile and assert the kernel's own cycle / missing-dependency
//! vocabulary is what a bad profile surfaces.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde_json::{Value, json};
use uva::kernel::{
    Context,
    MountError,
    PLUGIN_PROTOCOL_VERSION,
    PluginState,
    Realms,
    mount_all_with,
};
use uva_app_server::profile::{
    BootContext,
    Capability,
    Manifest,
    Patch,
    Plugin,
    PluginError,
    PluginFactories,
    PluginFactory,
    PluginManifest,
    Profile,
    ProfileError,
    ProfileSource,
    ProfileWatcher,
    RuntimeKind,
    build_plugins,
    diff_profiles,
    dump,
    dump_with_manifests,
    resolve,
    resolve_files,
};

// -- fakes -----------------------------------------------------------

/// A factory that hands back a no-op plugin carrying the same manifest.
struct FakeFactory {
    id: &'static str,
    provides: Vec<String>,
    inject: Vec<String>,
    schema: Option<Value>,
    volatile: &'static [&'static str],
}

impl FakeFactory {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            provides: Vec::new(),
            inject: Vec::new(),
            schema: None,
            volatile: &[],
        }
    }

    fn provides(mut self, keys: &[&str]) -> Self {
        self.provides = keys.iter().map(|key| (*key).to_owned()).collect();
        self
    }

    fn inject(mut self, keys: &[&str]) -> Self {
        self.inject = keys.iter().map(|key| (*key).to_owned()).collect();
        self
    }

    fn schema(mut self, schema: Value) -> Self {
        self.schema = Some(schema);
        self
    }

    fn volatile(mut self, fields: &'static [&'static str]) -> Self {
        self.volatile = fields;
        self
    }

    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_owned(),
            version: PLUGIN_PROTOCOL_VERSION.to_owned(),
            runtime: RuntimeKind::Native,
            provides: self.provides.clone(),
            inject: self.inject.clone(),
            capabilities: Vec::new(),
            config_schema: self.schema.clone(),
        }
    }
}

impl PluginFactory for FakeFactory {
    fn id(&self) -> &'static str {
        self.id
    }

    fn manifest(&self) -> PluginManifest {
        FakeFactory::manifest(self)
    }

    fn build(
        &self,
        _config: &Value,
        _capabilities: &[Capability],
        _boot: &BootContext,
    ) -> Result<Arc<dyn Plugin>, PluginError> {
        Ok(Arc::new(FakePlugin {
            manifest: self.manifest(),
        }))
    }

    fn volatile_fields(&self) -> &'static [&'static str] {
        self.volatile
    }
}

struct FakePlugin {
    manifest: PluginManifest,
}

impl Plugin for FakePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        _ctx: &Context,
    ) -> Result<Vec<uva::kernel::EffectHandle>, PluginError> {
        Ok(Vec::new())
    }
}

fn factories(specs: Vec<FakeFactory>) -> PluginFactories {
    let mut registry = PluginFactories::new();
    for spec in specs {
        registry
            .register(Arc::new(spec))
            .expect("unique factory ids");
    }
    registry
}

fn boot() -> BootContext {
    BootContext::new("/tmp/uva-profile-test")
}

fn entry(id: &str, config: Value) -> Manifest {
    Manifest {
        config,
        ..Manifest::plain(id, id)
    }
}

fn profile(hmr: bool, entries: Vec<Manifest>) -> Profile {
    Profile {
        core: entries,
        ui: Vec::new(),
        hmr,
    }
}

/// The isolation-scope registry the assembly mounts against.
fn realms() -> Arc<Realms> {
    Arc::new(Realms::default())
}

fn profile_error<T>(
    result: Result<T, ProfileError>,
    context: &str,
) -> ProfileError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

fn mount_error<T>(result: Result<T, MountError>, context: &str) -> MountError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

fn find<'a>(profile: &'a Profile, id: &str) -> &'a Manifest {
    profile
        .core
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| {
            panic!("entry `{id}` missing from the resolved profile")
        })
}

// -- assembly --------------------------------------------------------

#[test]
fn later_layer_replaces_whole_config() {
    let bundle = Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "a", temperature = 0.2 } },
          { id = "tools", name = "tools" },
        ]
        "#,
    )
    .expect("bundle profile parses");

    let profile_layer = Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "b" } },
        ]
        "#,
    )
    .expect("profile layer parses");

    let merged = resolve(
        &[
            ProfileSource::Profile(bundle),
            ProfileSource::Profile(profile_layer),
        ],
        None,
    )
    .expect("layers resolve");

    let llm = find(&merged, "llm");
    assert_eq!(llm.config, json!({ "model": "b" }));
    assert!(
        llm.config.get("temperature").is_none(),
        "a later layer replaces the whole config, never deep-merges it"
    );
    assert_eq!(merged.core.len(), 2, "the untouched entry survives");
    assert_eq!(
        merged.core[0].id, "llm",
        "replacement keeps the original position"
    );

    // The CLI overlay is the last layer and wins over the profile.
    let overlay = Patch::from_toml_str(
        r#"
        [[replace]]
        id = "llm"
        config = { model = "cli" }
        "#,
    )
    .expect("overlay parses");
    let merged = resolve(
        &[
            ProfileSource::Profile(bundle_again()),
            ProfileSource::Profile(profile_layer_again()),
        ],
        Some(overlay),
    )
    .expect("overlay resolves");
    assert_eq!(find(&merged, "llm").config, json!({ "model": "cli" }));
}

fn bundle_again() -> Profile {
    Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "a" } },
          { id = "tools", name = "tools" },
        ]
        "#,
    )
    .expect("bundle profile parses")
}

fn profile_layer_again() -> Profile {
    Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "b" } },
        ]
        "#,
    )
    .expect("profile layer parses")
}

#[test]
fn patch_layers_apply_in_order() {
    let base = Profile::from_toml_str(
        r#"
        [core]
        entries = [{ id = "llm", name = "llm", config = { model = "a" } }]
        "#,
    )
    .expect("profile parses");
    let patch = Patch::from_toml_str(
        r#"
        [[replace]]
        id = "llm"
        config = { model = "patch" }
        "#,
    )
    .expect("patch parses");

    let resolved = resolve(
        &[ProfileSource::Profile(base), ProfileSource::Patch(patch)],
        None,
    )
    .expect("patch layers resolve");
    assert_eq!(find(&resolved, "llm").config, json!({ "model": "patch" }));
}

#[test]
fn disabled_prevents_mount() {
    let base = Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "a" } },
          { id = "tools", name = "tools" },
        ]
        "#,
    )
    .expect("profile parses");
    let patch =
        Patch::from_toml_str("disabled = [\"llm\"]").expect("patch parses");

    let resolved = resolve(&[ProfileSource::Profile(base)], Some(patch))
        .expect("disabled resolves");
    assert_eq!(resolved.core.len(), 1);
    assert_eq!(resolved.core[0].id, "tools");

    let factories =
        factories(vec![FakeFactory::new("llm"), FakeFactory::new("tools")]);
    let built = build_plugins(&resolved, &factories, &boot(), &realms())
        .expect("the remaining entry builds");
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].plugin.manifest().id, "tools");
}

#[test]
fn insert_appends_entry() {
    let base = Profile::from_toml_str(
        r#"
        [core]
        entries = [{ id = "llm", name = "llm" }]
        "#,
    )
    .expect("profile parses");
    let patch = Patch::from_toml_str(
        r#"
        [[insert]]
        id = "extra"
        name = "extra"
        config = { n = 1 }
        "#,
    )
    .expect("patch parses");

    let resolved = resolve(&[ProfileSource::Profile(base)], Some(patch))
        .expect("insert resolves");
    assert_eq!(
        resolved
            .core
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["llm", "extra"]
    );
    assert_eq!(find(&resolved, "extra").config, json!({ "n": 1 }));

    let factories =
        factories(vec![FakeFactory::new("llm"), FakeFactory::new("extra")]);
    let built = build_plugins(&resolved, &factories, &boot(), &realms())
        .expect("inserted entry builds");
    assert_eq!(built.len(), 2);
}

#[test]
fn unknown_patch_id_fails() {
    let base = Profile::from_toml_str(
        r#"
        [core]
        entries = [{ id = "llm", name = "llm" }]
        "#,
    )
    .expect("profile parses");
    let patch = Patch::from_toml_str(
        r#"
        [[replace]]
        id = "missing"
        config = { model = "b" }
        "#,
    )
    .expect("patch parses");

    let error = resolve(&[ProfileSource::Profile(base)], Some(patch))
        .expect_err("an unknown patch id is an error");
    assert!(
        matches!(&error, ProfileError::UnknownPatchId { id } if id == "missing"),
        "unexpected error: {error}"
    );
}

#[test]
fn duplicate_id_in_one_layer_fails() {
    let error = Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm" },
          { id = "llm", name = "llm" },
        ]
        "#,
    )
    .expect_err("a duplicate id in one layer is an error");
    assert!(
        matches!(&error, ProfileError::DuplicateId { id } if id == "llm"),
        "unexpected error: {error}"
    );
}

#[test]
fn core_loader_rejects_ui_key() {
    // The other half of the namespace boundary: a core plugin may not
    // claim a `ui.` service, just as the UI loader refuses a core key.
    let factories =
        factories(vec![FakeFactory::new("theme").provides(&["ui.theme"])]);
    let profile = profile(false, vec![entry("theme", json!({}))]);

    let error = profile_error(
        build_plugins(&profile, &factories, &boot(), &realms()),
        "a core manifest declaring `ui.theme` is rejected",
    );
    assert!(
        matches!(
            &error,
            ProfileError::NamespaceViolation { id, key }
                if id == "theme" && key == "ui.theme"
        ),
        "unexpected error: {error}"
    );
}

// -- dump ------------------------------------------------------------

#[test]
fn dump_is_deterministic() {
    let source = r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "b", temperature = 0.2 } },
          { id = "tools", name = "tools" },
        ]
        "#;
    let first = resolve(
        &[ProfileSource::Profile(
            Profile::from_toml_str(source).expect("profile parses"),
        )],
        None,
    )
    .expect("resolves");
    let second = resolve(
        &[ProfileSource::Profile(
            Profile::from_toml_str(source).expect("profile parses"),
        )],
        None,
    )
    .expect("resolves");

    let dumped = dump(&first);
    assert_eq!(dumped, dump(&first), "same value dumps identically");
    assert_eq!(dumped, dump(&second), "same input dumps byte-identically");
    assert!(dumped.contains("\"model\": \"b\""), "{dumped}");
    assert!(dumped.contains("\"temperature\": 0.2"), "{dumped}");
}
#[test]
fn dump_with_the_boot_factories_prints_manifest_services() {
    let profile = Profile::from_toml_str(
        r#"
        [core]
        entries = [
          { id = "tool-registry", name = "tool-registry" },
          { id = "skills", name = "skills" },
          { id = "unregistered", name = "unregistered" },
        ]

        [ui]
        entries = [
          { id = "ui.slot.chat.sidebar", name = "ui.slot.chat.sidebar", config = {} },
        ]
        "#,
    )
    .expect("profile parses");

    let dumped = uva_app_server::boot::dump_profile(&profile);
    let parsed: Value =
        serde_json::from_str(&dumped).expect("the dump is valid JSON");

    assert_eq!(parsed["core"][0]["provides"], json!(["tools"]));
    assert_eq!(parsed["core"][0]["inject"], json!([]));
    assert_eq!(parsed["core"][1]["provides"], json!(["skills"]));
    assert_eq!(parsed["core"][1]["inject"], json!(["tools"]));
    // A row with no registered factory is explicit, not omitted.
    assert_eq!(parsed["core"][2]["provides"], json!([]));
    assert_eq!(parsed["core"][2]["inject"], json!([]));
    // The UI segment has no core factory, so it also prints empty lists.
    assert_eq!(parsed["ui"][0]["provides"], json!([]));
    assert_eq!(parsed["ui"][0]["inject"], json!([]));
}

// -- hot reload ------------------------------------------------------

#[test]
fn a_ui_row_never_reaches_the_core_loader() {
    // A row's segment is its position in the document: a `[ui]` row
    // belongs to the frontend loader, so it never rides the core list
    // and never reaches the core loader's `ui.` namespace check.
    let profile = Profile::from_toml_str(
        r#"
        [core]
        entries = [{ id = "tools", name = "tools" }]

        [ui]
        entries = [{ id = "ui.shell", name = "ui.shell", config = {} }]
        "#,
    )
    .expect("profile parses");

    let resolved =
        resolve(&[ProfileSource::Profile(profile)], None).expect("resolves");
    assert_eq!(resolved.ui.len(), 1);
    assert_eq!(resolved.ui[0].id, "ui.shell");
    assert_eq!(resolved.core.len(), 1);
    assert_eq!(resolved.core[0].id, "tools");

    // The core loader builds the core rows only: the ui row is not
    // built, so it needs no factory.
    let factories = factories(vec![FakeFactory::new("tools")]);
    let built = build_plugins(&resolved, &factories, &boot(), &realms())
        .expect("the core row builds");
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].plugin.manifest().id, "tools");
}

#[test]
fn hmr_remounts_only_affected_fiber() {
    let factories = factories(vec![
        FakeFactory::new("llm").volatile(&["api_key"]),
        FakeFactory::new("tools"),
    ]);

    let previous = profile(
        true,
        vec![
            entry("llm", json!({ "model": "a", "api_key": "key-1" })),
            entry("tools", json!({})),
        ],
    );
    let changed_model = profile(
        true,
        vec![
            entry("llm", json!({ "model": "b", "api_key": "key-1" })),
            entry("tools", json!({})),
        ],
    );

    let diff = diff_profiles(&previous, &changed_model, &factories);
    assert_eq!(diff.remounted, vec!["llm".to_owned()]);
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert!(diff.volatile_changed.is_empty());
    assert!(!diff.restart_required);

    // A volatile-only change does not remount the fiber.
    let changed_volatile = profile(
        true,
        vec![
            entry("llm", json!({ "model": "a", "api_key": "key-2" })),
            entry("tools", json!({})),
        ],
    );
    let diff = diff_profiles(&previous, &changed_volatile, &factories);
    assert!(diff.remounted.is_empty());
    assert_eq!(diff.volatile_changed, vec!["llm".to_owned()]);
    assert!(!diff.restart_required);

    // With HMR off the process does not reload; a restart is required.
    let previous_off = Profile {
        hmr: false,
        ..previous.clone()
    };
    let changed_off = Profile {
        hmr: false,
        ..changed_model.clone()
    };
    let diff = diff_profiles(&previous_off, &changed_off, &factories);
    assert!(diff.restart_required);
    assert!(diff.remounted.is_empty());
    assert!(diff.added.is_empty() && diff.removed.is_empty());

    // Turning HMR off in the new profile also requires a restart: the
    // running process must not keep reloading after the flag is gone.
    let hmr_turned_off = Profile {
        hmr: false,
        ..changed_model.clone()
    };
    let diff = diff_profiles(&previous, &hmr_turned_off, &factories);
    assert!(diff.restart_required, "{diff:?}");
    assert!(diff.remounted.is_empty());
}

/// A config change on a provider cascades to its injectors: the
/// dependent must remount against the new service value or it keeps
/// the stale one for the rest of the process's life.
#[test]
fn hmr_remount_cascades_to_direct_injectors() {
    let factories = factories(vec![
        FakeFactory::new("model-adapter").provides(&["llm", "llm.default"]),
        FakeFactory::new("agent-loop").inject(&["llm"]),
    ]);
    let previous = profile(
        true,
        vec![
            entry("model-adapter", json!({ "model": "a" })),
            entry("agent-loop", json!({})),
        ],
    );
    let changed = profile(
        true,
        vec![
            entry("model-adapter", json!({ "model": "b" })),
            entry("agent-loop", json!({})),
        ],
    );

    let diff = diff_profiles(&previous, &changed, &factories);
    let mut remounted = diff.remounted.clone();
    remounted.sort();
    assert_eq!(
        remounted,
        vec!["agent-loop".to_owned(), "model-adapter".to_owned()],
        "the changed row and its injector both remount: {diff:?}"
    );
    assert!(diff.added.is_empty() && diff.removed.is_empty());
}

/// The cascade is transitive: `a` provides `s1`, `b` injects `s1`
/// and provides `s2`, `c` injects `s2`. Changing `a`'s config
/// remounts all three.
#[test]
fn hmr_remount_cascade_is_transitive() {
    let factories = factories(vec![
        FakeFactory::new("a").provides(&["s1"]),
        FakeFactory::new("b").provides(&["s2"]).inject(&["s1"]),
        FakeFactory::new("c").inject(&["s2"]),
        FakeFactory::new("bystander"),
    ]);
    let previous = profile(
        true,
        vec![
            entry("a", json!({ "v": 1 })),
            entry("b", json!({})),
            entry("c", json!({})),
            entry("bystander", json!({})),
        ],
    );
    let changed = profile(
        true,
        vec![
            entry("a", json!({ "v": 2 })),
            entry("b", json!({})),
            entry("c", json!({})),
            entry("bystander", json!({})),
        ],
    );

    let diff = diff_profiles(&previous, &changed, &factories);
    let mut remounted = diff.remounted.clone();
    remounted.sort();
    assert_eq!(
        remounted,
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        "the whole injection chain remounts, the bystander does not: \
         {diff:?}"
    );
}

/// Removing a provider does NOT cascade: unloading it withdraws the
/// services and the kernel parks the injectors itself, so a rebuild
/// would mount a dependent with no provider (and fail the group).
#[test]
fn hmr_removed_provider_does_not_cascade() {
    let factories = factories(vec![
        FakeFactory::new("model-adapter").provides(&["llm"]),
        FakeFactory::new("agent-loop").inject(&["llm"]),
    ]);
    let previous = profile(
        true,
        vec![
            entry("model-adapter", json!({})),
            entry("agent-loop", json!({})),
        ],
    );
    let changed = profile(true, vec![entry("agent-loop", json!({}))]);

    let diff = diff_profiles(&previous, &changed, &factories);
    assert_eq!(diff.removed, vec!["model-adapter".to_owned()]);
    assert!(
        diff.remounted.is_empty(),
        "the injector is parked by the kernel, not rebuilt: {diff:?}"
    );
}

#[test]
fn watcher_polls_mtime_without_a_live_process() {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(file, "hmr = true").expect("write");
    file.flush().expect("flush");

    let mut watcher = ProfileWatcher::new(vec![file.path().to_path_buf()]);
    assert!(!watcher.poll().expect("first poll records stamps"));

    // Bump the mtime into the future so the test does not depend on the
    // filesystem's timestamp granularity.
    let handle = OpenOptions::new()
        .write(true)
        .open(file.path())
        .expect("open for mtime bump");
    handle
        .set_modified(SystemTime::now() + Duration::from_secs(5))
        .expect("set mtime");
    drop::<File>(handle);

    assert!(watcher.poll().expect("second poll sees the change"));
    assert!(!watcher.poll().expect("third poll is stable"));
}

// -- factory binding -------------------------------------------------

#[test]
fn build_plugins_unknown_id_fails() {
    let profile = profile(false, vec![entry("missing", json!({}))]);
    let error = profile_error(
        build_plugins(&profile, &PluginFactories::new(), &boot(), &realms()),
        "an unregistered id is an error",
    );
    assert!(
        matches!(&error, ProfileError::UnknownPluginId { id, .. } if id == "missing"),
        "unexpected error: {error}"
    );
}

#[test]
fn build_plugins_validates_config_against_the_manifest_schema() {
    let schema = json!({
        "type": "object",
        "required": ["model"],
        "properties": {
            "model": { "type": "string" },
            "temperature": { "type": "number" },
        },
        "additionalProperties": false,
    });
    let factories = factories(vec![FakeFactory::new("llm").schema(schema)]);

    let valid = profile(false, vec![entry("llm", json!({ "model": "gpt" }))]);
    assert!(build_plugins(&valid, &factories, &boot(), &realms()).is_ok());

    let wrong_type = profile(
        false,
        vec![entry("llm", json!({ "model": "a", "temperature": "hot" }))],
    );
    let error = profile_error(
        build_plugins(&wrong_type, &factories, &boot(), &realms()),
        "a wrong value type is rejected",
    );
    assert!(
        matches!(&error, ProfileError::InvalidConfig { id, message }
            if id == "llm" && message.contains("temperature")),
        "unexpected error: {error}"
    );

    let missing = profile(false, vec![entry("llm", json!({}))]);
    let error = profile_error(
        build_plugins(&missing, &factories, &boot(), &realms()),
        "a missing required field is rejected",
    );
    assert!(
        matches!(&error, ProfileError::InvalidConfig { message, .. }
            if message.contains("model")),
        "unexpected error: {error}"
    );

    let extra = profile(
        false,
        vec![entry("llm", json!({ "model": "a", "prototype": true }))],
    );
    let error = profile_error(
        build_plugins(&extra, &factories, &boot(), &realms()),
        "an additional property is rejected",
    );
    assert!(
        matches!(&error, ProfileError::InvalidConfig { message, .. }
            if message.contains("prototype")),
        "unexpected error: {error}"
    );
}

#[test]
fn mount_all_surfaces_cycle_and_missing_dependency() {
    let cycle =
        profile(false, vec![entry("a", json!({})), entry("b", json!({}))]);
    let cycle_factories = factories(vec![
        FakeFactory::new("a").provides(&["a"]).inject(&["b"]),
        FakeFactory::new("b").provides(&["b"]).inject(&["a"]),
    ]);
    let scope = realms();
    let plugins = build_plugins(&cycle, &cycle_factories, &boot(), &scope)
        .expect("plugins build");
    let rows: Vec<_> = plugins
        .into_iter()
        .map(|mounted| (mounted.plugin, mounted.row))
        .collect();
    let error = mount_error(
        mount_all_with(&Context::new(), &scope, &rows),
        "a dependency cycle fails the mount",
    );
    assert!(matches!(error, MountError::Cycle { .. }), "got {error}");

    // A missing dependency parks the row instead of failing the mount:
    // the fiber waits for its provider (dsh's PENDING semantics).
    let missing = profile(false, vec![entry("a", json!({}))]);
    let missing_factories =
        factories(vec![FakeFactory::new("a").inject(&["absent"])]);
    let scope = realms();
    let plugins = build_plugins(&missing, &missing_factories, &boot(), &scope)
        .expect("plugins build");
    let rows: Vec<_> = plugins
        .into_iter()
        .map(|mounted| (mounted.plugin, mounted.row))
        .collect();
    let handle = mount_all_with(&Context::new(), &scope, &rows)
        .expect("a parked row is not a failed mount");
    assert_eq!(
        handle.plugin_state("a"),
        Some(PluginState::Pending),
        "the row waits for its missing dependency"
    );
}

#[test]
fn capabilities_are_parsed_and_rendered() {
    let profile = Profile::from_toml_str(
        r#"
        [core]
        entries = [{ id = "web-tool", name = "web-tool", capabilities = ["net:http", "log"], config = {} }]
        "#,
    )
    .expect("profile with capabilities parses");
    let entry = find(&profile, "web-tool");
    assert_eq!(entry.capabilities.len(), 2);
    assert_eq!(entry.capabilities[0].to_string(), "net:http");

    let parsed: Value =
        serde_json::from_str(&dump(&profile)).expect("valid JSON");
    assert_eq!(
        parsed["core"][0]["capabilities"],
        json!(["net:http", "log"])
    );
}

// -- manifest-aware dump --------------------------------------------

#[test]
fn dump_with_manifests_includes_provides_and_inject() {
    let factories = factories(vec![
        FakeFactory::new("llm")
            .provides(&["llm"])
            .inject(&["config"]),
        FakeFactory::new("tools").provides(&["tools"]),
    ]);
    let profile = profile(
        false,
        vec![
            entry("llm", json!({ "model": "a" })),
            entry("tools", json!({})),
            entry("unbound", json!({})),
        ],
    );

    let dumped = dump_with_manifests(&profile, &factories);
    let parsed: Value =
        serde_json::from_str(&dumped).expect("the dump is valid JSON");
    assert_eq!(parsed["core"][0]["config"]["model"], json!("a"));
    assert_eq!(parsed["core"][0]["provides"], json!(["llm"]));
    assert_eq!(parsed["core"][0]["inject"], json!(["config"]));
    assert_eq!(parsed["core"][1]["provides"], json!(["tools"]));
    assert_eq!(parsed["core"][1]["inject"], json!([]));
    // An id with no registered factory prints explicit empty lists
    // instead of silently omitting the fields.
    assert_eq!(parsed["core"][2]["provides"], json!([]));
    assert_eq!(parsed["core"][2]["inject"], json!([]));

    // The manifest-free dump keeps the older, manifest-less shape.
    let plain: Value =
        serde_json::from_str(&dump(&profile)).expect("valid JSON");
    assert!(plain["core"][0].get("provides").is_none());
}

// -- file layers -----------------------------------------------------

#[test]
fn bundle_profile_home_and_overlay_apply_in_order() {
    let dir = tempfile::tempdir().expect("temp dir");
    let bundle = dir.path().join("bundle.toml");
    let profile_path = dir.path().join("profile.toml");
    let home = dir.path().join("home.toml");
    let overlay = dir.path().join("overlay.toml");
    std::fs::write(
        &bundle,
        r#"
        [[insert]]
        id = "llm"
        name = "llm"
        config = { model = "bundle", temperature = 0.2 }

        [[insert]]
        id = "tools"
        name = "tools"
        "#,
    )
    .expect("bundle");
    std::fs::write(
        &profile_path,
        r#"
        [core]
        entries = [
          { id = "llm", name = "llm", config = { model = "profile" } },
        ]
        "#,
    )
    .expect("profile");
    std::fs::write(
        &home,
        r#"
        [[replace]]
        id = "llm"
        config = { model = "home" }
        "#,
    )
    .expect("home");
    std::fs::write(
        &overlay,
        r#"
        [[replace]]
        id = "llm"
        config = { model = "cli" }
        "#,
    )
    .expect("overlay");

    let resolved = resolve_files(
        Some(&bundle),
        Some(&profile_path),
        Some(&home),
        Some(&overlay),
    )
    .expect("file layers resolve");

    // The CLI overlay is last.
    assert_eq!(find(&resolved, "llm").config, json!({ "model": "cli" }));
    // The profile replaced the bundle row whole; `temperature` is gone.
    assert!(find(&resolved, "llm").config.get("temperature").is_none());
    // A bundle row the profile did not mention survives.
    assert!(resolved.contains("tools"));

    // Without the overlay the home layer wins over the profile.
    let resolved =
        resolve_files(Some(&bundle), Some(&profile_path), Some(&home), None)
            .expect("home layer resolves");
    assert_eq!(find(&resolved, "llm").config, json!({ "model": "home" }));
}
