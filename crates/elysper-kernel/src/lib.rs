//! # elysper-kernel
//!
//! The plugin kernel: manifests, reversible mounting, and
//! lifecycle-owned effects. This is the one crate that names the
//! (pre-1.0) Cordis framework; everything else speaks the kernel's
//! contract instead.
//!
//! The kernel knows nothing about agents: it never depends on a
//! provider, tool, session, harness, app-server, or UI crate.
//!
//! ## Contract
//!
//! A [`Plugin`] declares a [`PluginManifest`] and registers services on
//! the [`Context`] from [`Plugin::apply`]. [`mount_all`] validates a
//! group, orders it by the manifest's `inject` declarations, and hands
//! each plugin to Cordis as a fiber. The returned [`MountHandle`] owns
//! the effects `apply` returned plus those fibers; dropping it revokes
//! the effects in reverse and disposes the fibers in reverse. A failed
//! `apply` rolls the whole group back. Hosts that unload one plugin
//! call [`MountHandle::dispose_plugin`]: the plugin's fiber and effects
//! go, its consumers park, and the rest of the group stays mounted.
//!
//! Every registration method on [`Context`] returns an
//! [`EffectHandle`] whose **drop revokes the registration exactly
//! once** — clones share the guard, so cleanup runs when the last clone
//! drops. This is the kernel's contract; the framework's own handle
//! drop is inert because the fiber owns its effects.

pub mod context;
pub mod effect;
pub mod error;
pub mod manifest;
pub mod mount;
pub mod plugin;
pub mod scope;
pub mod waterfall;

pub use context::Context;
pub use effect::EffectHandle;
pub use error::{MountError, PluginError, WaterfallError};
pub use manifest::{
    Capability,
    PLUGIN_PROTOCOL_VERSION,
    PluginManifest,
    RuntimeKind,
    ServiceKey,
    WasmAbi,
};
pub use mount::{MountHandle, PluginState, mount_all};
pub use plugin::Plugin;
pub use scope::{Namespace, ServiceHandle, UI_PREFIX, namespace_allows};
pub use waterfall::{
    BoxFuture,
    EventValue,
    Inner,
    Middleware,
    Next,
    Outcome,
    Waterfall,
    WaterfallCtx,
};
