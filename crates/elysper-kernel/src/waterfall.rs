//! The kernel's asynchronous middleware chain.
//!
//! A [`Waterfall`] composes an ordered list of [`Middleware`] around an
//! innermost callback. The first registration is the outermost layer and
//! runs first; a middleware decides whether the chain continues
//! ([`Outcome::Continue`]) or stops it with a value
//! ([`Outcome::ShortCircuit`]). Short-circuiting skips both the later
//! middleware and the inner callback.
//!
//! The chain is a snapshot at call time: [`Waterfall::run`] copies the
//! registry under the lock, releases the lock, and only then builds the
//! future. No guard is held across an `await`, and a middleware added
//! while a chain is running applies to the next call, never the current
//! one.
//!
//! Registration is reversible: the [`EffectHandle`] returned by
//! [`Waterfall::register`] removes that exact middleware when it is
//! dropped.
//!
//! The request view ([`WaterfallCtx`]) is read-only. A middleware can
//! reach the kernel [`Context`] through [`WaterfallCtx::services`] but
//! cannot mutate the event or payload another middleware sees.

use std::{
    fmt::{self, Debug, Formatter},
    sync::{Arc, Mutex, PoisonError},
};

pub use cordis::EventValue;

use crate::{Context, EffectHandle, PluginError, error::WaterfallError};

/// A boxed, sendable future, as used by the chain callbacks.
pub type BoxFuture<T> = cordis::utils::BoxFuture<T>;

/// The outcome of one chain pass.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Keep running: later middleware and then the inner callback.
    Continue,
    /// Stop the chain and use this value as the result. Later
    /// middleware and the inner callback are not called.
    ShortCircuit(EventValue),
}

/// The read-only request view handed to every layer.
pub struct WaterfallCtx {
    event: String,
    payload: EventValue,
    services: Context,
}

impl WaterfallCtx {
    /// Build a request view for `event`.
    pub fn new(
        event: impl Into<String>,
        payload: EventValue,
        services: Context,
    ) -> Self {
        Self {
            event: event.into(),
            payload,
            services,
        }
    }

    /// The event name this chain runs for.
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The payload that entered the chain.
    pub fn payload(&self) -> &EventValue {
        &self.payload
    }

    /// The service view the chain resolves its handles from.
    pub fn services(&self) -> &Context {
        &self.services
    }
}

impl Debug for WaterfallCtx {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaterfallCtx")
            .field("event", &self.event)
            .field("payload_type", &self.payload.type_name())
            .finish_non_exhaustive()
    }
}

/// The continuation a middleware calls to run the rest of the chain.
pub type Next = Arc<
    dyn Fn(Arc<WaterfallCtx>) -> BoxFuture<Result<Outcome, WaterfallError>>
        + Send
        + Sync,
>;

/// One layer of a [`Waterfall`].
pub type Middleware = Arc<
    dyn Fn(
            Arc<WaterfallCtx>,
            Next,
        ) -> BoxFuture<Result<Outcome, WaterfallError>>
        + Send
        + Sync,
>;

/// The innermost callback a waterfall wraps.
pub type Inner = Next;

struct Entry {
    middleware: Middleware,
}

/// An ordered middleware chain over an isolated [`Context`].
#[derive(Clone)]
pub struct Waterfall {
    ctx: Context,
    chain: Arc<Mutex<Vec<Arc<Entry>>>>,
}

impl Waterfall {
    /// Create an empty chain whose services resolve through `ctx`.
    pub fn new(ctx: &Context) -> Self {
        Self {
            ctx: ctx.clone(),
            chain: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Append a middleware and return the handle that removes it.
    ///
    /// The first registration is outermost: it sees the request first
    /// and may short-circuit before any later layer or the inner
    /// callback runs. Dropping the returned handle removes exactly this
    /// middleware; clones share the guard, so removal happens when the
    /// last clone drops.
    ///
    /// # Errors
    ///
    /// Returns the context's error if the removal effect cannot be
    /// registered. The middleware is not kept in that case.
    pub fn register(
        &self,
        middleware: Middleware,
    ) -> Result<EffectHandle, PluginError> {
        let entry = Arc::new(Entry { middleware });
        lock(&self.chain).push(Arc::clone(&entry));

        let chain = Arc::clone(&self.chain);
        let cleanup = Arc::clone(&entry);
        let effect = self.ctx.effect("waterfall:middleware", move || {
            let mut chain = lock(&chain);
            chain.retain(|candidate| !Arc::ptr_eq(candidate, &cleanup));
            Ok(())
        });
        if effect.is_err() {
            let mut chain = lock(&self.chain);
            chain.retain(|candidate| !Arc::ptr_eq(candidate, &entry));
        }
        effect
    }

    /// Run the chain, wrapping `inner` as its innermost callback.
    ///
    /// The registry is snapshotted before the future is built, so the
    /// lock is never held across an `await`. An empty chain calls
    /// `inner` directly.
    pub async fn run(
        &self,
        ctx: Arc<WaterfallCtx>,
        inner: Inner,
    ) -> Result<Outcome, WaterfallError> {
        let chain = self.snapshot();
        if chain.is_empty() {
            return inner(ctx).await;
        }

        let mut next = inner;
        for entry in chain.into_iter().rev() {
            let middleware = Arc::clone(&entry.middleware);
            let previous = next;
            next = Arc::new(move |ctx| middleware(ctx, Arc::clone(&previous)));
        }
        next(ctx).await
    }

    /// Number of registered middleware.
    pub fn len(&self) -> usize {
        lock(&self.chain).len()
    }

    /// Whether the chain has no middleware.
    pub fn is_empty(&self) -> bool {
        lock(&self.chain).is_empty()
    }

    fn snapshot(&self) -> Vec<Arc<Entry>> {
        lock(&self.chain).clone()
    }
}

fn lock(
    chain: &Mutex<Vec<Arc<Entry>>>,
) -> std::sync::MutexGuard<'_, Vec<Arc<Entry>>> {
    chain.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{Waterfall, WaterfallCtx};

    #[test]
    fn public_chain_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Waterfall>();
        assert_send_sync::<WaterfallCtx>();
    }
}
