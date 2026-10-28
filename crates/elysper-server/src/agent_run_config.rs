//! Server-side run configuration types for the HTTP server's
//! [`SessionController`](crate::session::controller::SessionController).
//!
//! These types describe how *this server* drives an agent run, not what
//! the agent loop itself needs, so they live here rather than in
//! `elysper-harness`. The harness owns the seams they feed (the
//! `ReActAgent` setters, the `ContextManager` and the `Compactor`).

pub use super::agent_run_config_types::{AgentRunConfig, RunOverrides};
