//! [`elysper_agents`] — agent definitions as files.
//!
//! An agent is a directory: `agent.md` (YAML frontmatter +
//! instructions) beside an optional `config.json` (the structured
//! fields). The definitions live in the two-layer `~/.agents` layout
//! [`elysper_core::layout`] owns, and the first source that offers a
//! name wins.

pub use elysper_agents::*;
