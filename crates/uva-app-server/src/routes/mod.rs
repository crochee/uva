pub mod agents;
pub mod attachment;
/// REST + SSE surface for chat. Mounted at `/api/v1/chat` by
/// `server/router.rs::create_router`, where the chat routes are
/// declared inside the `api_routes` block.
///
/// Session listing and detail live on the management surface
/// (`routes::sessions` mounted at `/api/v1/sessions`) so there
/// is exactly one canonical SessionSummary / SessionDetail
/// shape on the wire.
pub mod chat;
pub mod evals;
pub mod follow;
pub mod health;
pub(crate) mod list;
pub(crate) mod messages;
pub mod schedules;
pub mod search;
pub mod session_search;
pub mod sessions;
pub mod skills;
pub mod subagents;
pub mod tasks;
pub mod tool;
pub mod ui;

#[cfg(test)]
mod tests;

pub use agents::{create_agent, delete_agent, get_agent, list_agents};
pub use tool::{get_tool, list_tools, register_tool, unregister_tool};
