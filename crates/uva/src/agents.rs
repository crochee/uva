//! [`uva_core::agent_def`] — agent definitions as files.
//!
//! An agent is a directory: `agent.md` (YAML frontmatter +
//! instructions) beside an optional `config.json` (the structured
//! fields). The definitions live in the two-layer `~/.agents` layout
//! [`uva_core::layout`] owns, and the first source that offers a
//! name wins.
//!
//! The format lives in `uva-core` beside the identity it
//! produces ([`uva_core::AgentDescriptor`]) and the layout it is
//! discovered through, so "an agent" is one concept in one place.

pub use uva_core::agent_def::*;
