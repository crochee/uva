//! Kernel-owned effect handles that revoke on drop.

use std::sync::Arc;

/// A revocable registration: a service, an event listener, a tool, or a
/// cleanup callback.
///
/// Dropping the handle revokes the registration exactly once. Clones
/// share one guard, so the cleanup runs when the **last** clone is
/// dropped. This is the kernel's contract, not the framework's: Cordis
/// owns effects through the plugin fiber and its own handle drop is
/// inert, so the kernel wraps that handle and disposes it from the
/// guard's destructor.
///
/// `mount_all` retains the handles a plugin returns from
/// [`Plugin::apply`](crate::Plugin::apply) and drops them in reverse on
/// unload. An effect a plugin does not return stays owned by its fiber,
/// which remains the backstop for cleanup.
#[derive(Clone, Debug)]
pub struct EffectHandle(Arc<EffectGuard>);

#[derive(Debug)]
struct EffectGuard(cordis::EffectHandle);

impl Drop for EffectGuard {
    fn drop(&mut self) {
        if let Err(error) = self.0.dispose() {
            tracing::warn!(error = %error, "failed to dispose kernel effect");
        }
    }
}

impl EffectHandle {
    /// Wrap one framework effect. The kernel owns the resulting handle
    /// (and therefore the revocation).
    pub(crate) fn new(inner: cordis::EffectHandle) -> Self {
        Self(Arc::new(EffectGuard(inner)))
    }

    /// Whether the registration has already been revoked.
    ///
    /// A handle is disposed after its last clone is dropped, or after
    /// the owning fiber is unloaded.
    pub fn is_disposed(&self) -> bool {
        self.0.0.is_disposed()
    }
}
