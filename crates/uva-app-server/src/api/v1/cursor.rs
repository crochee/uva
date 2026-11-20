// Allow `result_large_err` for the whole file: P1b added 4 hidden
// fields to every struct-form variant (frames, backtrace, source,
// and the synthetic source chain), so every `Result<_, Error>` is
// at least 128 bytes. Boxing the error would force every call site
// to `.map_err(|e| *e)` (or accept the allocation), and the existing
// API has no `Box<Error>` in the public surface. Accept the size
// cost; revisit if profiling shows it matters.
#![allow(clippy::result_large_err)]

//! Opaque cursor encoding for keyset pagination.
//!
//! Cursor = base64 (URL-safe, no-pad) encoding of the last
//! resource ID on the current page. Clients treat it as opaque;
//! servers decode it to resume via `WHERE id > last_id`.
//!
//! These helpers return [`uva::core::Error`] (the
//! transport-agnostic core error). HTTP transports can then
//! map specific variants (`Error::InvalidItem`,
//! `Error::Validation`) to the appropriate wire envelope
//! without the cursor helpers having to know the transport.

use uva::core::{Error, cursor};

use super::page_query::{DEFAULT_LIMIT, MAX_LIMIT};

/// Encode a resource ID into an opaque base64 cursor.
///
/// ```
/// use uva_app_server::api::encode_cursor;
/// assert_eq!(encode_cursor("task_abc"), "dGFza19hYmM");
/// ```
///
/// Thin delegation over [`uva::core::cursor::encode`], the
/// canonical cursor codec. The `uva-core` registry's
/// in-memory pagination uses the same encoding, so a cursor
/// emitted by one round-trips on the other.
pub fn encode_cursor(id: &str) -> String {
    cursor::encode(id)
}

/// Decode an opaque base64 cursor back into a resource ID.
///
/// Returns [`Error`] with the [`Error::InvalidItem`] variant if
/// the input is not valid URL-safe base64 or is not valid
/// UTF-8 after decoding. Thin delegation over
/// [`uva::core::cursor::decode`].
pub fn decode_cursor(cursor: &str) -> Result<String, Error> {
    uva::core::cursor::decode(cursor)
}

/// Normalize a raw `limit` parameter.
///
/// - `None` → `DEFAULT_LIMIT` (20).
/// - `Some(0)` → `Err(Error)` with the
///   [`Error::Validation`] variant.
/// - `Some(n)` where `n > MAX_LIMIT` → `MAX_LIMIT` (100),
///   silently truncated.
/// - Otherwise → `n`.
pub fn normalize_limit(limit: Option<u64>) -> Result<u64, Error> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(0) => Err(Error::validation("limit must be greater than 0")),
        Some(n) if n > MAX_LIMIT => Ok(MAX_LIMIT),
        Some(n) => Ok(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- encode_cursor / decode_cursor ---

    #[test]
    fn encode_cursor_matches_spec_example() {
        // Spec: cursor of "task_abc" = "dGFza19hYmM"
        // (base64 URL-safe no-pad).
        assert_eq!(encode_cursor("task_abc"), "dGFza19hYmM");
    }

    #[test]
    fn decode_cursor_round_trip() {
        let id = "task_abc";
        let encoded = encode_cursor(id);
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn decode_cursor_handles_unicode_ids() {
        let id = "task_αβγ";
        let encoded = encode_cursor(id);
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn decode_cursor_rejects_invalid_base64() {
        assert!(decode_cursor("not-base64!!!").is_err());
    }

    #[test]
    fn decode_cursor_rejects_non_utf8_payload() {
        // 0xff is not valid UTF-8 (standalone). We need a
        // valid base64 payload that decodes to non-UTF-8
        // bytes, so we route through the core's `encode`
        // (which takes a string) — but encode rejects non-UTF-8
        // by construction. So we use `base64` directly to
        // build the bad payload.
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let bad = URL_SAFE_NO_PAD.encode([0xffu8]);
        assert!(decode_cursor(&bad).is_err());
    }

    // --- normalize_limit ---

    #[test]
    fn normalize_limit_none_yields_default() {
        assert_eq!(normalize_limit(None).unwrap(), DEFAULT_LIMIT);
    }

    #[test]
    fn normalize_limit_zero_is_error() {
        assert!(normalize_limit(Some(0)).is_err());
    }

    #[test]
    fn normalize_limit_within_range_is_passthrough() {
        assert_eq!(normalize_limit(Some(1)).unwrap(), 1);
        assert_eq!(normalize_limit(Some(20)).unwrap(), 20);
        assert_eq!(normalize_limit(Some(100)).unwrap(), 100);
    }

    #[test]
    fn normalize_limit_above_max_is_truncated_to_max() {
        assert_eq!(normalize_limit(Some(101)).unwrap(), MAX_LIMIT);
        assert_eq!(normalize_limit(Some(1000)).unwrap(), MAX_LIMIT);
    }
}
