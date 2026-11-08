//! Scope isolation, the shared root event bus, and the `ui.` /
//! core-key namespace boundary.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use cordis::{FiberState, Inject, PluginOutput, plugin_sync};
use elysper_kernel::{Context, Namespace, ServiceHandle, namespace_allows};

#[test]
fn isolated_scope_does_not_leak() {
    let root = Context::new();
    // `isolate(key)` reserves that service key for one branch: each call
    // gets a fresh scope label, so two branches provide/read their own
    // implementations of the same key.
    let child = root.isolate("counter");
    let sibling = root.isolate("counter");
    let value = Arc::new(AtomicUsize::new(7));

    let _service = child
        .provide_arc("counter", Arc::clone(&value))
        .expect("provide in the child scope");

    let seen = child
        .require::<AtomicUsize>("counter")
        .expect("child sees it");
    assert!(Arc::ptr_eq(&seen, &value));
    assert!(
        sibling
            .get::<AtomicUsize>("counter")
            .expect("read")
            .is_none(),
        "sibling scope must not see the child service"
    );
    assert!(
        root.get::<AtomicUsize>("counter").expect("read").is_none(),
        "parent must not see the child service"
    );
}

#[test]
fn child_shares_root_event_bus() {
    let root = Context::new();
    let child = root.isolate("child");
    let calls = Arc::new(AtomicUsize::new(0));

    let _listener = child
        .on("test/ping", {
            let calls = Arc::clone(&calls);
            move |_event| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        })
        .expect("child listener");

    root.inner().emit("test/ping", []).expect("parent emit");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "child listener sees the root event exactly once"
    );
}

#[test]
fn core_rejects_ui_key() {
    assert!(!namespace_allows(Namespace::Core, "ui.theme"));
    assert!(!namespace_allows(Namespace::Core, "ui."));
    assert!(
        namespace_allows(Namespace::Core, "llm"),
        "core keys stay available"
    );
}

#[test]
fn ui_rejects_core_key() {
    assert!(!namespace_allows(Namespace::Ui, "llm"));
    assert!(!namespace_allows(Namespace::Ui, "ui"));
    assert!(
        namespace_allows(Namespace::Ui, "ui.theme"),
        "prefixed UI keys stay available"
    );
}

#[test]
fn namespace_rejects_empty_key() {
    assert!(!namespace_allows(Namespace::Core, ""));
    assert!(!namespace_allows(Namespace::Ui, ""));
}

#[test]
fn provider_disposal_cascades_consumer_and_remount_reactivates() {
    let ctx = Context::new();
    let root = ctx.inner();
    let starts = Arc::new(AtomicUsize::new(0));

    let consumer = root.plugin_default(plugin_sync::<(), _>(
        "consumer",
        Inject::new(["counter"]),
        {
            let starts = Arc::clone(&starts);
            move |ctx, _| {
                ctx.require::<AtomicUsize>("counter")?;
                starts.fetch_add(1, Ordering::SeqCst);
                Ok(PluginOutput::none())
            }
        },
    ));
    assert_eq!(consumer.state(), FiberState::Pending);

    let provider =
        plugin_sync::<(), _>("provider", Inject::none(), |ctx, _| {
            ctx.provide_arc("counter", Arc::new(AtomicUsize::new(1)))?;
            Ok(PluginOutput::none())
        });

    let provider_fiber = root.plugin_default(provider.clone());
    assert_eq!(consumer.state(), FiberState::Active);
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    provider_fiber
        .dispose()
        .expect("dispose the provider fiber");
    assert_eq!(
        consumer.state(),
        FiberState::Pending,
        "consumer cascades to Pending when its provider unloads"
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "consumer is not re-applied while Pending"
    );

    let _remounted = root.plugin_default(provider);
    assert_eq!(
        consumer.state(),
        FiberState::Active,
        "remounting the provider reactivates the consumer"
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        2,
        "consumer apply runs again against the new provider"
    );
}

#[test]
fn service_handle_survives_provider_withdrawal() {
    let ctx = Context::new();
    let value = Arc::new(AtomicUsize::new(41));
    let provider = ctx
        .provide_arc("counter", Arc::clone(&value))
        .expect("provide");

    let handle = ServiceHandle::<AtomicUsize>::resolve(&ctx, "counter")
        .expect("resolve handle");
    assert!(Arc::ptr_eq(handle.get(), &value));

    drop(provider);
    assert!(
        ctx.get::<AtomicUsize>("counter").expect("read").is_none(),
        "the provider withdrawal is visible to the context"
    );
    assert_eq!(
        handle.get().load(Ordering::SeqCst),
        41,
        "the cached handle keeps the resolved instance alive"
    );

    assert!(
        ServiceHandle::<AtomicUsize>::resolve(&ctx, "counter").is_err(),
        "a fresh resolution after withdrawal fails"
    );
}
