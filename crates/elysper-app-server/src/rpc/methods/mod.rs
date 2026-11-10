//! The method table.
//!
//! One entry per method: the name on the wire, the request struct,
//! and the handler. The transport knows no method by name — the
//! table is the whole contract.
//!
//! # Naming
//!
//! `domain/verb`, the slash style LSP, MCP and gRPC use. The domain
//! is the surface the method belongs to (`chat`, `sessions`,
//! `agents`, …); the verb is what it does.
//!
//! # Parameters
//!
//! Every method takes one `params` object. The fields are the path
//! parameters the REST surface carried (`session_id`, `id`, `name`)
//! plus the request body, flattened in beside them — so
//! `chat/message/send` takes
//! `{"session_id": "s1", "message": "hello"}`.
//!
//! # Results
//!
//! A result is the payload the REST surface returned, unchanged: the
//! same serde structures, the same camelCase. A list keeps the
//! `List<T>` envelope (`data`, `next_cursor`, `has_more`, `total`).
pub mod agents;
pub mod automation;
pub mod chat;
pub mod misc;
pub mod profile;
pub mod sessions;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{RpcError, RpcRouter};

/// Parse the `params` member into a request type and run the type's
/// own schema validation.
pub fn parse<T>(raw: Value) -> Result<T, RpcError>
where
    T: DeserializeOwned + validator::Validate,
{
    super::params(raw)
}
/// Parse the `params` member into a request type without schema
/// validation.
pub fn parse_raw<T>(raw: Value) -> Result<T, RpcError>
where
    T: DeserializeOwned,
{
    super::params_raw(raw)
}
/// Register every method the process answers.
pub fn register_all(router: &mut RpcRouter) {
    agents::register(router);
    automation::register(router);
    chat::register(router);
    misc::register(router);
    profile::register(router);
    sessions::register(router);
}
