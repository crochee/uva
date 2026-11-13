//! Agent definitions as **files**.
//!
//! An agent is a directory:
//!
//! ```text
//! agents/<name>/
//!   agent.md      — identity, persona and instructions
//!   config.json   — optional structured fields
//! ```
//!
//! The definitions live in the two-layer `~/.agents` layout that
//! [`crate::layout`] owns: the project's own `<root>/.agents/agents/`,
//! the plugin bundles beside it, the global `~/.agents/agents/`, and
//! the global bundles. The first source that offers a name wins.
//!
//! ## Why this lives beside [`crate::agent`]
//!
//! Three things describe one agent, and they cannot drift apart:
//!
//! - [`crate::agent::AgentDescriptor`] — the identity the rest of the
//!   framework names an agent by;
//! - [`crate::layout`] — **where** definitions live and which source
//!   wins;
//! - this module — **how** a definition is spelled on disk.
//!
//! Splitting the third away from the first two put the file format in
//! a crate whose only consumer was the facade, and made "an agent" a
//! concept split across two crates. Keeping them together means the
//! format, the identity it produces, and the layout it is discovered
//! through are edited in one place.
//!
//! ## Surfaces
//!
//! - [`crate::agent_def::format`] — the file format: [`crate::agent_def::format::AgentFrontmatter`]
//!   (the `agent.md` frontmatter), [`crate::agent_def::format::AgentConfig`] (the
//!   `config.json` fields) and [`crate::agent_def::format::AgentDefinition`] (the two
//!   composed).
//! - [`crate::agent_def::discovery`] — the walk across the layout's sources.
//!
//! The two files never share a key, so the composition has no
//! precedence to decide: the frontmatter is the identity a human
//! writes, the JSON is the configuration a tool writes.

/// The walk across the layout's sources.
pub mod discovery;
/// The file format: the `agent.md` frontmatter, the `config.json`
/// fields, and the two composed.
pub mod format;

pub use discovery::{DiscoveredAgent, Location, discover};
pub use format::{
    AGENT_CONFIG_FILE,
    AGENT_FILE,
    AgentConfig,
    AgentDefinition,
    AgentFileError,
    AgentFrontmatter,
    DEFAULT_KIND,
    DEFAULT_VERSION,
    write,
};

#[cfg(test)]
mod tests;
