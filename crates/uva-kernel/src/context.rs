//! The kernel context: a thin, revocable wrapper over the framework
//! context.

use std::sync::Arc;

use cordis::{ErrorCode, Event, EventResult};

use crate::{effect::EffectHandle, error::PluginError};

/// The kernel's view of a plugin context.
///
/// Every registration method returns a kernel [`EffectHandle`] whose
/// drop revokes that registration exactly once; clones share ownership,
/// so cleanup runs when the last clone drops. Registration and lookup
/// otherwise delegate to the wrapped Cordis context.
///
/// [`inner`](Self::inner) is the escape hatch for framework features
/// the kernel does not wrap yet (waterfall composition, for example).
#[derive(Clone)]
pub struct Context {
    inner: cordis::Context,
}

impl Context {
    /// Create a root context with the framework's core services
    /// installed.
    pub fn new() -> Self {
        Self {
            inner: cordis::Context::new(),
        }
    }

    /// Derive a child context with a fresh, independent service scope.
    ///
    /// The argument names the service key that branch isolates
    /// (matching the framework's scope model): a child created with
    /// `isolate("counter")` provides and reads its own `counter`, while
    /// the parent and any sibling created by another `isolate` call see
    /// no `counter` at all. Other keys are still shared with the
    /// ancestors.
    pub fn isolate(&self, name: impl Into<String>) -> Context {
        Self {
            inner: self.inner.isolate(name),
        }
    }

    /// Provide a shared service.
    ///
    /// Dropping the returned handle withdraws the service; a consumer's
    /// subsequent `require` reports it unavailable.
    pub fn provide_arc<T>(
        &self,
        key: impl Into<String>,
        value: Arc<T>,
    ) -> Result<EffectHandle, PluginError>
    where
        T: Send + Sync + 'static,
    {
        self.inner
            .provide_arc(key, value)
            .map(EffectHandle::new)
            .map_err(PluginError::from)
    }

    /// Provide a service, wrapping it in the context's shared storage.
    ///
    /// Dropping the returned handle withdraws the service.
    pub fn provide<T>(
        &self,
        key: impl Into<String>,
        value: T,
    ) -> Result<EffectHandle, PluginError>
    where
        T: Send + Sync + 'static,
    {
        self.inner
            .provide(key, value)
            .map(EffectHandle::new)
            .map_err(PluginError::from)
    }

    /// Register a cleanup callback.
    ///
    /// Dropping the returned handle runs the callback exactly once, in
    /// the reverse of registration order relative to the other effects
    /// the same owner drops.
    pub fn effect<F>(
        &self,
        label: impl Into<String>,
        dispose: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: FnOnce() -> Result<(), PluginError> + Send + 'static,
    {
        let handle = self
            .inner
            .effect(label, move || {
                dispose().map_err(|error| {
                    cordis::CordisError::with_message(
                        ErrorCode::Plugin,
                        error.to_string(),
                    )
                })
            })
            .map_err(PluginError::from)?;
        Ok(EffectHandle::new(handle))
    }

    /// Register a synchronous event listener.
    ///
    /// Dropping the returned handle removes the listener.
    pub fn on<F>(
        &self,
        name: impl Into<String>,
        listener: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(Event) -> EventResult + Send + Sync + 'static,
    {
        self.inner
            .on(name, listener)
            .map(EffectHandle::new)
            .map_err(PluginError::from)
    }

    /// Register an asynchronous event listener.
    ///
    /// Dropping the returned handle removes the listener.
    pub fn on_async<F, Fut>(
        &self,
        name: impl Into<String>,
        listener: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = EventResult> + Send + 'static,
    {
        self.inner
            .on_async(name, listener)
            .map(EffectHandle::new)
            .map_err(PluginError::from)
    }

    /// Resolve an active service, or report it unavailable.
    pub fn require<T>(&self, key: &str) -> Result<Arc<T>, PluginError>
    where
        T: Send + Sync + 'static,
    {
        self.inner.require(key).map_err(PluginError::from)
    }

    /// Resolve a service if it is currently active.
    pub fn get<T>(&self, key: &str) -> Result<Option<Arc<T>>, PluginError>
    where
        T: Send + Sync + 'static,
    {
        self.inner.get(key).map_err(PluginError::from)
    }

    /// Borrow the wrapped framework context.
    pub fn inner(&self) -> &cordis::Context {
        &self.inner
    }

    /// Adopt a framework context for a plugin activation.
    pub(crate) fn from_inner(inner: cordis::Context) -> Self {
        Self { inner }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}
