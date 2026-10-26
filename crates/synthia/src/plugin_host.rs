//! Facade re-export for [`synthia_plugin_host`].
//!
//! Enabled by the `plugin-host` facade feature. It turns a plugin
//! *process* into [`Host`](crate::harness::Host) registrations, so the
//! agent loop needs no knowledge of it; the wire vocabulary is the
//! sibling [`plugin_protocol`](crate::plugin_protocol) module.

pub use synthia_plugin_host::*;
