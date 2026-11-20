//! The one Anthropic usage mapping.
//!
//! The non-streaming `parse_response` and the streaming decoder both
//! call [`token_usage`], so the same conversation reports the same
//! totals in both modes — bucket for bucket.

use crate::types::TokenUsage;

/// Map an Anthropic usage report onto the canonical [`TokenUsage`]
/// buckets.
///
/// - `input_tokens` becomes `prompt_tokens`
/// - `output_tokens` becomes `completion_tokens`
/// - their sum becomes `total_tokens`
/// - `cache_read_input_tokens` feeds BOTH `cached_prompt_tokens` and
///   `cache_read_tokens` (one provider number in two vocabularies)
/// - `cache_creation_input_tokens` feeds `cache_write_tokens`
/// - there is no reasoning bucket: Anthropic bills thinking as output
///   and does not report a separate count.
///
/// A provider-reported zero stays a zero-valued bucket (`Some(0)`),
/// never absent: `Some(0)` means "reported a cache miss" while `None`
/// means "does not report cache metrics", and readers distinguish the
/// two.
#[must_use]
pub(crate) fn token_usage(
    input_tokens: usize,
    output_tokens: usize,
    cache_read_input_tokens: Option<usize>,
    cache_creation_input_tokens: Option<usize>,
) -> TokenUsage {
    TokenUsage {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
        total_tokens: input_tokens + output_tokens,
        cached_prompt_tokens: cache_read_input_tokens,
        cache_read_tokens: cache_read_input_tokens,
        cache_write_tokens: cache_creation_input_tokens,
        reasoning_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five buckets map one-to-one, including the cache-read value
    /// landing in both the cached-prompt and cache-read slots.
    #[test]
    fn maps_every_bucket() {
        let usage = token_usage(100, 50, Some(80), Some(20));
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.cached_prompt_tokens, Some(80));
        assert_eq!(usage.cache_read_tokens, Some(80));
        assert_eq!(usage.cache_write_tokens, Some(20));
        assert_eq!(usage.reasoning_tokens, None);
    }

    /// A reported zero is a value: it must stay `Some(0)` rather than
    /// collapsing into "not reported".
    #[test]
    fn keeps_a_reported_zero() {
        let usage = token_usage(0, 0, Some(0), Some(0));
        assert_eq!(usage.cached_prompt_tokens, Some(0));
        assert_eq!(usage.cache_read_tokens, Some(0));
        assert_eq!(usage.cache_write_tokens, Some(0));
    }

    /// Absent cache metrics stay absent — the provider said nothing,
    /// so neither do the buckets.
    #[test]
    fn leaves_unreported_cache_buckets_absent() {
        let usage = token_usage(10, 5, None, None);
        assert_eq!(usage.cached_prompt_tokens, None);
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(usage.cache_write_tokens, None);
        assert_eq!(usage.total_tokens, 15);
    }
}
