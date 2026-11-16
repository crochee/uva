//! Test suite for the prompt assembly, harness side.
//!
//! The manifest's own renderers are tested in `uva-agent` beside the
//! builder. What stays here is the half only the loop can drive: a
//! plugin's `Host` registrations reaching a run's manifest
//! ([`sections`]) and the runtime-context snapshot seam ([`snapshot`]).
//! Cross-cutting fixtures live in [`support`].
//!
//! | Submodule          | What it covers                                            |
//! |--------------------|-----------------------------------------------------------|
//! | [`sections`]       | Plugin prompt sections: host registrations, registration order, placement, empty title |
//! | [`snapshot`]       | The `RuntimeContext` snapshot seam: supersession frame, clock injection |
//! | [`support`]        | Shared fixtures (`descriptor_with_instructions`, `peer_descriptor`, `fixed_runtime_context`) |

mod sections;
mod snapshot;
mod support;
