//! Stop-reason helpers shared by every provider adapter and
//! the orchestrator. Provider-agnostic: matches on whatever shape
//! the upstream wire spelled ("length", "max_tokens", or any
//! future equivalent) without leaking vendor-specific strings
//! into the loop.

/// True when a provider stop reason means the assistant output
/// was cut off by the output token limit, so tool-call
/// arguments in that turn may be silently truncated.
///
/// Providers spell the reason differently: OpenAI-compatible
/// APIs report `"length"`, Anthropic reports `"max_tokens"`.
/// Matching is case-insensitive so gateway normalizations do
/// not slip past the guard. Everything else — `"end_turn"`,
/// `"tool_use"`, `None`, … — is a normal stop.
pub fn is_length_stop_reason(stop_reason: &Option<String>) -> bool {
    stop_reason.as_deref().is_some_and(|reason| {
        reason.eq_ignore_ascii_case("length")
            || reason.eq_ignore_ascii_case("max_tokens")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_provider_token_limit_reasons() {
        assert!(is_length_stop_reason(&Some("length".to_string())));
        assert!(is_length_stop_reason(&Some("max_tokens".to_string())));
        assert!(is_length_stop_reason(&Some("LENGTH".to_string())));
        assert!(is_length_stop_reason(&Some("MAX_TOKENS".to_string())));
    }

    #[test]
    fn rejects_non_length_reasons() {
        assert!(!is_length_stop_reason(&None));
        assert!(!is_length_stop_reason(&Some("end_turn".to_string())));
        assert!(!is_length_stop_reason(&Some("tool_use".to_string())));
        assert!(!is_length_stop_reason(&Some("stop_sequence".to_string())));
    }
}
