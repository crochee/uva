//! The host side of the plugin → host event direction.
//!
//! Every other role a plugin can take is the host talking: it asks
//! for tools, it sends [`EVENT`](crate::protocol::methods::EVENT)
//! notifications, it hands over results to rewrite. `elysper/publish`
//! is the one direction where the plugin speaks first — a fact of its
//! own the host's side should record — so the host needs somewhere to
//! put what it hears. That somewhere is [`PluginEventSink`], and the
//! host is deliberately silent about what it does with the event: the
//! sink is the seam, and the consumer decides.
//!
//! The sink is **synchronous** because neither of its callers can
//! wait: the plugin channel's reader thread — a dedicated OS thread
//! that must never wait on the very channel it drains — and the
//! install path, whose settle delivers the frames buffered during
//! the handshake window. A consumer on an async runtime bridges with
//! a channel: `publish` sends, never blocks, and the runtime's task
//! drains the other end.
//!
//! The sink is **attached where the plugin is installed** (see
//! [`RemotePlugin::with_event_sink`](crate::host::RemotePlugin::with_event_sink)),
//! not per event: a plugin process cannot name a session it never
//! saw, so routing an event to the session (or sessions) it belongs
//! to is the installer's decision, made once at attach time.

use serde_json::Value;

/// An event a plugin published to the host.
///
/// Exactly what the plugin's
/// [`PUBLISH`](crate::protocol::methods::PUBLISH) params
/// said: a plugin-defined kind and a plugin-defined payload, with no
/// host interpretation. The payload is unbounded here — the wire
/// frame cap is the only limit — and the consumer that persists it
/// applies its own bound (see `SessionEvent::plugin` in
/// `elysper-session`, which caps the payload a durable row carries).
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedEvent {
    /// Plugin-defined event kind, e.g. `"custom"`.
    pub kind: String,
    /// Plugin-defined payload, as the plugin wrote it.
    pub payload: Value,
}

/// Where the host hands a plugin's published events to.
///
/// Installed on a [`RemotePlugin`](crate::host::RemotePlugin) with
/// [`with_event_sink`](crate::host::RemotePlugin::with_event_sink); the
/// plugin's handshake must also declare the `publish` capability, or
/// the notifications are dropped and the sink is never called.
pub trait PluginEventSink: Send + Sync {
    /// Handle one published event.
    ///
    /// **Must not block.** Two threads call this and neither can
    /// wait: the channel's reader thread (a slow `publish` delays
    /// every later frame from this plugin, including the answers its
    /// tools owe) and the install path on whatever runtime the host
    /// runs on — the settle that ends the handshake window delivers
    /// the frames buffered during it synchronously. Hand off (a
    /// channel send), do not process.
    fn publish(&self, event: PublishedEvent);
}
