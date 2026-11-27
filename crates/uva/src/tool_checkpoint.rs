//! [`uva_tool_plugins::checkpoint`] — the explore-then-cut-back pair:
//! `checkpoint` marks the boundary a later `rewind` cuts back to,
//! keeping the report in place of the exploration. The tools hold a
//! `RewindStore` seam the host installs, so where the state lives and
//! how the cut is applied is the host's choice.

pub use uva_tool_plugins::checkpoint::*;
