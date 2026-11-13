//! `mount_all` contract: ordering, validation, rollback, and reverse
//! unloading.
//!
//! Every plugin here is a counting plugin: `apply` appends its label to
//! an order log, the cleanup handle returned by `ctx.effect` appends to a
//! cleanup log when it is dropped (group unload or rollback), and a
//! consumer resolves the provider's value with
//! `ctx.require::<AtomicUsize>`, which only succeeds after the provider
//! has mounted.

use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use uva_kernel::{
    Context,
    EffectHandle,
    MountError,
    PLUGIN_PROTOCOL_VERSION,
    Plugin,
    PluginError,
    PluginManifest,
    PluginState,
    RuntimeKind,
    mount_all,
};

/// Append-only order log shared by the test plugins.
type Order = Arc<Mutex<Vec<&'static str>>>;

const KEY: &str = "counter";

fn manifest(id: &str, provides: &[&str], inject: &[&str]) -> PluginManifest {
    PluginManifest {
        id: id.to_owned(),
        version: PLUGIN_PROTOCOL_VERSION.to_owned(),
        runtime: RuntimeKind::Native,
        provides: provides.iter().map(|key| (*key).to_owned()).collect(),
        inject: inject.iter().map(|key| (*key).to_owned()).collect(),
        capabilities: Vec::new(),
        config_schema: None,
    }
}

enum Behavior {
    /// Provide a service and stay active.
    Provide {
        key: &'static str,
        value: Arc<AtomicUsize>,
    },
    /// Require the provider's service, prove it is the expected value,
    /// and count the activation.
    Consume {
        key: &'static str,
        expected: Arc<AtomicUsize>,
    },
    /// Provide a service, register cleanup, then fail: the rollback
    /// test's third plugin.
    ProvideThenFail {
        key: &'static str,
        value: Arc<AtomicUsize>,
        message: &'static str,
    },
    /// Register cleanup, then panic: the kernel must treat a panicking
    /// `apply` as a failed one and roll the group back.
    Panic,
    /// Register nothing but cleanup.
    Noop,
}

struct TestPlugin {
    manifest: PluginManifest,
    label: &'static str,
    behavior: Behavior,
    applied: Order,
    cleaned: Order,
}

impl TestPlugin {
    fn new(
        manifest: PluginManifest,
        label: &'static str,
        behavior: Behavior,
        applied: Order,
        cleaned: Order,
    ) -> Self {
        Self {
            manifest,
            label,
            behavior,
            applied,
            cleaned,
        }
    }

    fn register_cleanup(
        &self,
        ctx: &Context,
    ) -> Result<EffectHandle, PluginError> {
        let cleaned = self.cleaned.clone();
        let label = self.label;
        ctx.effect(format!("cleanup:{}", self.label), move || {
            cleaned.lock().expect("cleanup log").push(label);
            Ok(())
        })
    }
}

impl Plugin for TestPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        self.applied.lock().expect("apply log").push(self.label);
        let mut effects = Vec::new();
        match &self.behavior {
            Behavior::Provide { key, value } => {
                effects.push(ctx.provide_arc(*key, value.clone())?);
            }
            Behavior::Consume { key, expected } => {
                let value = ctx.require::<AtomicUsize>(key)?;
                if !Arc::ptr_eq(&value, expected) {
                    return Err(PluginError::new(format!(
                        "`{}` resolved the wrong service instance",
                        self.label
                    )));
                }
                value.fetch_add(1, Ordering::SeqCst);
            }
            Behavior::ProvideThenFail {
                key,
                value,
                message,
            } => {
                effects.push(ctx.provide_arc(*key, value.clone())?);
                effects.push(self.register_cleanup(ctx)?);
                return Err(PluginError::new(*message));
            }
            Behavior::Panic => {
                effects.push(self.register_cleanup(ctx)?);
                panic!("intentional plugin panic");
            }
            Behavior::Noop => {}
        }
        effects.push(self.register_cleanup(ctx)?);
        Ok(effects)
    }
}

fn plugin(
    manifest: PluginManifest,
    label: &'static str,
    behavior: Behavior,
    applied: &Order,
    cleaned: &Order,
) -> Arc<dyn Plugin> {
    Arc::new(TestPlugin::new(
        manifest,
        label,
        behavior,
        applied.clone(),
        cleaned.clone(),
    ))
}

fn order() -> Order {
    Arc::new(Mutex::new(Vec::new()))
}

fn recorded(log: &Order) -> Vec<&'static str> {
    log.lock().expect("order log").clone()
}

#[test]
fn mounts_provider_before_consumer() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let value = Arc::new(AtomicUsize::new(0));

    // Declaration order is consumer first: the topological sort must
    // reorder it so the provider's service exists when `apply` runs.
    let consumer = plugin(
        manifest("consumer", &[], &[KEY]),
        "consumer",
        Behavior::Consume {
            key: KEY,
            expected: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let provider = plugin(
        manifest("provider", &[KEY], &[]),
        "provider",
        Behavior::Provide {
            key: KEY,
            value: value.clone(),
        },
        &applied,
        &cleaned,
    );

    let handle = mount_all(&ctx, &[consumer, provider]).expect("mount");
    assert_eq!(recorded(&applied), vec!["provider", "consumer"]);
    assert_eq!(value.load(Ordering::SeqCst), 1, "consumer activated");
    assert!(
        ctx.get::<AtomicUsize>(KEY).expect("read service").is_some(),
        "provider service is visible"
    );

    drop(handle);
    assert_eq!(recorded(&cleaned), vec!["consumer", "provider"]);
    assert!(
        ctx.get::<AtomicUsize>(KEY).expect("read service").is_none(),
        "provider service is withdrawn"
    );
}

#[test]
fn rejects_duplicate_provider() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let value = Arc::new(AtomicUsize::new(0));
    let first = plugin(
        manifest("first", &[KEY], &[]),
        "first",
        Behavior::Provide {
            key: KEY,
            value: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let second = plugin(
        manifest("second", &[KEY], &[]),
        "second",
        Behavior::Provide { key: KEY, value },
        &applied,
        &cleaned,
    );

    let error =
        mount_all(&ctx, &[first, second]).expect_err("duplicate provider");
    assert!(matches!(
        error,
        MountError::DuplicateProvider { ref key, .. } if key == KEY
    ));
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn rejects_duplicate_declaration() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let plugin = plugin(
        manifest("self-duplicate", &[KEY, KEY], &[]),
        "self-duplicate",
        Behavior::Noop,
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[plugin]).expect_err("duplicate declaration");
    match error {
        MountError::DuplicateDeclaration { plugin_id, key } => {
            assert_eq!(plugin_id, "self-duplicate");
            assert_eq!(key, KEY);
        }
        other => {
            panic!("expected a duplicate-declaration error, got {other:?}")
        }
    }
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn rejects_cycle() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let a = plugin(
        manifest("a", &["a"], &["b"]),
        "a",
        Behavior::Noop,
        &applied,
        &cleaned,
    );
    let b = plugin(
        manifest("b", &["b"], &["a"]),
        "b",
        Behavior::Noop,
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[a, b]).expect_err("cycle");
    match error {
        MountError::Cycle { plugins } => {
            assert_eq!(plugins.len(), 2);
            assert!(plugins.contains(&"a".to_owned()));
            assert!(plugins.contains(&"b".to_owned()));
        }
        other => panic!("expected a cycle error, got {other:?}"),
    }
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn rejects_missing_dependency() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let consumer = plugin(
        manifest("consumer", &[], &["missing"]),
        "consumer",
        Behavior::Consume {
            key: KEY,
            expected: Arc::new(AtomicUsize::new(0)),
        },
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[consumer]).expect_err("missing dependency");
    match error {
        MountError::MissingDependency { plugin_id, keys } => {
            assert_eq!(plugin_id, "consumer");
            assert_eq!(keys, vec!["missing".to_owned()]);
        }
        other => panic!("expected a missing-dependency error, got {other:?}"),
    }
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn parent_service_satisfies_dependency() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let parent_value = Arc::new(AtomicUsize::new(0));
    let parent_effect = ctx
        .provide_arc(KEY, parent_value.clone())
        .expect("provide parent service");

    let consumer = plugin(
        manifest("consumer", &[], &[KEY]),
        "consumer",
        Behavior::Consume {
            key: KEY,
            expected: parent_value.clone(),
        },
        &applied,
        &cleaned,
    );

    let handle = mount_all(&ctx, &[consumer]).expect("mount");
    assert_eq!(recorded(&applied), vec!["consumer"]);
    assert_eq!(parent_value.load(Ordering::SeqCst), 1);

    // The group does not own the parent's service.
    drop(handle);
    assert!(
        ctx.get::<AtomicUsize>(KEY).expect("read service").is_some(),
        "parent service survives the group unload"
    );

    drop(parent_effect);
    assert!(ctx.get::<AtomicUsize>(KEY).expect("read service").is_none());
}

#[test]
fn apply_failure_unwinds_previous_plugins() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let value = Arc::new(AtomicUsize::new(0));

    let a = plugin(
        manifest("a", &["a"], &[]),
        "a",
        Behavior::Provide {
            key: "a",
            value: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let b = plugin(
        manifest("b", &["b"], &[]),
        "b",
        Behavior::Provide {
            key: "b",
            value: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let c = plugin(
        manifest("c", &["c"], &[]),
        "c",
        Behavior::ProvideThenFail {
            key: "c",
            value,
            message: "boom",
        },
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[a, b, c]).expect_err("apply failure");
    match error {
        MountError::ApplyFailed { plugin_id, source } => {
            assert_eq!(plugin_id, "c");
            assert!(source.to_string().contains("boom"));
        }
        other => panic!("expected an apply failure, got {other:?}"),
    }

    assert_eq!(recorded(&applied), vec!["a", "b", "c"]);
    assert_eq!(
        recorded(&cleaned),
        vec!["c", "b", "a"],
        "failed plugin first, then previous fibers in reverse"
    );
    for key in ["a", "b", "c"] {
        assert!(
            ctx.get::<AtomicUsize>(key).expect("read service").is_none(),
            "service `{key}` left behind"
        );
    }
}

#[test]
fn apply_panic_unwinds_previous_plugins() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let value = Arc::new(AtomicUsize::new(0));

    let a = plugin(
        manifest("a", &["a"], &[]),
        "a",
        Behavior::Provide {
            key: "a",
            value: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let b = plugin(
        manifest("b", &[], &[]),
        "b",
        Behavior::Panic,
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[a, b]).expect_err("apply panic");
    match error {
        MountError::ApplyFailed { plugin_id, source } => {
            assert_eq!(plugin_id, "b");
            assert!(source.to_string().contains("intentional plugin panic"));
        }
        other => panic!("expected an apply failure, got {other:?}"),
    }

    assert_eq!(recorded(&applied), vec!["a", "b"]);
    assert_eq!(recorded(&cleaned), vec!["b", "a"]);
    assert!(
        ctx.get::<AtomicUsize>("a").expect("read service").is_none(),
        "previously mounted service left behind"
    );
}

#[test]
fn rejects_empty_id() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let plugin = plugin(
        manifest("", &[], &[]),
        "empty",
        Behavior::Noop,
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[plugin]).expect_err("empty id");
    assert!(matches!(error, MountError::EmptyPluginId { index: 0 }));
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn accepts_compatible_protocol_minor() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let mut compatible = manifest("compatible", &[], &[]);
    compatible.version = "1.5".to_owned();
    let plugin =
        plugin(compatible, "compatible", Behavior::Noop, &applied, &cleaned);

    let handle =
        mount_all(&ctx, &[plugin]).expect("minor version is compatible");
    assert_eq!(recorded(&applied), vec!["compatible"]);
    assert_eq!(handle.len(), 1);
    drop(handle);
    assert_eq!(recorded(&cleaned), vec!["compatible"]);
}

#[test]
fn drop_effect_handle_revokes_service() {
    let ctx = Context::new();
    let value = Arc::new(AtomicUsize::new(7));
    let handle = ctx.provide_arc(KEY, value).expect("provide");

    assert_eq!(
        ctx.require::<AtomicUsize>(KEY)
            .expect("require")
            .load(Ordering::SeqCst),
        7
    );
    assert!(ctx.get::<AtomicUsize>(KEY).expect("get").is_some());

    drop(handle);

    assert!(ctx.get::<AtomicUsize>(KEY).expect("get").is_none());
    assert!(matches!(
        ctx.require::<AtomicUsize>(KEY),
        Err(PluginError::Failed { .. })
    ));
}

#[test]
fn cloned_handle_revokes_once() {
    let ctx = Context::new();
    let value = Arc::new(AtomicUsize::new(0));
    let handle = ctx.provide_arc(KEY, value).expect("provide");
    let clone = handle.clone();

    drop(handle);
    assert!(
        ctx.get::<AtomicUsize>(KEY).expect("get").is_some(),
        "one clone still owns the service"
    );

    drop(clone);
    assert!(
        ctx.get::<AtomicUsize>(KEY).expect("get").is_none(),
        "the last clone revokes the service"
    );

    let runs = Arc::new(AtomicUsize::new(0));
    let runs_in = Arc::clone(&runs);
    let cleanup = ctx
        .effect("once", move || {
            runs_in.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("effect");
    let cleanup_clone = cleanup.clone();

    drop(cleanup);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "cleanup waits for the last clone"
    );

    drop(cleanup_clone);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "cleanup ran exactly once");
}

#[test]
fn effect_handle_drop_runs_cleanup_once() {
    let ctx = Context::new();
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_in = Arc::clone(&runs);
    let handle = ctx
        .effect("only-once", move || {
            runs_in.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("effect");

    assert_eq!(runs.load(Ordering::SeqCst), 0);
    drop(handle);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_listener_handle_unregisters() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in = Arc::clone(&calls);
    let handle = ctx
        .on("test/ping", move |_event| {
            calls_in.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
        .expect("listener");

    ctx.inner().emit("test/ping", []).expect("emit");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    drop(handle);
    ctx.inner().emit("test/ping", []).expect("emit");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "revoked listener is not called again"
    );
}

#[test]
fn rejects_duplicate_id() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let first = plugin(
        manifest("same", &["a"], &[]),
        "first",
        Behavior::Noop,
        &applied,
        &cleaned,
    );
    let second = plugin(
        manifest("same", &["b"], &[]),
        "second",
        Behavior::Noop,
        &applied,
        &cleaned,
    );

    let error = mount_all(&ctx, &[first, second]).expect_err("duplicate id");
    assert!(matches!(
        error,
        MountError::DuplicatePluginId { ref id } if id == "same"
    ));
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn rejects_incompatible_protocol() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let mut incompatible = manifest("future", &[], &[]);
    incompatible.version = "2.0".to_owned();
    let plugin =
        plugin(incompatible, "future", Behavior::Noop, &applied, &cleaned);

    let error = mount_all(&ctx, &[plugin]).expect_err("protocol mismatch");
    match error {
        MountError::ProtocolIncompatible {
            plugin_id,
            plugin_version,
            host_version,
        } => {
            assert_eq!(plugin_id, "future");
            assert_eq!(plugin_version, "2.0");
            assert_eq!(host_version, PLUGIN_PROTOCOL_VERSION);
        }
        other => panic!("expected a protocol error, got {other:?}"),
    }
    assert!(recorded(&applied).is_empty(), "nothing mounted");
}

#[test]
fn dispose_plugin_withdraws_one_provider_and_parks_its_consumers() {
    let ctx = Context::new();
    let applied = order();
    let cleaned = order();
    let value = Arc::new(AtomicUsize::new(0));

    let consumer = plugin(
        manifest("consumer", &[], &[KEY]),
        "consumer",
        Behavior::Consume {
            key: KEY,
            expected: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let provider = plugin(
        manifest("provider", &[KEY], &[]),
        "provider",
        Behavior::Provide {
            key: KEY,
            value: value.clone(),
        },
        &applied,
        &cleaned,
    );
    let sibling = plugin(
        manifest("sibling", &["sibling"], &[]),
        "sibling",
        Behavior::Provide {
            key: "sibling",
            value: Arc::new(AtomicUsize::new(7)),
        },
        &applied,
        &cleaned,
    );

    let mut handle =
        mount_all(&ctx, &[consumer, provider, sibling]).expect("mount");
    assert_eq!(handle.len(), 3);
    assert_eq!(handle.plugin_state("provider"), Some(PluginState::Active));

    assert!(
        handle.dispose_plugin("provider"),
        "the provider is mounted and is disposed"
    );
    assert_eq!(
        handle.plugin_state("consumer"),
        Some(PluginState::Pending),
        "a consumed service is withdrawn, so the consumer parks"
    );
    assert_eq!(
        handle.plugin_state("sibling"),
        Some(PluginState::Active),
        "an unrelated plugin stays mounted"
    );
    assert_eq!(
        handle.plugin_state("provider"),
        None,
        "a disposed plugin is no longer owned by the handle"
    );
    assert_eq!(handle.len(), 2);
    assert!(
        ctx.get::<AtomicUsize>(KEY).expect("read service").is_none(),
        "the provider service is withdrawn"
    );
    assert_eq!(
        recorded(&cleaned),
        vec!["provider", "consumer"],
        "the disposed provider's cleanup runs, and parking the \
         consumer withdraws the effects it had registered"
    );
    assert_eq!(
        value.load(Ordering::SeqCst),
        1,
        "the parked consumer is not re-applied"
    );

    assert!(
        !handle.dispose_plugin("provider"),
        "disposing an already-disposed plugin is a no-op"
    );
    assert!(!handle.dispose_plugin("unknown"));

    drop(handle);
    assert_eq!(
        recorded(&cleaned),
        vec!["provider", "consumer", "sibling"],
        "dropping the handle after a dispose stays idempotent"
    );
}
