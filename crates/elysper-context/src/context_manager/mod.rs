//! Pluggable, async context-window management.
//!
//! Mirrors [`traitclaw_core::traits::context_manager::ContextManager`]:
//! `prepare` is called before every LLM request to ensure the message
//! list fits within the model's context window. The method is `async`
//! so implementations can call out to a summarisation LLM or a remote
//! tokenizer without blocking the agent runtime.
//!
//! # Contract
//!
//! - Implementations **MUST NOT** remove messages whose role is
//!   [`Role::System`] — those carry the agent's persistent identity,
//!   tool manifest, and skill manifest, and the model relies on them
//!   landing at the high-attention edge of the context.
//! - Implementations **MUST** update `state.estimated_tokens` to
//!   reflect the post-prepare message list so downstream consumers
//!   (the `Tracker` / `Hint` subsystems and the `Usage` system
//!   event) can quote the same number the manager itself used to
//!   make its decision.
//! - Implementations **SHOULD** set `state.last_truncated = true`
//!   whenever they removed or compressed any message. The agent
//!   loop surfaces this flag as a `WarningKind::ContextTruncated`
//!   system event so the UI can render a visible notice.
//!
//! # Default estimation
//!
//! The trait ships with a cheap `4-char ≈ 1-token` heuristic that
//! delegates to [`elysper_provider::token_counter::estimate_messages_token_count`].
//! Elysper's `Message.content` is a `Vec<ContentPart>` that may carry
//! tool-use JSON, images, and audio, all of which that estimator
//! collapses to text-only — it is intentionally conservative.
//! Implementations that need provider-accurate counts (Anthropic
//! tokenizer, OpenAI `tiktoken`) override `estimate_tokens`.
//!
//! [`traitclaw_core::traits::context_manager::ContextManager`]:
//!     https://docs.rs/traitclaw-core

use std::sync::Arc;

use async_trait::async_trait;
use elysper_provider::{
    Message,
    ModelConfig,
    Role,
    TokenUsage,
    token_counter::estimate_messages_token_count,
};

/// Per-category usage attribution for one agent run (dsh
/// token-meter semantics).
/// `AgentState` holds one of these and feeds it via
/// [`AgentState::add_token_usage`]; after the run the caller
/// reads the split for cost dashboards and billing exports.
/// All counters saturate on overflow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageMeter {
    /// Sum of provider-reported prompt tokens.
    pub input_tokens: usize,
    /// Sum of provider-reported completion tokens (includes
    /// reasoning tokens where the provider bills them as output).
    pub output_tokens: usize,
    /// Sum of KV-cache read tokens (Anthropic
    /// `cache_read_input_tokens` / OpenAI `cached_tokens`).
    pub cache_read_tokens: usize,
    /// Sum of KV-cache write tokens (Anthropic
    /// `cache_creation_input_tokens`).
    pub cache_write_tokens: usize,
    /// Sum of reasoning / thinking tokens (OpenAI
    /// `completion_tokens_details.reasoning_tokens`).
    pub reasoning_tokens: usize,
    /// Number of sampling passes recorded.
    pub calls: usize,
}

impl UsageMeter {
    /// Fold one provider response's usage into the meter.
    /// `None` optionals contribute `0`.
    pub fn record(&mut self, usage: &TokenUsage) {
        self.input_tokens =
            self.input_tokens.saturating_add(usage.prompt_tokens);
        self.output_tokens =
            self.output_tokens.saturating_add(usage.completion_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens.unwrap_or(0));
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens.unwrap_or(0));
        self.reasoning_tokens = self
            .reasoning_tokens
            .saturating_add(usage.reasoning_tokens.unwrap_or(0));
        self.calls += 1;
    }

    /// Cache hit ratio over the run: cache-read tokens divided
    /// by total input tokens (`0.0` when no input was recorded).
    #[allow(clippy::cast_precision_loss)]
    pub fn cache_hit_ratio(&self) -> f32 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f32 / self.input_tokens as f32
    }
}

/// Snapshot of the agent's runtime state passed to [`ContextManager`].
///
/// The agent loop constructs one of these at the start of each
/// session from the resolved [`ModelConfig`] and hands it to
/// every `prepare` call. Implementations read it for
/// context-budget information and may mutate it to publish their
#[derive(Clone)]
/// Snapshot of the agent's runtime state passed to [`ContextManager`].
///
/// own observations (the post-prepare token estimate, whether
/// anything was dropped).
pub struct AgentState {
    /// Maximum token budget for this run. Comes straight from
    /// [`ModelConfig::context_window`] and stays constant for the
    /// lifetime of a session.
    pub context_window: usize,
    /// Post-`prepare` token estimate of the full message list.
    /// Read by the `SamplingResult::usage` pipeline
    /// and by `Hint` implementations that fire when utilisation
    /// crosses a threshold.
    pub estimated_tokens: usize,
    /// Current ReAct iteration. Bumped by `ReActLoop::react_step`
    /// before each sample pass; the manager reads it but never
    /// writes it (the loop owns the canonical counter).
    pub iteration_count: usize,
    /// `true` when the most recent `prepare` actually removed or
    /// compressed a message. Reset to `false` at the start of every
    /// `prepare` call so callers can rely on the flag meaning
    /// "this call did something".
    pub last_truncated: bool,
    /// Provider-reported token totals accumulated over the whole
    /// run (prompt + completion, every sampling pass summed).
    /// Maintained by the agent loop right after each provider
    /// response; read by budget guards and hints.
    pub total_tokens: usize,
    /// Per-category usage attribution accumulated over the whole
    /// run (dsh token-meter semantics): input / output /
    /// cache-read / cache-write / reasoning. Fed by
    /// [`AgentState::add_token_usage`]; queryable after the run
    /// for cost dashboards and billing exports.
    pub usage_meter: UsageMeter,
    /// Number of tool calls dispatched this run (including calls
    /// later denied by a guard — the counter bumps before the
    /// guard pipeline runs). Maintained by the agent loop.
    pub tool_call_count: usize,
    /// Rolling fingerprints of recently dispatched tool calls
    /// (`tool_name` + canonicalised arguments), oldest first.
    /// Capped at [`FINGERPRINT_WINDOW`]. The steering layer's
    /// loop-detection guard reads the identical tail run to spot
    /// a model stuck repeating the exact same call.
    pub recent_tool_fingerprints: Vec<String>,
}

/// Cap on [`AgentState::recent_tool_fingerprints`]. Sixteen
/// entries is far beyond any sane loop-detection window while
/// keeping the per-run state bounded.
pub const FINGERPRINT_WINDOW: usize = 16;

impl AgentState {
    /// Construct a fresh state from a model's resolved
    /// [`ModelConfig`]. `estimated_tokens` starts at `0`; the agent
    /// loop runs `prepare` once at session start to populate it
    /// before the first LLM call.
    pub fn from_config(cfg: &ModelConfig) -> Self {
        Self::with_window(cfg.context_window)
    }

    /// Construct a state with an explicit window. Used by tests and
    /// by callers that do not have a full [`ModelConfig`] in scope
    /// (e.g. the embedded unit tests in this module).
    pub fn with_window(context_window: usize) -> Self {
        Self {
            context_window,
            estimated_tokens: 0,
            iteration_count: 0,
            last_truncated: false,
            total_tokens: 0,
            usage_meter: UsageMeter::default(),
            tool_call_count: 0,
            recent_tool_fingerprints: Vec::new(),
        }
    }

    /// Record one dispatched tool call: bump the counter and push
    /// the call's fingerprint onto the rolling window. Called by
    /// the agent loop before the guard pipeline so guards observe
    /// the call being counted.
    pub fn record_tool_call(&mut self, fingerprint: String) {
        self.tool_call_count += 1;
        self.recent_tool_fingerprints.push(fingerprint);
        if self.recent_tool_fingerprints.len() > FINGERPRINT_WINDOW {
            let overflow =
                self.recent_tool_fingerprints.len() - FINGERPRINT_WINDOW;
            self.recent_tool_fingerprints.drain(0..overflow);
        }
    }

    /// Length of the trailing run of identical fingerprints (0 when
    /// the window is empty). `n` means the last `n` dispatched
    /// tool calls were indistinguishable from each other.
    pub fn identical_tail_run(&self) -> usize {
        let Some(last) = self.recent_tool_fingerprints.last() else {
            return 0;
        };
        self.recent_tool_fingerprints
            .iter()
            .rev()
            .take_while(|fp| *fp == last)
            .count()
    }

    /// Accumulate provider-reported usage into [`AgentState::total_tokens`]
    /// (counting prompt + completion tokens; cache reads stay excluded
    pub fn add_token_usage(&mut self, usage: &TokenUsage) {
        self.total_tokens =
            self.total_tokens.saturating_add(usage.total_tokens);
        self.usage_meter.record(usage);
    }

    /// Fraction of the context window that the current message list
    /// is consuming (0.0 - 1.0). Returns `0.0` for an unset
    /// window so callers never divide by zero.
    #[allow(clippy::cast_precision_loss)]
    pub fn context_utilization(&self) -> f32 {
        if self.context_window == 0 {
            return 0.0;
        }
        self.estimated_tokens as f32 / self.context_window as f32
    }
}

/// Async trait for pluggable context-window management.
///
/// Called by `ReActLoop::prepare` before every provider call. The
/// implementation decides how to keep the message list under
/// `state.context_window` — typical strategies are pairwise
/// user/assistant drop, LLM-driven summarisation of the oldest
/// turn, and tool-result eviction.
/// A `ContextManager` trims the message list to fit inside the
/// configured context window before each LLM call.
///
/// The trait exposes **two** surfaces:
///
/// - [`ContextManager::prepare`] (mutating, legacy). Accepts
///   `&mut Vec<Message>`; implementations may shrink in place. The
///   synthesia core has used this shape since v0; the agent loop
///   continues to call it on the *first* iteration where no
///   cached `Arc` exists yet.
/// - [`ContextManager::prepare_arc`] (Arc-shared, R6-6). Accepts
///   `Arc<Vec<Message>>` and returns the same `Arc` when no
///   truncation happened, or a fresh `Arc::new(...)` when it
///   did. The agent loop's hot path uses this surface so the
///   downstream [`elysper_provider::cache_policy::CachePolicyApplier`]
///   can `Arc::ptr_eq` the messages Arc against the previous call
///   and short-circuit cache-marker recomputation when nothing
///   actually changed.
///
/// All shipped managers (`Noop`, `Truncating`) implement
/// `prepare_arc` natively; the `prepare` default impl falls back to
/// the legacy surface via
/// `Arc::get_mut` so callers that *only* override `prepare` still
/// get correct semantics — they just lose the cache shortcut.
/// Blanket impl: `Arc<T>` derefs to `T`, so an
/// `Arc<dyn ContextManager>` field can hold any concrete manager
/// (including a `TruncatingContextManager`, an `InlineSpawner`,
/// or a test counter).
#[async_trait]
impl<T: ContextManager + ?Sized> ContextManager for Arc<T> {
    async fn prepare(
        &self,
        messages: &mut Vec<Message>,
        state: &mut AgentState,
    ) {
        (**self).prepare(messages, state).await
    }

    async fn prepare_arc(
        &self,
        messages: Arc<Vec<Message>>,
        state: &mut AgentState,
    ) -> Arc<Vec<Message>> {
        (**self).prepare_arc(messages, state).await
    }

    async fn kept_indices(
        &self,
        prefix: &[Message],
        body: &[Message],
        state: &mut AgentState,
    ) -> Option<Vec<usize>> {
        (**self).kept_indices(prefix, body, state).await
    }

    fn estimate_tokens(&self, messages: &[Message]) -> usize {
        (**self).estimate_tokens(messages)
    }
}

#[async_trait]
pub trait ContextManager: Send + Sync {
    /// Mutate `messages` in place until it fits within
    /// `state.context_window`. May call out to other async
    /// services (an LLM, a remote tokenizer, a memory store).
    /// Prefer [`ContextManager::prepare_arc`] on the agent loop's
    /// hot path so the Arc-shared cache contract survives.
    async fn prepare(
        &self,
        messages: &mut Vec<Message>,
        state: &mut AgentState,
    );

    /// Arc-shared variant of [`ContextManager::prepare`]. Returns
    /// the input `Arc` unchanged when the manager did not need
    /// to mutate the list, and a fresh `Arc` when it did. The
    /// default implementation materialises a copy of `messages`
    /// via `Arc::get_mut` (which requires unique ownership, so
    /// the input `Arc` must be the sole holder) and dispatches
    /// to the legacy [`ContextManager::prepare`]; this default
    /// is sound but loses the cache shortcut. Override
    /// `prepare_arc` directly to keep the Arc alive on the
    /// no-op fast path.
    async fn prepare_arc(
        &self,
        messages: Arc<Vec<Message>>,
        state: &mut AgentState,
    ) -> Arc<Vec<Message>> {
        match Arc::try_unwrap(messages) {
            Ok(mut owned) => {
                self.prepare(&mut owned, state).await;
                Arc::new(owned)
            }
            Err(shared) => {
                // Another consumer (cache policy, prompt assembler)
                // still holds the Arc. Materialise a fresh copy,
                // apply the manager, return.
                let mut copy = shared.as_ref().clone();
                self.prepare(&mut copy, state).await;
                Arc::new(copy)
            }
        }
    }

    /// Estimate the token cost of a message list. The default
    /// delegates to the cheap
    /// [`elysper_provider::token_counter::estimate_messages_token_count`]
    /// helper; override with a provider-accurate counter
    /// (tiktoken, Anthropic tokenizer) when tight-window
    /// decisions matter.
    fn estimate_tokens(&self, messages: &[Message]) -> usize {
        elysper_provider::token_counter::estimate_messages_token_count(messages)
    }

    /// The messages of `body` that survive the window, as indices
    /// into `body`, ascending.
    ///
    /// `prefix` is the request's leading messages — the assembled
    /// system prompt — which precede `body` in the list the manager
    /// prunes and are never dropped. The contract:
    ///
    /// - the projection names exactly the messages
    ///   [`ContextManager::prepare`] keeps when it is handed
    ///   `prefix ++ body`, in the same order;
    /// - `state` carries the same values `prepare` would have left
    ///   behind — the estimate of the surviving list and whether
    ///   anything was dropped;
    /// - `None` means "no projection known": the caller keeps every
    ///   message and lets `prepare` do the pruning.
    ///
    /// The projection exists so a caller that owns the transcript can
    /// copy only what survives. Copying the whole transcript and then
    /// pruning the copy does the same work twice — once for every
    /// message the window is about to drop.
    async fn kept_indices(
        &self,
        prefix: &[Message],
        body: &[Message],
        state: &mut AgentState,
    ) -> Option<Vec<usize>> {
        let _ = (prefix, body, state);
        None
    }
}

/// Boxed, type-erased handle. Most call sites hold this rather than
/// the concrete type so they can be swapped at runtime (the
/// `elysper-server` layer may pick the strategy based on the
/// `ModelConfig`).
pub type SharedContextManager = Arc<dyn ContextManager>;

/// No-op manager — never mutates `messages`, only refreshes the
/// token estimate. Use for very small context windows (e.g. a
/// 4 kB model where the prompt itself dominates) or for tests
/// that need a deterministic message list.
pub struct NoopContextManager;

#[async_trait]
impl ContextManager for NoopContextManager {
    async fn prepare(
        &self,
        messages: &mut Vec<Message>,
        state: &mut AgentState,
    ) {
        state.estimated_tokens = self.estimate_tokens(messages);
        state.last_truncated = false;
        state.estimated_tokens = self.estimate_tokens(messages);
        state.last_truncated = false;
    }

    /// No-op manager: return the input `Arc` unchanged so the
    /// provider-side cache policy can `Arc::ptr_eq` against the
    /// previous request.
    async fn prepare_arc(
        &self,
        messages: Arc<Vec<Message>>,
        state: &mut AgentState,
    ) -> Arc<Vec<Message>> {
        state.estimated_tokens = self.estimate_tokens(&messages);
        state.last_truncated = false;
        messages
    }
}

/// Default pairing-drop strategy.
///
/// Walks the message list from oldest to newest, evicting the
/// oldest **user** message and the assistant reply that follows it
/// as a single atomic pair, repeating until the list fits within
/// `state.context_window`. System messages are preserved verbatim
/// and never reordered. Tool-result messages attached to a dropped
/// assistant turn are dropped with it (no orphaned tool results
/// left in the visible history).
///
/// # Why pairwise drop
///
/// Dropping only the user message would leave the assistant reply
/// ungrounded, and dropping only the assistant reply would leave
/// the user question dangling — both shapes confuse the model in
/// practice. Pairwise drop keeps the conversation surface
/// continuous and is the cheapest correct strategy for
/// provider-side context windows that lack a built-in
/// summariser.
pub struct TruncatingContextManager;

// ---------------------------------------------------------------------
// The drop plan
//
// The pairwise drop needs two facts about a message: its role (what
// the rule looks at) and its own token estimate (what the budget is
// made of). Both fit in a slot, so the decision can be taken over a
// list the caller does not own — the plan is computed over slots,
// never over the messages themselves.
//
// The estimator is additive: `estimate_messages_token_count` converts
// each message's units once and sums the results, so a message's slot
// carries exactly the share of the list's estimate that belongs to
// it. That is what lets the plan subtract a dropped message instead of
// re-summing the survivors.
// ---------------------------------------------------------------------

/// One message's drop-relevant facts.
#[derive(Clone, Copy)]
struct Slot {
    /// The message's role — the drop rule looks at it.
    role: Role,
    /// The message's own share of the list's token estimate.
    tokens: usize,
}

/// The slots of `messages`.
fn slots_of(messages: &[Message]) -> Vec<Slot> {
    messages
        .iter()
        .map(|message| Slot {
            role: message.role,
            tokens: estimate_messages_token_count(std::slice::from_ref(
                message,
            )),
        })
        .collect()
}

/// The messages the window does not fit, as indices into `slots`.
///
/// The rule is the pairwise drop: the oldest user message and the
/// assistant reply that follows it go together, and the scan repeats
/// until the tail fits. The leading system messages are never
/// dropped.
///
/// `whole_tokens` is the estimate of the *whole* list — the system
/// prefix included — and it is the entry condition, which is
/// deliberately stricter than the tail-only budget the loop settles
/// on: a system prompt that alone exceeds the window must still let
/// the tail be evicted rather than short-circuiting the loop before
/// it drops anything (pinned by `truncating_never_drops_system_
/// messages`).
fn drop_plan(slots: &[Slot], whole_tokens: usize, window: usize) -> Vec<usize> {
    // The leading system prefix: preserved verbatim, never reordered.
    let mut in_system = true;
    let mut sys_end = 0;
    for (index, slot) in slots.iter().enumerate() {
        if in_system && slot.role == Role::System {
            sys_end = index + 1;
        } else {
            in_system = false;
        }
    }
    // The tail's own total, and the messages it holds. The scan
    // works on the *surviving* indices, so a drop shifts the next
    // message into the freed slot exactly as `Vec::remove` does.
    let mut tail: Vec<usize> = (sys_end..slots.len()).collect();
    let mut tail_tokens: usize =
        tail.iter().map(|index| slots[*index].tokens).sum();
    let mut dropped: Vec<usize> = Vec::new();
    let mut over_budget = whole_tokens;
    let mut i = 0;
    while over_budget > window && i < tail.len() {
        let index = tail[i];
        if slots[index].role == Role::User {
            // Drop the user turn. Re-check before deciding whether
            // the following assistant turn also has to go (the
            // assistant turn is the answer to this question; keeping
            // it without the question is incoherent).
            tail_tokens = tail_tokens.saturating_sub(slots[index].tokens);
            dropped.push(index);
            tail.remove(i);
            over_budget = tail_tokens;

            if i < tail.len() && slots[tail[i]].role == Role::Assistant {
                let index = tail[i];
                tail_tokens = tail_tokens.saturating_sub(slots[index].tokens);
                dropped.push(index);
                tail.remove(i);
                over_budget = tail_tokens;
            }
            // Cursor does not advance: the next element has shifted
            // into the slot we just freed.
        } else {
            i += 1;
        }
    }
    dropped
}

/// Remove the messages the plan drops, in one pass.
///
/// The old shape removed each dropped message from the vector as the
/// scan went, which is a memmove of the whole tail per drop — so a
/// long history paid a quadratic number of moves. The plan knows
/// every drop up front, so one pass does.
fn apply_plan(messages: &mut Vec<Message>, dropped: &[usize]) {
    if dropped.is_empty() {
        return;
    }
    let mut kept: Vec<Message> =
        Vec::with_capacity(messages.len() - dropped.len());
    let mut drops = dropped.iter().peekable();
    for (index, message) in messages.drain(..).enumerate() {
        if drops.peek() == Some(&&index) {
            drops.next();
            continue;
        }
        kept.push(message);
    }
    *messages = kept;
}

#[async_trait]
impl ContextManager for TruncatingContextManager {
    async fn prepare(
        &self,
        messages: &mut Vec<Message>,
        state: &mut AgentState,
    ) {
        // Reset the truncation flag every call — its semantic is
        // "this call removed something", not "any call ever
        // removed something".
        state.last_truncated = false;
        state.estimated_tokens = self.estimate_tokens(messages);

        // Already fits. No work to do; the manager returns
        // silently and the loop proceeds to the provider.
        if state.estimated_tokens <= state.context_window {
            return;
        }

        // The system prefix is guaranteed non-empty under production
        // code paths: ReActLoop always injects the assembled system
        // prompt first. We `debug_assert!` rather than `return` so a
        // malformed caller surfaces as a test failure instead of
        // silent bypass.
        let slots = slots_of(messages);
        debug_assert!(
            slots.first().is_some_and(|slot| slot.role == Role::System),
            "ReActLoop must inject at least one System message before ContextManager::prepare"
        );

        let dropped =
            drop_plan(&slots, state.estimated_tokens, state.context_window);
        if dropped.is_empty() {
            return;
        }
        apply_plan(messages, &dropped);
        state.last_truncated = true;
        state.estimated_tokens = self.estimate_tokens(messages);
    }

    /// Arc-shared variant: when the input already fits, return it
    /// unchanged so the cache policy's `Arc::ptr_eq` shortcut
    /// survives. Only materialises a fresh `Arc` when the
    /// pairwise drop actually fires.
    async fn prepare_arc(
        &self,
        messages: Arc<Vec<Message>>,
        state: &mut AgentState,
    ) -> Arc<Vec<Message>> {
        state.last_truncated = false;
        state.estimated_tokens = self.estimate_tokens(&messages);

        if state.estimated_tokens <= state.context_window {
            return messages;
        }

        // Truncation needed. Materialise a fresh Vec, hand off to
        // the legacy `prepare` (which already does pairwise drop
        // and final token re-estimation), and re-wrap.
        match Arc::try_unwrap(messages) {
            Ok(mut owned) => {
                self.prepare(&mut owned, state).await;
                Arc::new(owned)
            }
            Err(shared) => {
                let mut copy = shared.as_ref().clone();
                self.prepare(&mut copy, state).await;
                Arc::new(copy)
            }
        }
    }

    /// The pairwise drop, decided without owning the messages.
    ///
    /// The projection is the same decision `prepare` makes — the
    /// same plan, over the same view (`prefix ++ body`) — so a
    /// caller that copies only the projected messages sends the model
    /// exactly the list `prepare` would have left behind. The
    /// contract test in this module pins that equality.
    async fn kept_indices(
        &self,
        prefix: &[Message],
        body: &[Message],
        state: &mut AgentState,
    ) -> Option<Vec<usize>> {
        let mut slots = Vec::with_capacity(prefix.len() + body.len());
        slots.extend(slots_of(prefix));
        let body_start = slots.len();
        slots.extend(slots_of(body));

        // The whole view's estimate: the prefix's share plus the
        // body's. The estimator is additive, so the slots carry the
        // same number `estimate_tokens` would have returned.
        let whole_tokens: usize = slots.iter().map(|slot| slot.tokens).sum();
        state.estimated_tokens = whole_tokens;
        state.last_truncated = false;

        let dropped = if whole_tokens <= state.context_window {
            Vec::new()
        } else {
            drop_plan(&slots, whole_tokens, state.context_window)
        };
        if dropped.is_empty() {
            // Nothing was dropped: every message of the body
            // survives, and the estimate is the whole view's.
            return Some((0..body.len()).collect());
        }

        // The survivors, in order. The dropped indices are ascending,
        // so one walk over the body answers both which messages
        // survive and what they cost.
        let mut kept: Vec<usize> = Vec::with_capacity(body.len());
        let mut kept_tokens = 0usize;
        let mut drops = dropped.iter().peekable();
        for (offset, slot) in slots[body_start..].iter().enumerate() {
            let index = body_start + offset;
            if drops.peek() == Some(&&index) {
                drops.next();
                continue;
            }
            kept_tokens += slot.tokens;
            kept.push(offset);
        }
        // The surviving list's estimate: the prefix (never dropped)
        // plus the messages of the body that survived.
        let prefix_tokens: usize =
            slots[..body_start].iter().map(|slot| slot.tokens).sum();
        state.estimated_tokens = prefix_tokens + kept_tokens;
        state.last_truncated = true;
        Some(kept)
    }
}

#[cfg(test)]
mod tests;
