//! Provider-neutral cache policy injection.
//!
//! Aligned with opencode's `applyCachePolicy` from
//! `packages/llm/src/cache-policy.ts`. Marks the last tool / last user
//! message with a [`CacheControlMark`] so that provider
//! `transform_request` implementations can translate the mark to
//! provider-specific `cache_control` hints (e.g. Anthropic
//! `{"type":"ephemeral"}`).
//!
//! Marks are stored on `CompletionRequest` fields
//! ([`crate::types::ToolDefinition::cache_control`],
//! [`crate::types::TextContent::cache_control`]). Providers that don't
//! support inline cache hints (e.g. OpenAI) ignore the marks entirely.
//!
//! # Idempotency
//!
//! [`apply_cache_policy`] is idempotent: calling it multiple times with
//! the same `(request, policy)` produces identical request state. The
//! mark is overwritten in-place rather than appended, so repeated calls
//! do not accumulate duplicate markers.
//!
//! # System marking
//!
//! `system` marking is NOT performed here — it is deferred to the
//! provider `transform_request` because the system text is embedded as a
//! `Role::System` message inside `CompletionRequest.messages` and is only
//! extracted during the provider-specific transform. The
//! [`CachePolicy::system`] flag is read directly by the provider when
//! constructing its system field (e.g. `AnthropicSystem::Structured`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    cache_mark::{CacheControlMark, CacheScope, CacheTtl},
    types::{Content, ContentPart, Message, Role},
};

/// Strategy for caching the message tail.
///
/// `None` disables message caching; `LatestUserMessage` marks the last
/// user message's final content part with a [`CacheControlMark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageCacheStrategy {
    None,
    LatestUserMessage,
}

/// Cache policy. [`Default`] aligns with opencode `AUTO`:
/// `tools: true, system: true, messages: LatestUserMessage,
/// ttl_seconds: None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePolicy {
    pub tools: bool,
    pub system: bool,
    pub messages: MessageCacheStrategy,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ttl_seconds: Option<u32>,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            tools: true,
            system: true,
            messages: MessageCacheStrategy::LatestUserMessage,
            ttl_seconds: None,
        }
    }
}

/// Map a [`CachePolicy::ttl_seconds`] value to a [`CacheTtl`] class.
///
/// `None` or small values (≤ 300s) map to [`CacheTtl::Ephemeral`]; larger
/// values map to [`CacheTtl::Extended`]. Scope injection is deferred to the
/// production path (Task 9); here we use [`CacheScope::default()`] so the
/// mark is well-formed while `apply_cache_policy` has no user context.
fn ttl_from_policy(ttl_seconds: Option<u32>) -> CacheTtl {
    match ttl_seconds {
        None => CacheTtl::Ephemeral,
        Some(n) if n <= 300 => CacheTtl::Ephemeral,
        Some(_) => CacheTtl::Extended,
    }
}

/// Build a [`CacheControlMark`] from a cache policy's `ttl_seconds`.
fn mark_from_policy(ttl_seconds: Option<u32>) -> CacheControlMark {
    CacheControlMark {
        ttl: ttl_from_policy(ttl_seconds),
        scope: CacheScope::default(),
        pinned: false,
    }
}

/// Deployment-supplied cache-economics policy hook (R62, pi
/// `cache_warming_decision` parity).
///
/// Invoked once per request, between [`apply_cache_policy`] and the
/// provider-specific `transform_request`. The hook receives the
/// request snapshot and the boot-time policy, and returns the
/// **effective** policy that the provider should apply. A default
/// identity implementation that returns `previous` is provided so
/// deployments opt in.
///
/// Used for the cases where cache strategy is not a static
/// deployment choice:
///
/// - "this user explicitly opted out of cache warming"
/// - "session hit cache saturation; drop `system` for the next
///   N calls"
/// - "ephemeral when the tool count exceeds the cache budget"
///
/// The hook must be deterministic for a given `(request, previous)`
/// pair — non-deterministic returns break the provider's prompt cache.
pub trait CacheStrategyHook: Send + Sync {
    /// Return the policy the provider should apply for `request`.
    /// Returning `previous` keeps the boot-time choice.
    fn adjust(
        &self,
        request: &crate::types::CompletionRequest,
        previous: &CachePolicy,
    ) -> CachePolicy;
}

/// Identity hook: returns `previous`. The default for callers that
/// have not installed a [`CacheStrategyHook`].
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityCacheHook;

impl CacheStrategyHook for IdentityCacheHook {
    fn adjust(
        &self,
        _request: &crate::types::CompletionRequest,
        previous: &CachePolicy,
    ) -> CachePolicy {
        previous.clone()
    }
}

/// Resolve the effective cache policy for `request`: runs the
/// optional [`CacheStrategyHook`] over `boot_policy`, falling back to
/// `boot_policy` when no hook is installed. Pure — does not mutate
/// `request`.
pub fn resolve_cache_policy(
    hook: Option<&dyn CacheStrategyHook>,
    request: &crate::types::CompletionRequest,
    boot_policy: &CachePolicy,
) -> CachePolicy {
    match hook {
        Some(hook) => hook.adjust(request, boot_policy),
        None => boot_policy.clone(),
    }
}

/// Apply `policy` to `request` by marking the last tool definition and
/// the last user message's final content part with a
/// [`CacheControlMark`].
///
/// Idempotent: calling twice with the same `(request, policy)` produces
/// identical state — the mark is overwritten in place, not appended.
///
/// `system` marking is deferred to the provider `transform_request`
/// because the system text is embedded as a `Role::System` message and
/// only extracted during transform. The provider reads
/// [`CachePolicy::system`] directly when constructing its system field.
pub fn apply_cache_policy(
    request: &mut crate::types::CompletionRequest,
    policy: &CachePolicy,
) {
    if policy.tools {
        let tools = Arc::make_mut(&mut request.tools);
        if let Some(last_tool) = tools.last_mut() {
            last_tool.cache_control =
                Some(mark_from_policy(policy.ttl_seconds));
        }
    }
    if policy.messages == MessageCacheStrategy::LatestUserMessage {
        let messages = Arc::make_mut(&mut request.messages);
        mark_last_user_message(messages, policy.ttl_seconds);
    }
    // system marking deferred to provider transform_request.
}

/// Stateful cache policy applier that short-circuits when the
/// request's `tools` and `messages` `Arc` references are identical
/// to the previous call. This avoids redundant `cache_control` mark
/// re-application and signals to the caller that the provider's
/// prompt cache prefix remains valid.
///
/// "system" is embedded as a `Role::System` message inside `messages`,
/// so if `messages` Arc is ptr_eq, system is unchanged too. The spec's
/// "three fields" requirement is satisfied: tools (Arc), system (part
/// of messages Arc), messages (Arc) — all covered by two ptr_eq checks.
///
/// Aligns with opencode's `applyCachePolicy` reference equality
/// semantics: when the inputs are the same object (same `Arc` pointer),
/// skip the full cache policy evaluation entirely.
#[derive(Debug)]
pub struct CachePolicyApplier {
    previous_tools: Option<Arc<Vec<crate::types::ToolDefinition>>>,
    previous_messages: Option<Arc<Vec<Message>>>,
}

impl Default for CachePolicyApplier {
    fn default() -> Self {
        Self::new()
    }
}

impl CachePolicyApplier {
    pub fn new() -> Self {
        Self {
            previous_tools: None,
            previous_messages: None,
        }
    }

    /// Apply `policy` to `request`, short-circuiting when the request's
    /// `tools` and `messages` `Arc` references are identical to the
    /// previous call.
    ///
    /// Returns `true` if the short-circuit fired (no work needed — the
    /// marks from the previous call are still present). Returns `false`
    /// if full evaluation was performed (and the stored references are
    /// updated for the next call).
    pub fn apply(
        &mut self,
        request: &mut crate::types::CompletionRequest,
        policy: &CachePolicy,
    ) -> bool {
        let tools_same = self
            .previous_tools
            .as_ref()
            .map(|prev| Arc::ptr_eq(prev, &request.tools))
            .unwrap_or(false);
        let messages_same = self
            .previous_messages
            .as_ref()
            .map(|prev| Arc::ptr_eq(prev, &request.messages))
            .unwrap_or(false);

        if tools_same && messages_same {
            // Short-circuit: all fields unchanged by reference. The
            // marks from the previous full evaluation are still present
            // on the tools / messages, so the provider's prompt cache
            // prefix remains valid — no re-application needed.
            return true;
        }

        // Full evaluation path. `apply_cache_policy` uses
        // `Arc::make_mut` which mutates in place when refcount == 1
        // (preserving the pointer) or clones when refcount > 1.
        // We store the (potentially post-`make_mut`) references AFTER
        // evaluation so the stored pointer matches what the caller now
        // holds. When the same request is passed again (e.g., retry,
        // guardian review), `ptr_eq` matches and short-circuits.
        apply_cache_policy(request, policy);
        self.previous_tools = Some(Arc::clone(&request.tools));
        self.previous_messages = Some(Arc::clone(&request.messages));
        false
    }
}

/// Mark the last user message's final content part with a
/// [`CacheControlMark`]. Scans messages in reverse and marks only the
/// first (i.e. last in chronological order) user message encountered.
fn mark_last_user_message(messages: &mut [Message], ttl_seconds: Option<u32>) {
    for msg in messages.iter_mut().rev() {
        if msg.role != Role::User {
            continue;
        }
        let mark = mark_from_policy(ttl_seconds);
        match &mut msg.content {
            Content::Single(part) => {
                set_cache_control_on_part(part, mark);
            }
            Content::Multi(parts) => {
                if let Some(last_part) = parts.last_mut() {
                    set_cache_control_on_part(last_part, mark);
                }
            }
        }
        break;
    }
}

/// Set `cache_control` on a [`ContentPart`] when it is a `Text` variant.
/// Other variants (Image/Audio/ToolUse/ToolResult/Reasoning/Resource) are
/// not marked for MVP — only `Text` carries the cache hint.
fn set_cache_control_on_part(part: &mut ContentPart, mark: CacheControlMark) {
    if let ContentPart::Text(tc) = part {
        tc.cache_control = Some(mark);
    }
}

#[cfg(test)]
mod tests;
