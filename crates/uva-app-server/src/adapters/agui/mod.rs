//! The AG-UI adapter: the web front-end's door.
//!
//! The browser does not speak ACP, and it should not: ACP is the
//! headless control plane's language — editors, terminals, any
//! ACP-capable client. The browser needs a stream it can render, so
//! the server projects the core's own events into AG-UI and hands
//! the browser that.
//!
//! # What this adapter is
//!
//! A projection, nothing more: the core's event stream in, AG-UI
//! events out. It never decides what a run means, and it never
//! reaches past the stream into the harness's internals. That is what
//! keeps the layering honest — the core says what happened, and each
//! adapter says how its protocol says it.
//!
//! # What this adapter is not
//!
//! It is not the browser's only view of a session. The durable
//! transcript is still read from the log and the live projection is
//! still the follow stream's own; AG-UI is the *protocol-shaped*
//! door beside them, for a client that speaks AG-UI.

pub mod event;
pub mod projection;

pub use event::AguiEvent;
pub use projection::AguiProjection;
