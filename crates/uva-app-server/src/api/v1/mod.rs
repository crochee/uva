//! RESTful v1 API shared types: `List<T>`, `PageQuery`,
//! `SessionPageQuery`, cursor encode/decode, and request validation
//! helpers.
//!
//! These types are HTTP-wire concerns and live alongside the v1
//! error adapter (`error::ApiError`, wrapping `uva::core::Error`)
//! in `uva-app-server::api`. Keeping the v1 surface in one place
//! makes the wire contract easy to audit.
//!
//! # Module Layout
//!
//! - [`list`]: [`list::List<T>`] — generic list envelope returned
//!   by every list endpoint.
//! - [`page_query`]: [`page_query::PageQuery`] and the
//!   resource-specific [`page_query::SessionPageQuery`] query
//!   parameter structs, plus the `DEFAULT_LIMIT` / `MAX_LIMIT`
//!   constants.
//! - [`cursor`]: opaque base64 (URL-safe, no-pad) cursor encoding
//!   for keyset pagination. Encodes the last resource ID of a
//!   page so the next page can resume via `WHERE id > last_id`.
//! - [`validation`]: request-parameter validation helpers —
//!   resource-name regex, sort whitelist enforcement, and API
//!   key masking.
//! - [`profile`]: [`profile::ProfileRow`] and
//!   [`profile::ProfileChange`] — the profile seam's wire shapes,
//!   with the catalog of the methods that answer with them.
//! - [`session_view`]: the durable-row and legacy-attachment views
//!   the three untyped `Vec<Value>` response fields carry.

pub mod cursor;
pub mod list;
pub mod page_query;
pub mod profile;
pub mod session_view;
pub mod validation;

pub use cursor::{
    ResolvedPage,
    decode_cursor,
    encode_cursor,
    next_cursor,
    resolve_page,
};
pub use list::List;
pub use page_query::{DEFAULT_LIMIT, MAX_LIMIT, PageQuery, SessionPageQuery};
pub use profile::{
    ProfileChange,
    ProfileChangeState,
    ProfileRow,
    ProfileSegment,
};
pub use validation::{api_key_mask, validate_resource_name, validate_sort};
