//! The request/response shapes the RPC methods share.
//!
//! Owns everything that touches the v1 wire contract:
//!
//! - [`v1`]: list response envelope, page query params, opaque
//!   cursor encoding, request-parameter validation helpers
//!   (resource-name regex, API-key masking).
//!   `uva::core::Error` — [`crate::rpc::RpcError`] (the
//!   boundary struct handlers return). The `From<uva::core::Error>
//!   for RpcError` impl owns the variant → machine-code mapping
//!   inline; the server defines no error variants of its own.
//! - [`contract`]: the catalog of the named types
//!   `uva-web/src/api/types.ts` is generated from, and the JSON
//!   Schema document that carries it.

pub mod contract;
pub mod operation;
pub mod v1;
pub use operation::OperationRequest;
pub use v1::{
    List,
    MAX_LIMIT,
    PageQuery,
    SessionPageQuery,
    api_key_mask,
    decode_cursor,
    encode_cursor,
    validate_resource_name,
};
