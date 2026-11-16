//! Test suite for the prompt manifest.
//!
//! Split into focused submodules by concern, so the builder in the
//! parent module stays a single, small file. Each submodule tests
//! exactly one of the renderers, and the cross-cutting fixtures live in
//! [`support`]. The harness-side half — a `Host` registration reaching a
//! run's manifest, and the runtime-context snapshot — stays with the
//! loop that owns it.
//!
//! | Submodule          | What it covers                                            |
//! |--------------------|-----------------------------------------------------------|
//! | [`assemble`]       | The public `assemble` contract — order, purity, byte-stability, no-runtime-facts |
//! | [`identity`]       | The `<identity>` renderer: display_name fallback, empty fields |
//! | [`skills_agents`]  | The `<available_skills>` / `<available_agents>` renderers and their absence rules |
//! | [`support`]        | Shared fixtures (`descriptor_with_instructions`, `peer_descriptor`) |

mod assemble;
mod identity;
mod skills_agents;
mod support;
