//! # elysper-agents
//!
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
//! [`elysper_core::layout`] owns: the project's own
//! `<root>/.agents/agents/`, the plugin bundles beside it, the
//! global `~/.agents/agents/`, and the global bundles. The first
//! source that offers a name wins.
//!
//! ## Surfaces
//!
//! - [`mod@format`] — the file format: [`format::AgentFrontmatter`]
//!   (the `agent.md` frontmatter), [`format::AgentConfig`] (the
//!   `config.json` fields) and [`format::AgentDefinition`] (the
//!   two composed).
//! - [`discovery`] — the walk across the layout's sources.
//!
//! The two files never share a key, so the composition has no
//! precedence to decide: the frontmatter is the identity a human
//! writes, the JSON is the configuration a tool writes.

pub mod discovery;
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
