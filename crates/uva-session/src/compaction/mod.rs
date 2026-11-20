//! Compaction seam — when a history outgrows its window, one
//! summary replaces the span it shadows.
//!
//! The trait is **provider-free**: `plan` decides *what* to
//! compact from the projected history and the token budget, and
//! `summarize` produces the replacement. The default
//! [`SummaryCompactor`] owns a provider handle and a model name,
//! so a host that wants an LLM summary constructs it once and
//! installs it on the agent.
//!
//! ## Who owns what
//!
//! - The **compactor** owns selection: the surface range, and the
//!   summary text that replaces it.
//! - The **harness** owns provenance: a plan sees provider
//!   messages, not log ordinals, so `shadowed_seqs` is filled from
//!   [`crate::surface::FoldedSurface::surface_seqs`] after `plan`
//!   returns. A summary's `Replace` therefore cites exactly the
//!   rows the fold showed the model, satisfying the fold's rule
//!   that every cited seq precedes the compaction row and no
//!   later than the previous surface maximum.
//!
//! ## Order
//!
//! Tool results are pruned **before** the summary call
//! ([`Compactor::prune`]): the summariser reads placeholders for
//! oversized results, not the full text. Pruning is a property of
//! the summary input only — the durable rows and the `Replace`
//! range are untouched.
//!
//! ## Pairing
//!
//! A summary that cuts between an assistant's `tool_use` and its
//! `tool_result` leaves the kept tail invalid. [`SummaryCompactor::plan`]
//! moves its cut back to the nearest balanced boundary
//! ([`crate::tool_pairing::balanced_cut_at_or_before`]).

pub mod settings;

use std::{ops::Range, sync::Arc};

use futures::{FutureExt, future::BoxFuture};
use serde_json::Value;
pub use settings::{
    CompactionDetails,
    CompactionSettings,
    TouchedPair,
    ValidationError,
    should_compact,
    touched_target,
};
use uva_core::{
    CancelToken,
    sleeper::{Sleeper, ThreadSleeper},
};
use uva_provider::{
    CompletionRequest,
    Content,
    ContentPart,
    Message,
    ModelProvider,
    Role,
    TokenUsage,
    estimate_messages_token_count,
};

use crate::{
    SessionError,
    events::{CompactionLifecycleOutcome, SurfaceOp, SurfaceToken},
    history::surface_message,
    log::SessionLog,
    log_surface::try_fold_log_surface,
    surface::FoldError,
    tool_pairing::balanced_cut_at_or_before,
    typed_event_builders::{
        compaction_end_committed,
        compaction_end_failed,
        compaction_row,
        compaction_start,
        compaction_summary,
    },
};

/// The placeholder an oversized tool result is replaced with in
/// the summary input.
pub const PRUNED_TOOL_RESULT: &str = "[tool result pruned]";

/// Results shorter than this many characters reach the summariser
/// verbatim.
pub const DEFAULT_PRUNE_MIN_CHARS: usize = 1_024;

const DEFAULT_RESERVE: usize = 16_384;
/// The kept tail may use this fraction of the post-reserve budget.
const TAIL_DIVISOR: usize = 2;
const SUMMARY_INSTRUCTION: &str = "You compact an assistant's \
conversation history for another assistant. Summarise what happened: \
the goal, the decisions, the files and tools that mattered, and what \
is still open. Keep concrete identifiers. Reply with the summary only.";

/// What a request may spend: the model's window and the reserve the
/// reply needs.
/// The ordinal a message carries when it has no log row of its own.
///
/// A live run's inbox turn was written by the host, so the loop does
/// not know its ordinal. Such a row cannot be cited as compaction
/// provenance, so a plan may not shadow it.
const UNCITABLE_SEQ: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenBudget {
    pub window: usize,
    pub reserve: usize,
}

impl TokenBudget {
    /// A budget for `window`, reserving the smaller of
    /// `DEFAULT_RESERVE` and a quarter of the window.
    #[must_use]
    pub fn for_window(window: usize) -> Self {
        Self {
            window,
            reserve: DEFAULT_RESERVE.min(window / 4),
        }
    }

    /// Tokens the history may occupy before compaction triggers.
    #[must_use]
    pub fn trigger(&self) -> usize {
        self.window.saturating_sub(self.reserve)
    }
}

/// One compaction decision: which surface messages to shadow and
/// what the replacement is made from.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionPlan {
    /// Surface range `[start, end)` the summary replaces.
    ///
    /// The range is a position in the **model-visible** surface: the
    /// fold's order, in which a steer that landed between a
    /// `tool_use` and its `tool_result` waits for the result. The
    /// durable fold reads the range in the same coordinates.
    pub range: Range<usize>,
    /// Seqs of the shadowed surface rows.
    ///
    /// Empty when `plan` built the plan (it sees no log); the
    /// harness fills it from the fold before `summarize`.
    pub shadowed_seqs: Vec<u64>,
    /// Number of messages the plan keeps after the cut.
    pub keep_recent: usize,
    /// The shadowed messages, snapshotted at plan time — the
    /// summariser's input.
    pub shadowed: Vec<Message>,
    /// Heuristic token price of the shadowed span, measured before
    /// pruning.
    pub shadowed_tokens: usize,
}

/// What one successful summary produced.
///
/// `shadowed_seqs` / `shadowed_tokens` echo the plan. The harness
/// treats them as advisory: it writes the *plan's* values into the
/// durable replace, so a compactor cannot cite a seq the fold did not
/// hand it. The identity fields describe the call that produced the
/// text; a custom compactor may leave them `None`.
#[derive(Clone, Debug)]
pub struct CompactionOutcome {
    /// The assistant turn that replaces the shadowed span.
    pub summary: String,
    /// The plan's shadowed seqs, echoed back.
    pub shadowed_seqs: Vec<u64>,
    /// The plan's pre-prune token price, echoed back.
    pub shadowed_tokens: usize,
    /// The summariser provider's name.
    pub provider: Option<String>,
    /// The model the summary was requested from.
    pub model: Option<String>,
    /// The provider's usage report for the summary call.
    pub usage: Option<TokenUsage>,
}

/// Why a summary did not happen.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CompactionError {
    /// The run was cancelled before the summary was produced.
    #[error("compaction cancelled")]
    Cancelled,
    /// The summariser's provider call failed.
    #[error("summariser provider failed: {0}")]
    Provider(String),
    /// The summariser did not answer within its timeout.
    #[error("summariser timed out after {0:?}")]
    Timeout(std::time::Duration),
    /// The provider returned no text.
    #[error("summariser returned no text")]
    EmptySummary,
}

/// Decides whether to compact and produces the replacement.
pub trait Compactor: Send + Sync {
    /// Return the span to compact, or `None` to leave history alone.
    fn plan(
        &self,
        history: &[Message],
        budget: TokenBudget,
    ) -> Option<CompactionPlan>;

    /// Prune the plan's tool results ahead of the summary call.
    ///
    /// The default replaces oversized tool-result text with
    /// [`PRUNED_TOOL_RESULT`]. Overrides keep the same contract:
    /// the returned messages are the summary input, never the
    /// surface.
    fn prune(&self, plan: &CompactionPlan) -> Vec<Message> {
        prune_tool_results(&plan.shadowed, DEFAULT_PRUNE_MIN_CHARS)
    }

    /// Produce the summary. May call the provider once.
    fn summarize(
        &self,
        plan: &CompactionPlan,
        cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>>;
}

/// The default compactor: keep a balanced recent tail, summarise
/// the rest with the installed provider.
pub struct SummaryCompactor {
    provider: Arc<dyn ModelProvider>,
    model: String,
    /// The host's compaction policy. `None` means the compactor
    /// obeys the [`TokenBudget`] it is handed and nothing else.
    settings: Option<CompactionSettings>,
    timeout: Option<std::time::Duration>,
    /// The seam that arms `timeout`. Runtime-free by default, so the
    /// crate never names a timer.
    sleeper: Arc<dyn Sleeper>,
}

/// The timeout a host should install on its summariser.
///
/// The harness is runtime-free, so it cannot arm a timer itself; a
/// host installs one with [`SummaryCompactor::with_sleeper`] and opts
/// into [`SummaryCompactor::with_timeout`]. Without a timeout a hung
/// provider is bounded only by the transport's own timeout.
pub const DEFAULT_SUMMARY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(120);

impl SummaryCompactor {
    /// Build the default compactor around `provider` and `model`.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            settings: None,
            timeout: None,
            sleeper: Arc::new(ThreadSleeper),
        }
    }

    /// Install the host's compaction policy.
    ///
    /// The settings own the trigger (`reserve_tokens`), the retained
    /// tail (`keep_recent_tokens`) and the master switch. Without
    /// them the compactor falls back to the [`TokenBudget`] it is
    /// handed, which is the right shape for a caller that has a
    /// window but no policy.
    #[must_use]
    pub fn with_settings(mut self, settings: CompactionSettings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Install the seam that waits out [`Self::with_timeout`].
    ///
    /// The default, [`ThreadSleeper`], waits on a std thread — it
    /// needs no reactor, at the cost of one thread per armed timeout.
    /// A host on a real runtime installs that runtime's sleeper (the
    /// tokio one lives in `uva-core-tokio`).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Override how long the provider call may take.
    ///
    /// The deadline is armed through [`Self::with_sleeper`]; the
    /// default sleeper needs no runtime, so this works on any
    /// executor.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn tail_cut(&self, history: &[Message], tail_budget: usize) -> usize {
        let mut tokens = 0usize;
        let mut cut = history.len();
        for (index, message) in history.iter().enumerate().rev() {
            let price =
                estimate_messages_token_count(std::slice::from_ref(message));
            if cut < history.len() && tokens.saturating_add(price) > tail_budget
            {
                break;
            }
            tokens = tokens.saturating_add(price);
            cut = index;
        }
        cut
    }
}

impl Compactor for SummaryCompactor {
    fn plan(
        &self,
        history: &[Message],
        budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        let trigger = match self.settings {
            // A disabled policy never compacts, whatever the
            // utilisation says.
            Some(settings) if !settings.enabled => return None,
            Some(settings) => budget.window.saturating_sub(
                usize::try_from(settings.reserve_tokens).unwrap_or(usize::MAX),
            ),
            None => budget.trigger(),
        };
        if trigger == 0 || history.len() < 2 {
            return None;
        }
        if estimate_messages_token_count(history) <= trigger {
            return None;
        }
        let tail_budget = match self.settings {
            Some(settings) => usize::try_from(settings.keep_recent_tokens)
                .unwrap_or(usize::MAX)
                .min(budget.window / 2)
                .max(1),
            None => (trigger / TAIL_DIVISOR).max(1),
        };
        let cut = self.tail_cut(history, tail_budget);
        let cut = balanced_cut_at_or_before(history, cut);
        // A leading system message is part of the provider's cached
        // prefix; never shadow it.
        let start = usize::from(history[0].role == Role::System);
        if cut <= start {
            return None;
        }
        let shadowed = history[start..cut].to_vec();
        Some(CompactionPlan {
            range: start..cut,
            shadowed_seqs: Vec::new(),
            keep_recent: history.len() - cut,
            shadowed_tokens: estimate_messages_token_count(&shadowed),
            shadowed,
        })
    }

    fn summarize(
        &self,
        plan: &CompactionPlan,
        cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        // Pruning is part of building the request, so it provably
        // precedes the provider call.
        let prompt = render_messages(&self.prune(plan));
        let provider = Arc::clone(&self.provider);
        let provider_name = self.provider.name().to_string();
        let model = self.model.clone();
        let shadowed_seqs = plan.shadowed_seqs.clone();
        let shadowed_tokens = plan.shadowed_tokens;
        let timeout = self.timeout;
        let sleeper = Arc::clone(&self.sleeper);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(CompactionError::Cancelled);
            }
            let request = CompletionRequest {
                model: model.clone(),
                messages: Arc::new(vec![
                    Message::system(SUMMARY_INSTRUCTION),
                    Message::user(prompt),
                ]),
                ..CompletionRequest::default()
            };
            // The three futures are raced in this order and no other:
            // a cancelled run reports cancellation even when the
            // provider's answer happens to be ready too, and the
            // deadline only wins once nothing else is.
            let response = match timeout {
                Some(timeout) => futures::select_biased! {
                    () = cancel.cancelled().fuse() => {
                        return Err(CompactionError::Cancelled);
                    }
                    result = provider.complete(request).fuse() => result
                        .map_err(|error| {
                            CompactionError::Provider(error.to_string())
                        })?,
                    () = sleeper.sleep(timeout).fuse() => {
                        return Err(CompactionError::Timeout(timeout));
                    }
                },
                None => futures::select_biased! {
                    () = cancel.cancelled().fuse() => {
                        return Err(CompactionError::Cancelled);
                    }
                    response = provider.complete(request).fuse() => response
                        .map_err(|error| {
                            CompactionError::Provider(error.to_string())
                        })?,
                },
            };
            let summary = response
                .content
                .extract_text()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .ok_or(CompactionError::EmptySummary)?;
            Ok(CompactionOutcome {
                summary,
                shadowed_seqs,
                shadowed_tokens,
                provider: Some(provider_name),
                model: Some(model.clone()),
                usage: Some(response.usage),
            })
        })
    }
}

/// Replace oversized tool-result text with a placeholder.
///
/// Copies every message, so the caller's history is untouched. A
/// tool message (`role == Tool`) and a `ToolResult` content part
/// are both pruned; results under `min_chars` keep their text.
#[must_use]
pub fn prune_tool_results(
    messages: &[Message],
    min_chars: usize,
) -> Vec<Message> {
    messages
        .iter()
        .map(|message| prune_one(message, min_chars))
        .collect()
}

fn prune_one(message: &Message, min_chars: usize) -> Message {
    if message.role != Role::Tool
        && !message
            .content
            .iter()
            .any(|part| matches!(part, ContentPart::ToolResult(_)))
    {
        return message.clone();
    }
    let mut pruned = message.clone();
    match &mut pruned.content {
        Content::Single(part) => prune_part(part, min_chars),
        Content::Multi(parts) => {
            for part in parts {
                prune_part(part, min_chars);
            }
        }
    }
    pruned
}

fn prune_part(part: &mut ContentPart, min_chars: usize) {
    match part {
        ContentPart::Text(text) if text.text.chars().count() >= min_chars => {
            text.text = format!(
                "{PRUNED_TOOL_RESULT} ({} chars)",
                text.text.chars().count()
            );
        }
        ContentPart::ToolResult(result) => {
            for inner in &mut result.content {
                prune_part(inner, min_chars);
            }
        }
        _ => {}
    }
}

fn render_messages(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        out.push_str(message.role.as_str());
        out.push_str(": ");
        for part in &message.content {
            render_part(part, &mut out);
        }
        out.push('\n');
    }
    out
}

fn render_part(part: &ContentPart, out: &mut String) {
    match part {
        ContentPart::Text(text) => out.push_str(&text.text),
        ContentPart::ToolUse(call) => {
            out.push_str(&format!("called {}({})", call.name, call.input));
        }
        ContentPart::ToolResult(result) => {
            for inner in &result.content {
                render_part(inner, out);
            }
        }
        ContentPart::Reasoning(reasoning) => out.push_str(&reasoning.text),
        ContentPart::Image(_) => out.push_str("[image]"),
        ContentPart::Audio(_) => out.push_str("[audio]"),
        ContentPart::Resource(link) => {
            out.push_str(&format!("[resource {}]", link.uri));
        }
    }
}

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// The durable pass, shared by the loop and a host's manual compaction
// ---------------------------------------------------------------------------

/// Why a compaction pass could not even plan.
#[derive(Debug, thiserror::Error)]
pub enum CompactionPassError {
    // The log's surface ops are inconsistent (bad provenance).
    #[error("the log does not fold into a surface: {0}")]
    Fold(#[from] FoldError),
    // A surface row is not a provider message the compactor can read.
    #[error("a surface row is not a projectable message")]
    NotAMessage,
    // The message list and its provenance disagree on length.
    #[error(
        "the message list and its surface ordinals disagree: \
         {messages} messages, {seqs} ordinals"
    )]
    MismatchedSurface {
        /// How many messages the caller supplied.
        messages: usize,
        /// How many ordinals the caller supplied.
        seqs: usize,
    },
}

/// What a compaction pass produced, for the caller's audit/UI.
#[derive(Debug)]
pub enum CompactionPass<'a> {
    // The summary replaced its span; the Compaction row's ordinal is
    // returned by [`finish_compaction`].
    Committed(&'a CompactionOutcome),
    // The summariser failed (or was cancelled). The audit end row is
    // still written so the start/end pair is never orphaned.
    Failed {
        /// The summariser's error, recorded in the end row.
        error: &'a str,
        /// `failed` or `interrupted`.
        outcome: CompactionLifecycleOutcome,
    },
}

/// What planning a pass decided.
#[derive(Debug)]
pub enum PlannedCompaction {
    // There is a span to summarise.
    Plan(CompactionPlan),
    // The compactor declined: the history is under the trigger.
    Declined,
    // The compactor named a span that the fold clamps to nothing (an
    // out-of-range plan, or the system prompt as the only candidate).
    // Distinct from [`PlannedCompaction::Declined`] so a host can say
    // why nothing was committed.
    EmptySpan,
}

/// Plan one pass over a history the caller already holds.
///
/// Asks `compactor` whether to compact and binds the plan's range and
/// provenance to `history`: the range is clamped to the span both
/// lists cover, the leading system message is never shadowed, the cut
/// moves back to the nearest balanced tool-pair boundary, the cut
/// stops before the first row the caller cannot cite, and
/// `shadowed_seqs` comes from `surface_seqs` (never from the
/// compactor). A plan the binding clamps to nothing is
/// [`PlannedCompaction::EmptySpan`], not an error — nothing is broken,
/// there is just nothing to shadow.
///
/// The rows are the **model-visible** surface: the fold's order, in
/// which a steer that landed between a `tool_use` and its
/// `tool_result` waits for the result. `surface_seqs` names the rows
/// in that order, so the range is a position in the surface the
/// durable fold reproduces.
///
/// The loop calls this with the transcript it already holds, so a
/// plan costs no log read. A host that has only the log calls
/// [`plan_compaction_from_rows`].
///
/// # Errors
///
/// Returns [`CompactionPassError::MismatchedSurface`] when the two
/// lists disagree on length.
pub fn plan_compaction(
    history: &[Message],
    surface_seqs: &[u64],
    compactor: &dyn Compactor,
    budget: TokenBudget,
) -> Result<PlannedCompaction, CompactionPassError> {
    if history.len() != surface_seqs.len() {
        return Err(CompactionPassError::MismatchedSurface {
            messages: history.len(),
            seqs: surface_seqs.len(),
        });
    }
    let Some(plan) = compactor.plan(history, budget) else {
        return Ok(PlannedCompaction::Declined);
    };
    Ok(match bind_plan(plan, history, surface_seqs) {
        Some(plan) => PlannedCompaction::Plan(plan),
        None => PlannedCompaction::EmptySpan,
    })
}

/// Plan one pass over a durable log.
///
/// Folds the log, decodes one message per surface row, and delegates
/// to [`plan_compaction`]. This is the manual-compaction entry point: a
/// host that holds only the rows pays one fold, and the harness — which
/// holds the transcript — calls [`plan_compaction`] directly.
///
/// # Errors
///
/// Returns [`CompactionPassError`] when the log does not fold or a
/// surface row is not a message.
pub fn plan_compaction_from_rows(
    rows: &[Value],
    compactor: &dyn Compactor,
    budget: TokenBudget,
) -> Result<PlannedCompaction, CompactionPassError> {
    let surface = try_fold_log_surface(rows)?;
    let history = surface
        .messages
        .iter()
        .map(surface_message)
        .collect::<Option<Vec<_>>>()
        .ok_or(CompactionPassError::NotAMessage)?;
    plan_compaction(&history, &surface.surface_seqs, compactor, budget)
}

// Rebuild a plan's content from the history it points at, then fill the
// provenance the compactor cannot know. `None` refuses an empty span.
//
// The cut moves back to the nearest balanced tool-pair boundary before
// anything is snapshotted: a summary that cut between an assistant's
// `tool_use` and its `tool_result` would leave the kept tail invalid,
// and a custom compactor must not be able to produce that shape.
//
// The cut also stops before the first row the caller cannot cite: a
// message with no ordinal of its own — an inbox turn the host wrote
// while the run was in flight — has no log row to shadow, and the
// durable fold refuses a replace that cites it.
fn bind_plan(
    mut plan: CompactionPlan,
    history: &[Message],
    surface_seqs: &[u64],
) -> Option<CompactionPlan> {
    if history.is_empty() {
        return None;
    }
    let floor = usize::from(history[0].role == Role::System);
    let start = plan.range.start.max(floor);
    let citable_end = surface_seqs
        .iter()
        .position(|seq| *seq == UNCITABLE_SEQ)
        .unwrap_or(surface_seqs.len());
    let end = plan
        .range
        .end
        .min(history.len())
        .min(surface_seqs.len())
        .min(citable_end);
    let end = balanced_cut_at_or_before(history, end);
    if start >= end {
        return None;
    }
    plan.range = start..end;
    plan.shadowed = history[start..end].to_vec();
    plan.shadowed_seqs = surface_seqs[start..end].to_vec();
    plan.shadowed_tokens = estimate_messages_token_count(&plan.shadowed);
    plan.keep_recent = history.len() - end;
    Some(plan)
}

/// Write the pass's `compaction_start` audit row.
///
/// Called **before** the summary call so a crash mid-pass leaves a
/// start row the loader can report as an orphaned compaction.
///
/// # Errors
///
/// Returns the writer's error when the row cannot be queued.
pub async fn begin_compaction(
    writer: &dyn SessionLog,
    plan: &CompactionPlan,
    reason: &str,
) -> Result<SurfaceToken, SessionError> {
    let token = SurfaceToken::new();
    writer
        .append_event(&compaction_start(
            token.as_str(),
            reason,
            plan.range.start,
            plan.range.end,
            plan.shadowed_tokens,
        ))
        .await?;
    Ok(token)
}

/// Write the pass's terminal rows and flush.
///
/// A committed pass writes `compaction_summary` → `compaction`
/// (`SurfaceOp::Replace` citing the plan's ordinals) → `compaction_end`.
/// A failed one writes only the end row. The flush lands the replace
/// before the caller re-derives a request from it, so an observer never
/// sees a summary the next request will not carry.
///
/// Returns the `compaction` row's ordinal for a committed pass.
///
/// # Errors
///
/// Returns the writer's error when a row cannot be queued or flushed.
pub async fn finish_compaction(
    writer: &dyn SessionLog,
    token: &SurfaceToken,
    reason: &str,
    plan: &CompactionPlan,
    pass: CompactionPass<'_>,
) -> Result<Option<u64>, SessionError> {
    let seq = match pass {
        CompactionPass::Committed(outcome) => {
            writer
                .append_event(&compaction_summary(
                    token.as_str(),
                    &outcome.summary,
                ))
                .await?;
            let seq = writer
                .append_event(&compaction_row(
                    &outcome.summary,
                    plan.range.clone().collect::<Vec<_>>(),
                    plan.shadowed_tokens,
                    outcome.provider.as_deref(),
                    outcome.model.as_deref(),
                    outcome.usage.as_ref(),
                    SurfaceOp::Replace {
                        start: plan.range.start,
                        end: plan.range.end,
                        source_event_seqs: plan.shadowed_seqs.clone(),
                    },
                ))
                .await?;
            writer
                .append_event(&compaction_end_committed(
                    token.as_str(),
                    plan.range.len(),
                    plan.shadowed_tokens,
                    reason,
                ))
                .await?;
            Some(seq)
        }
        CompactionPass::Failed { error, outcome } => {
            writer
                .append_event(&compaction_end_failed(
                    token.as_str(),
                    outcome,
                    error,
                    reason,
                ))
                .await?;
            None
        }
    };
    writer.flush().await?;
    Ok(seq)
}
