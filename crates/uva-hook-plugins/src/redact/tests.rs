//! Tests for the `redact` result transformer.

use std::sync::Arc;

use uva_context::AgentState;
use uva_core::{AtomicCancelToken, CancelToken};
use uva_hook::{HookContext, ResultTransformer as _};
use uva_tool::ToolOutput;

use super::{RedactConfig, RedactTransformer, RedactionRule};

fn body(text: &str) -> ToolOutput {
    ToolOutput::text(text)
}

fn context<'a>(
    state: &'a AgentState,
    cancel: &'a Arc<dyn CancelToken>,
) -> HookContext<'a> {
    HookContext { state, cancel }
}

#[tokio::test]
async fn default_rules_mask_known_credential_shapes() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let mut output = body(
        "openai sk-abcdefgh12345678 \
         bearer Bearer eyJhbGciOiJIUzI1NiJ9.payload \
         github ghp_1234567890abcdef \
         aws AKIAABCDEFGHIJKLMNOP",
    );

    RedactTransformer::default()
        .transform(&mut output, &ctx)
        .await
        .expect("redact");

    let text = output.content[0].text().expect("text part");
    assert!(text.contains("[REDACTED:openai-key]"), "{text}");
    assert!(text.contains("[REDACTED:bearer]"), "{text}");
    assert!(text.contains("[REDACTED:github-token]"), "{text}");
    assert!(text.contains("[REDACTED:aws-access-key]"), "{text}");
    assert!(!text.contains("sk-abcdefgh"), "{text}");
    assert!(!text.contains("ghp_1234567890"), "{text}");
    assert!(!text.contains("AKIAABCDEFGHIJKLMNOP"), "{text}");
}

#[tokio::test]
async fn a_short_tail_or_a_mid_token_prefix_is_left_alone() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let mut output = body("sk-short and task-sk-abcdefgh1234");

    RedactTransformer::default()
        .transform(&mut output, &ctx)
        .await
        .expect("redact");

    let text = output.content[0].text().expect("text part");
    assert_eq!(text, "sk-short and task-sk-abcdefgh1234");
}

#[tokio::test]
async fn a_separator_before_the_prefix_is_a_boundary() {
    // `.env` exports, `env`, `docker inspect`, CI logs and config files
    // all write `KEY=value`; the boundary test must not read `=` as
    // part of the word the prefix sits in.
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let mut output = body(
        "OPENAI_API_KEY=sk-abcdefgh12345678\n\
         GITHUB_TOKEN=ghp_1234567890abcdef\n\
         AWS_ACCESS_KEY_ID=AKIAABCDEFGHIJKLMNOP\n\
         auth=Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln",
    );

    RedactTransformer::default()
        .transform(&mut output, &ctx)
        .await
        .expect("redact");

    let text = output.content[0].text().expect("text part");
    assert!(
        text.contains("OPENAI_API_KEY=[REDACTED:openai-key]"),
        "{text}"
    );
    assert!(
        text.contains("GITHUB_TOKEN=[REDACTED:github-token]"),
        "{text}"
    );
    assert!(
        text.contains("AWS_ACCESS_KEY_ID=[REDACTED:aws-access-key]"),
        "{text}"
    );
    assert!(text.contains("auth=[REDACTED:bearer]"), "{text}");
    assert!(!text.contains("sk-abcdefgh"), "{text}");
    assert!(!text.contains("ghp_1234567890"), "{text}");
    assert!(!text.contains("AKIAABCDEFGHIJKLMNOP"), "{text}");
    assert!(!text.contains("eyJhbGciOiJIUzI1NiJ9"), "{text}");
}

#[tokio::test]
async fn a_custom_rule_set_replaces_the_defaults() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let mut output =
        body("internal KEY_abcdefgh12345678 and public sk-abcdefgh1234");

    let config = RedactConfig::new(vec![
        RedactionRule::new("internal-key", "KEY_").with_min_tail(4),
    ]);
    RedactTransformer::new(config)
        .transform(&mut output, &ctx)
        .await
        .expect("redact");

    let text = output.content[0].text().expect("text part");
    assert!(text.contains("[REDACTED:internal-key]"), "{text}");
    assert!(
        text.contains("sk-abcdefgh1234"),
        "the default openai rule is not part of a custom set: {text}"
    );
}

#[tokio::test]
async fn metadata_strings_are_redacted_recursively() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let mut output = body("clean");
    output.metadata.insert(
        "nested".to_string(),
        serde_json::json!({
            "key": "sk-abcdefgh12345678",
            "list": ["ok", "Bearer eyJhbGciOiJIUzI1NiJ9.abc"],
            "count": 3,
        }),
    );

    RedactTransformer::default()
        .transform(&mut output, &ctx)
        .await
        .expect("redact");

    let metadata = output.metadata["nested"].to_string();
    assert!(metadata.contains("[REDACTED:openai-key]"), "{metadata}");
    assert!(metadata.contains("[REDACTED:bearer]"), "{metadata}");
    assert!(!metadata.contains("sk-abcdefgh"), "{metadata}");
}

#[tokio::test]
async fn redaction_is_idempotent() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let mut output = body("sk-abcdefgh12345678");
    let transformer = RedactTransformer::default();

    transformer
        .transform(&mut output, &ctx)
        .await
        .expect("first");
    let once = output.content[0].text().expect("text part").to_string();
    transformer
        .transform(&mut output, &ctx)
        .await
        .expect("second");

    assert_eq!(output.content[0].text(), Some(once.as_str()));
}

#[test]
fn the_transformer_names_itself_for_the_provenance_row() {
    assert_eq!(RedactTransformer::default().name(), "redact");
}

#[test]
fn default_rules_are_documented_and_ordered() {
    let rules = RedactionRule::defaults();
    let names: Vec<&str> = rules.iter().map(RedactionRule::name).collect();
    assert_eq!(
        names,
        [
            "bearer",
            "openai-key",
            "github-token",
            "github-pat",
            "aws-access-key",
            "aws-session-key",
            "slack-token",
        ]
    );
}
