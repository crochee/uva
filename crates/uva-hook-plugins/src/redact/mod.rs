//! Redact secrets from a tool result before the loop commits it.
//!
//! [`RedactTransformer`] is a [`ResultTransformer`]: the loop calls it
//! after the tool returned and *before* it writes the `tool_result`
//! row, so the row and the model both hold the redacted text. The loop
//! then appends one durable `tool_result_transformed` row naming this
//! transformer (`redact`) with a digest of the output before and after
//! the rewrite — provenance, not reconciliation.
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use uva_hook::{HookRegistry, MountPoint, RegistrationOrigin};
//! use uva_hook_plugins::redact::RedactTransformer;
//!
//! let registry = HookRegistry::new();
//! let _registration = registry
//!     .register_result_transformer_at(
//!         MountPoint::ToolPost,
//!         Arc::new(RedactTransformer::default()),
//!         RegistrationOrigin::InProcess,
//!     )
//!     .expect("a result transformer belongs on `tool/post-execute`");
//! ```
//!
//! ## Default rules
//!
//! A [`RedactionRule`] matches a literal prefix and redacts the token
//! that follows it, when that token is at least `min_tail` characters
//! long. Tokens consist of ASCII alphanumerics plus `- _ . + / =`.
//! The default rule set is documented and replaceable:
//!
//! | rule | prefix | min tail |
//! |---|---|---|
//! | `bearer` | `Bearer ` | 8 |
//! | `openai-key` | `sk-` | 8 |
//! | `github-token` | `ghp_` | 8 |
//! | `github-pat` | `github_pat_` | 8 |
//! | `aws-access-key` | `AKIA` | 16 |
//! | `aws-session-key` | `ASIA` | 16 |
//! | `slack-token` | `xoxb-` | 8 |
//!
//! A prefix is only matched at a word boundary: `task-sk-12345678`
//! is not an `sk-` key, while a separator in front of it —
//! `OPENAI_API_KEY=sk-…`, `auth=Bearer …`, a quote or whitespace —
//! makes it one. The boundary class is word characters only (ASCII
//! alphanumerics, `-`, `_`); the tail scan keeps the wider token class
//! (`+ . / =`) so base64 padding and wrapped tokens stay inside the
//! redacted span. Pass an explicit [`RedactConfig`] to replace the
//! defaults or add a rule for a secret shape of your own.
//!
//! Redaction applies to every text part of the output and to every
//! string value in [`ToolOutput::metadata`](uva_tool::ToolOutput).

use serde_json::Value;
use uva_hook::{HookContext, ResultTransformer};
use uva_tool::ToolOutput;

#[cfg(test)]
mod tests;

/// The marker `redact` leaves in place of a matched secret.
///
/// The marker names the rule, so a reader of the model-visible result
/// can tell a redacted credential from a genuinely absent value.
fn marker(rule: &str) -> String {
    format!("[REDACTED:{rule}]")
}

/// `true` for characters a secret token may contain.
///
/// The tail scan uses this set, so it must keep `=`, `.`, `/` and `+`:
/// base64 padding and wrapped tokens are part of the secret.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=')
}

/// `true` for characters that make the text before a prefix part of a
/// word.
///
/// Deliberately narrower than [`is_token_char`]: the boundary test asks
/// "does this prefix start a token?", not "may the surrounding token
/// contain this character?". Separators (`=`, `.`, `/`, `+`, `:`, `"`)
/// therefore end the preceding word — `OPENAI_API_KEY=sk-…` is a key
/// assignment, not the tail of a word.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_')
}

/// One literal-prefix secret rule.
///
/// A rule is intentionally not a regex: a prefix plus a minimum tail
/// length covers the shipped credential families and keeps the
/// matching cost linear in the text length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionRule {
    name: String,
    prefix: String,
    min_tail: usize,
}

impl RedactionRule {
    /// Build a rule that redacts the token following `prefix`.
    ///
    /// `name` labels the marker (`[REDACTED:<name>]`). The default
    /// minimum tail length is 8 characters; shorten or lengthen it
    /// with [`RedactionRule::with_min_tail`].
    #[must_use]
    pub fn new(name: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            prefix: prefix.into(),
            min_tail: 8,
        }
    }

    /// Set the minimum number of token characters after the prefix.
    ///
    /// A shorter tail is left untouched, so a rule does not fire on
    /// prose that merely mentions the prefix.
    #[must_use]
    pub fn with_min_tail(mut self, min_tail: usize) -> Self {
        self.min_tail = min_tail;
        self
    }

    /// The rule name carried in the marker.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The literal prefix the rule looks for.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The minimum token length the rule accepts.
    #[must_use]
    pub fn min_tail(&self) -> usize {
        self.min_tail
    }

    /// The defaults documented on this module.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        vec![
            Self::new("bearer", "Bearer "),
            Self::new("openai-key", "sk-"),
            Self::new("github-token", "ghp_"),
            Self::new("github-pat", "github_pat_"),
            Self::new("aws-access-key", "AKIA").with_min_tail(16),
            Self::new("aws-session-key", "ASIA").with_min_tail(16),
            Self::new("slack-token", "xoxb-"),
        ]
    }

    /// Rewrite every match in `text`; return the rewritten text.
    fn apply(&self, text: &str) -> String {
        if self.prefix.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        while let Some(rel) = text[cursor..].find(&self.prefix) {
            let start = cursor + rel;
            let after = start + self.prefix.len();
            // The boundary test asks whether the prefix starts a word,
            // so it uses the word set — not the token set the tail scan
            // needs. Sharing the token set made every `KEY=value`
            // assignment look mid-token (`=` counts as a token char)
            // and skipped the rule entirely.
            let boundary = text[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_char(c));
            let tail = &text[after..];
            let tail_len =
                tail.chars().take_while(|c| is_token_char(*c)).count();
            if boundary && tail_len >= self.min_tail {
                out.push_str(&text[cursor..start]);
                out.push_str(&marker(&self.name));
                cursor = tail
                    .char_indices()
                    .nth(tail_len)
                    .map(|(i, _)| after + i)
                    .unwrap_or(text.len());
            } else {
                out.push_str(&text[cursor..after]);
                cursor = after;
            }
        }
        out.push_str(&text[cursor..]);
        out
    }
}

/// The rule set a [`RedactTransformer`] applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactConfig {
    rules: Vec<RedactionRule>,
}

impl RedactConfig {
    /// Build a configuration from an explicit rule list.
    #[must_use]
    pub fn new(rules: Vec<RedactionRule>) -> Self {
        Self { rules }
    }

    /// Append one rule.
    #[must_use]
    pub fn with_rule(mut self, rule: RedactionRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// The rules, in application order.
    #[must_use]
    pub fn rules(&self) -> &[RedactionRule] {
        &self.rules
    }

    /// Rewrite `text` through every rule.
    fn redact_text(&self, text: &str) -> String {
        let mut current = text.to_string();
        for rule in &self.rules {
            current = rule.apply(&current);
        }
        current
    }
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self::new(RedactionRule::defaults())
    }
}

/// Rewrites a tool result so model-visible secrets are masked.
///
/// The transformer mutates the output in place. The loop notices the
/// changed aggregate and writes the provenance row; a result with no
/// match is left untouched and produces no row.
pub struct RedactTransformer {
    config: RedactConfig,
}

impl RedactTransformer {
    /// Build a transformer from an explicit configuration.
    #[must_use]
    pub fn new(config: RedactConfig) -> Self {
        Self { config }
    }

    /// The configuration this transformer applies.
    #[must_use]
    pub fn config(&self) -> &RedactConfig {
        &self.config
    }
}

impl Default for RedactTransformer {
    fn default() -> Self {
        Self::new(RedactConfig::default())
    }
}

/// Recursively redact string values in structured metadata.
fn redact_value(value: &mut Value, config: &RedactConfig) {
    match value {
        Value::String(text) => *text = config.redact_text(text),
        Value::Array(items) => {
            for item in items {
                redact_value(item, config);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                redact_value(value, config);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[async_trait::async_trait]
impl ResultTransformer for RedactTransformer {
    fn name(&self) -> &str {
        "redact"
    }

    async fn transform(
        &self,
        output: &mut ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        for part in &mut output.content {
            if let Some(text) = part.text_mut() {
                *text = self.config.redact_text(text);
            }
        }
        for value in output.metadata.values_mut() {
            redact_value(value, &self.config);
        }
        Ok(())
    }
}
