//! Critique types + the [`CriticProvider`] trait.

use std::sync::Arc;

use elysper_provider::ModelProvider;
use serde::{Deserialize, Serialize};

/// Severity of a critique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// No drift; no hint needed.
    Info,
    /// Drift detected; the model may need a nudge.
    Warn,
    /// Severe drift; the model is going off the rails.
    Error,
}

/// A critique returned by the critic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Critique {
    pub severity: Severity,
    pub reason: String,
    /// Optional hint to inject into the next prompt.
    pub suggested_hint: Option<String>,
}

impl Critique {
    pub fn info(reason: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            reason: reason.into(),
            suggested_hint: None,
        }
    }

    pub fn warn(reason: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            severity: Severity::Warn,
            reason: reason.into(),
            suggested_hint: hint,
        }
    }

    pub fn error(reason: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            severity: Severity::Error,
            reason: reason.into(),
            suggested_hint: hint,
        }
    }

    /// True when the critique warrants a hint injection.
    #[must_use]
    pub fn warrants_hint(&self) -> bool {
        !matches!(self.severity, Severity::Info)
            && self.suggested_hint.is_some()
    }
}

/// Pluggable critic provider. The default impl is provided by
/// [`crate::reflection::hook::ReflectionHook`], which talks to a
/// `ModelProvider`; consumers can override.
#[async_trait::async_trait]
pub trait CriticProvider: Send + Sync {
    async fn critique(&self, history: &[HistoryEntry]) -> Critique;
}

/// One entry in the bounded history snapshot the critic sees.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result_preview: String,
}

/// Default model-driven implementation of [`CriticProvider`] —
/// forwards a structured prompt to the model and parses the reply.
pub struct ModelCritic {
    pub provider: Arc<dyn ModelProvider>,
}

impl ModelCritic {
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl CriticProvider for ModelCritic {
    async fn critique(&self, history: &[HistoryEntry]) -> Critique {
        use elysper_provider::{
            CompletionRequest,
            Content,
            ContentPart,
            Message,
            Role,
            TextContent,
        };
        let mut text = String::from(
            "You are a critic. Reply with one of:\n  INFO <reason>\n  WARN <reason> | <hint>\n  ERROR <reason> | <hint>\n\nRecent tool calls:\n",
        );
        for entry in history {
            text.push_str(&format!(
                "- {}: {} | result: {}\n",
                entry.tool_name,
                entry.arguments,
                truncate(&entry.result_preview, 200)
            ));
        }
        let request = CompletionRequest {
            model: self.provider.model_config().name.clone(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Content::Single(ContentPart::Text(TextContent {
                    text,
                    cache_control: None,
                })),
                tool_call_id: None,
                name: None,
                tool_result_cleared_at: None,
            }]),
            tools: Arc::new(Vec::new()),
            ..CompletionRequest::default()
        };
        let response = match self.provider.complete(request).await {
            Ok(r) => r,
            Err(_) => return Critique::info("critic model call failed"),
        };
        let text = match &response.content {
            Content::Single(ContentPart::Text(t)) => t.text.clone(),
            Content::Multi(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            Content::Single(_) => String::new(),
        };
        parse_critique(&text)
    }
}

/// Parse the critic's reply into a [`Critique`].
#[must_use]
pub fn parse_critique(text: &str) -> Critique {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("INFO") {
        return Critique::info(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix("WARN") {
        let (reason, hint) = split_reason_hint(rest.trim());
        return Critique::warn(reason, hint);
    }
    if let Some(rest) = trimmed.strip_prefix("ERROR") {
        let (reason, hint) = split_reason_hint(rest.trim());
        return Critique::error(reason, hint);
    }
    // Fallback: treat unknown replies as info.
    Critique::info(trimmed.to_string())
}

fn split_reason_hint(s: &str) -> (String, Option<String>) {
    if let Some((reason, hint)) = s.split_once('|') {
        (reason.trim().to_string(), Some(hint.trim().to_string()))
    } else {
        (s.to_string(), None)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
