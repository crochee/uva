# uva-kernel

The plugin kernel: **manifests, reversible mounting, and lifecycle-owned
effects**. It is the one crate that names the (pre-1.0) Cordis framework;
everything else — the harness, the built-in plugins, the app server —
speaks the kernel's contract instead of the framework's types.

The kernel knows nothing about agents. Its dependency graph must stay
free of provider, tool, session, harness, app-server, and UI crates; the
`check-kernel-deps` gate asserts that (added in a later task).

## Surface

- `PluginManifest` — `id`, `version` (the plugin protocol version),
  `runtime`, `provides`, `inject`, `capabilities`, and
  `config_schema`.
- `Context` — the kernel's revocable wrapper over the Cordis context:
  `provide`, `provide_arc`, `effect`, `on`, `on_async`, `require`,
  `get`, and `isolate`. `inner()` is the escape hatch for framework
  features not wrapped yet.
- `EffectHandle` — dropping it revokes the registration exactly once;
  clones share ownership, so cleanup runs when the last clone drops.
- `Plugin` — a named contribution with an `apply` that registers
  services, listeners, and tools on the kernel context.
- `mount_all` — validates a group (ids, protocol, duplicate providers,
  cycles, missing dependencies), mounts in dependency order, and returns
  a `MountHandle`.
- `MountHandle` — owns the effect handles `apply` returned plus the
  plugin fibers; unloads them in reverse on drop, and rolls the group
  back if an `apply` fails. `dispose_plugin(id)` unloads one plugin in
  place: its handles and fiber go, consumers that injected its services
  park (`PluginState::Pending`), and every sibling keeps running.
- `PluginState` — `Pending` / `Loading` / `Active` / `Failed` /
  `Unloading` / `Disposed`; `MountHandle::plugin_state(id)` answers with
  the kernel's vocabulary, not the framework's.
- `Namespace` / `namespace_allows` / `UI_PREFIX` — the `ui.` boundary.
  Core services must not carry the prefix; UI services must. The
  app-server's core loader rejects a core manifest declaring a `ui.`
  key, and its UI loader (`ui/schema`) rejects an entry outside it.
- `ui/schema` is the only UI transport: the frontend receives JSON
  schema (`version`, `slot`, `kind`, `props`) and renders it through
  its own component registry. A bundle never carries JavaScript or CSS.

## Example

```rust
use std::sync::Arc;

use uva_kernel::{
    Context, EffectHandle, PLUGIN_PROTOCOL_VERSION, Plugin, PluginError, PluginManifest, mount_all,
};

struct Counter {
    manifest: PluginManifest,
}

impl Counter {
    fn new() -> Self {
        let mut manifest = PluginManifest::new("counter", PLUGIN_PROTOCOL_VERSION);
        manifest.provides.push("counter".to_owned());
        Self { manifest }
    }
}

impl Plugin for Counter {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let value = Arc::new(42usize);
        Ok(vec![ctx.provide_arc("counter", value)?])
    }
}

let ctx = Context::new();
let handle = mount_all(&ctx, &[Arc::new(Counter::new())]).expect("mount");
assert!(ctx.get::<usize>("counter").expect("read").is_some());
drop(handle);
assert!(ctx.get::<usize>("counter").expect("read").is_none());
```

## Mounting rules

`mount_all` rejects, before mounting anything:

- an empty or whitespace-only plugin id;
- two plugins with the same id;
- one manifest listing the same service key twice in `provides`;
- two plugins that provide the same service key;
- a protocol `version` whose major differs from `PLUGIN_PROTOCOL_VERSION`;
- a dependency neither provided inside the group nor present in the
  parent context;
- a dependency cycle (the error lists the plugins left in the cycle).

Plugins mount in dependency order, with declaration order breaking ties.
If a plugin's `apply` fails (or panics), the failed fiber is disposed
first, then the previous plugins' tracked effect handles are dropped in
reverse, then their fibers are disposed in reverse — no effect is left
behind.

Every registration method on `Context` returns an `EffectHandle` whose
drop revokes the registration exactly once; clones share the guard, so
cleanup runs when the last clone is dropped. `MountHandle` retains the
handles a plugin returns from `apply` and drops them in reverse on
unload, then disposes the fibers in reverse. An effect a plugin does not
return stays owned by its fiber, which remains the backstop, so every
registration still has exactly one cleanup path.

`MountHandle::dispose_plugin(id)` is the single-plugin form of that
teardown. It drops exactly that plugin's tracked handles and fiber. The
framework parks a consumer whose injected service disappeared; the
consumer is not unmounted and its siblings are untouched. Disposal is
idempotent (an unknown or already-disposed id answers `false`), and
dropping the handle afterwards still cleans up whatever remains.

## Runtime neutrality

`cordis-rs 0.6` has no third-party dependencies and the kernel adds none
of its own that carry a runtime. Tests are synchronous `#[test]` fns;
lifecycle transitions are eager, so no executor is needed.
