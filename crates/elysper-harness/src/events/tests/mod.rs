//! Test suite for the `events` types. Each concern lives in its
//! own focused submodule; the shared fixtures live in
//! [`helpers`].
//!
//! Concerns:
//! - [`wire_tags`] — serde `type` tags + `kind()` labels.
//! - [`round_trip`] — JSON round-trips + pairwise distinctness.
//! - [`agent_meta`] — `AgentMeta` constructors + equality + the
//!   documented `SystemEvent` / `AgentEvent` constructor shapes.
//! - [`events_durability`] — `AgentEvent::is_durable` matrix.

mod agent_meta;
mod events_durability;
mod helpers;
mod round_trip;
mod wire_tags;
