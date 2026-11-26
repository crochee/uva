//! [`uva_tool_plugins::goal`] — the `goal` tool plugin: the durable
//! objective. The tool writes the goal's state through the host's
//! store; the session controller reads it to decide whether a turn
//! that ended should start another.

pub use uva_tool_plugins::goal::*;
