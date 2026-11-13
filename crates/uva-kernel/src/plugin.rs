//! The contract every mountable plugin implements.

use crate::{
    context::Context,
    effect::EffectHandle,
    error::PluginError,
    manifest::PluginManifest,
};

/// One mountable contribution to the kernel.
///
/// A plugin is a value, not a lifecycle: the kernel wraps it in a Cordis
/// fiber and owns that fiber. `apply` runs when the fiber activates (its
/// declared `inject` services are available), and every registration it
/// makes through the context returns a kernel [`EffectHandle`]. Dropping
/// that handle revokes the registration exactly once (when the last
/// clone drops); the kernel also retains the returned handles so a group
/// unload revokes them in reverse, and the fiber is the backstop for
/// anything the plugin did not return.
///
/// Implementations must be cheap to share (`Send + Sync + 'static`):
/// `mount_all` clones an `Arc<dyn Plugin>` into the fiber.
pub trait Plugin: Send + Sync + 'static {
    /// This plugin's manifest.
    ///
    /// The value must be stable for the plugin's lifetime: the kernel
    /// reads it both before mounting (validation and ordering) and when
    /// wrapping the plugin in a fiber.
    fn manifest(&self) -> &PluginManifest;

    /// Register the plugin's services, listeners, and tools.
    ///
    /// `ctx` is the plugin fiber's context. Every kernel
    /// [`EffectHandle`] returned by [`Context::effect`],
    /// [`Context::provide`], or [`Context::provide_arc`] revokes its
    /// registration when dropped (once, when the last clone drops).
    /// Return the handles here so [`mount_all`](crate::mount_all) can
    /// own them and revoke them in reverse on unload; an effect the
    /// plugin does not return remains owned by its fiber, which is the
    /// backstop. Returning an empty vector is valid.
    ///
    /// # Errors
    ///
    /// Returning an error fails the mount: the handles created before
    /// the error revoke as they drop, the kernel disposes this fiber,
    /// then it revokes every previously mounted plugin's tracked effects
    /// in reverse and disposes their fibers in reverse. The error is
    /// [`MountError::ApplyFailed`](crate::MountError::ApplyFailed).
    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError>;
}
