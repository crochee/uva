//! Usage-anchored token meter — a pure fold over the durable
//! session event stream (dsh `token-meter` parity).
//!
//! Reference: `deepseek-harness/packages/llm/token-meter/src/
//! {index,projection,usage-projection}.ts`. Three concerns live
//! here, all derived from the same replay:
//!
//! 1. **Disjoint usage buckets** ([`UsageBuckets`]): the
//!    cumulative provider-reported `input` / `cache_read` /
//!    `cache_write` / `output` totals across the whole log. The
//!    four buckets never overlap — reasoning tokens are already
//!    inside `output` and are not counted twice.
//! 2. **Provider-usage anchoring** ([`TokenMeter`]): a
//!    provider-reported [`SessionEvent::Usage`] sample is adopted
//!    as the pressure baseline only when it plausibly priced the
//!    surface its request saw; the cheap char heuristic
//!    ([`estimate_message_tokens`]) covers the signed surface
//!    delta since that anchor.
//! 3. **Projected next-request tokens**
//!    ([`TokenMeasurement::total_tokens`]):
//!    `max(0, anchored_pressure + surface_delta_since_sample)` —
//!    the figure the compaction gate and status displays consume.
//!
//! ## Anchor rule
//!
//! A usage sample is stamped against the surface *its request
//! saw* — the surface immediately before the assistant response
//! joined it (`response_boundary_tokens` in the fold). Adoption
//! requires the sample's prompt-side total to be at least the
//! heuristic price of that surface; a provider count *below* the
//! heuristic estimate cannot describe this content (stale or
//! route-mismatched), so the meter falls back to the
//! conservative heuristic baseline instead. Because the heuristic
//! systematically underprices CJK text and JSON schemas, a genuine
//! sample is always adopted — anchoring is what keeps that
//! underpricing out of the occupancy figure.
//!
//! A later [`SessionEvent::RequestHeader`] drops the anchor: the
//! sample priced the previous request envelope, not the new one.
//!
//! ## Replay contract
//!
//! [`TokenMeter::observe`] is idempotent catch-up: pass the full
//! durable log each time and the meter folds only the events past
//! its cursor. A fold error (bad replace provenance, shrunken
//! log) leaves the failing event unconsumed so a retry re-sees
//! it; already-folded state is never half-applied.
//!
//! The fold reads the **typed** surface layer
//! ([`SessionEvent::UserMessage`] / `AssistantMessage` /
//! `ToolResult` / `Compaction` rows), exactly like
//! [`crate::surface::fold_surface`]; pre-R4 legacy envelopes
//! (`UserInput` / `Model` rows) contribute no
//! surface. A caller replaying a mixed log should feed the meter
//! the same typed partition it feeds `fold_surface`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uva_provider::{
    Message,
    TokenUsage,
    token_counter::estimate_messages_token_count,
};

use crate::{
    events::{SessionEvent, SurfaceOp},
    surface::{
        FoldError,
        fold::{RowKind, SurfaceFold, SurfaceRow, row_view},
    },
};

/// Disjoint provider usage buckets for one report or one folded
/// log.
///
/// The four buckets never overlap: `cache_read` / `cache_write`
/// are separate from `input`, and reasoning tokens are already
/// included in `output`. Every field carries a serde default so
/// partial payloads (and pre-R30 rows) keep parsing.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct UsageBuckets {
    /// Uncached prompt tokens the provider billed.
    #[serde(default)]
    pub input_tokens: u64,
    /// Completion tokens (reasoning included, not double-counted).
    #[serde(default)]
    pub output_tokens: u64,
    /// KV-cache read tokens; `None` when the provider did not
    /// report cache metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// KV-cache write tokens; `None` when unreported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

impl UsageBuckets {
    /// Project the provider wire usage onto the disjoint buckets
    /// (`prompt_tokens` → input, `completion_tokens` → output).
    #[must_use]
    pub fn from_token_usage(usage: &TokenUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens as u64,
            output_tokens: usage.completion_tokens as u64,
            cache_read_tokens: usage
                .cache_read_tokens
                .map(|tokens| tokens as u64),
            cache_write_tokens: usage
                .cache_write_tokens
                .map(|tokens| tokens as u64),
        }
    }

    /// Prompt-side pressure of the report: input plus cache read
    /// and write traffic. Response output is excluded, so the
    /// figure does not grow while the current turn streams.
    #[must_use]
    pub fn prompt_side_tokens(&self) -> u64 {
        self.input_tokens
            + self.cache_read_tokens.unwrap_or(0)
            + self.cache_write_tokens.unwrap_or(0)
    }

    /// Sum of all four disjoint buckets.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.prompt_side_tokens() + self.output_tokens
    }

    /// Saturating-accumulate another report into this one. Absent
    /// optional buckets stay absent only when both sides are
    /// absent; any present value wins through the merge.
    pub fn add(&mut self, other: &Self) {
        self.input_tokens =
            self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens =
            self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens =
            merge_bucket(self.cache_read_tokens, other.cache_read_tokens);
        self.cache_write_tokens =
            merge_bucket(self.cache_write_tokens, other.cache_write_tokens);
    }
}

fn merge_bucket(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (None, Some(b)) => Some(b),
        (Some(a), None) => Some(a),
        (None, None) => None,
    }
}

/// User-facing context-occupancy reference (dsh
/// `ContextPressureProjection` parity).
///
/// The fields are deliberately **not** one atomic request
/// observation: the pressure pair is a last-wins record of the
/// newest usage sample while `context_window` is the newest
/// advertised route capacity. Switching models can therefore pair
/// a fresh capacity with the previous route's pressure until the
/// next request reports usage. This is an intentional trade-off —
/// the value is a display and gating reference, not a billing
/// input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPressure {
    /// Provider-reported prompt-side size of the most recent
    /// request (`UsageBuckets::prompt_side_tokens`).
    pub pressure_tokens: u64,
    /// What the *next* request's prompt would cost:
    /// `pressure_tokens` plus the heuristic repricing of
    /// everything the surface gained or lost since that sample.
    /// Only the delta is estimated, so the figure stays anchored
    /// to the provider while still reacting the moment a
    /// compaction shadows a span — which `pressure_tokens` alone
    /// cannot do, since compaction reports no usage of its own.
    pub projected_tokens: u64,
    /// Newest recorded route capacity; `None` when no adapter
    /// advertised one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// The baseline a [`TokenMeasurement`] projects from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeasurementBaseline {
    /// Nothing measurable yet: no anchor and an empty surface.
    None,
    /// Heuristic char-estimate of the surface at the anchor.
    Estimated {
        /// Heuristic token count at the anchor point.
        tokens: u64,
    },
    /// Adopted provider usage sample.
    Usage {
        /// Prompt-side pressure the provider reported.
        pressure_tokens: u64,
        /// The full disjoint bucket report behind the sample.
        usage: UsageBuckets,
    },
}

/// Detached request-pressure and surface snapshot at one consumed
/// log revision (dsh `TokenMeasurement` parity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeasurement {
    /// Number of durable events consumed; equal to the next
    /// unread event index.
    pub log_revision: usize,
    /// Baseline the projection anchors against.
    pub baseline: MeasurementBaseline,
    /// Signed heuristic repricing of the current surface relative
    /// to the baseline anchor.
    pub surface_delta_tokens: i64,
    /// Non-negative projected next-request tokens:
    /// `max(0, baseline + surface_delta)`.
    pub total_tokens: u64,
    /// Total heuristic tokens across the current surface.
    pub surface_tokens: u64,
}

/// Heuristic per-message token price used between provider
/// anchors.
///
/// Well-formed `data` payloads decode as
/// `uva_provider::Message` and reuse the workspace's existing
/// char-density estimator (`estimate_messages_token_count`: 4
/// chars per ASCII token, 1.5 per CJK token). Payloads that do
/// not decode (legacy envelopes) fall back to pricing the
/// serialised JSON at the same 4-chars-per-token density, so the
/// meter never prices unknown content at zero.
#[must_use]
pub fn estimate_message_tokens(data: &Value) -> u64 {
    if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
        return estimate_messages_token_count(std::slice::from_ref(&message))
            as u64;
    }
    let serialised = data.to_string();
    (serialised.chars().count().div_ceil(4)) as u64
}

/// Errors the meter's replay can surface.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenMeterError {
    /// The surface fold rejected an event (provenance violation
    /// or out-of-bounds replace range). The failing event remains
    /// unconsumed; a retry re-sees it.
    #[error("surface fold rejected an event: {0}")]
    Fold(#[from] FoldError),
    /// The log passed to [`TokenMeter::observe`] is shorter than
    /// the prefix already folded — the caller truncated or
    /// replaced the durable log mid-replay.
    #[error(
        "durable log shrank between observes: folded {folded} events, got {len}"
    )]
    LogShrank {
        /// Events the meter had already folded.
        folded: usize,
        /// Length of the log slice just passed in.
        len: usize,
    },
}

/// One token-priced node in the current ordered session surface
/// — the heuristic price of one projected message. Only the
/// prices matter to the projection; replace provenance is
/// guarded by the running `max_surface_seq` counter.
type SurfaceNode = u64;

/// Anchor state: where the provider sample was taken and what it
/// said.
#[derive(Clone, Copy, Debug)]
struct MeasurementAnchor {
    /// Surface tokens at the sample (before the assistant
    /// response joined — the surface the request saw).
    surface_tokens: u64,
    /// Adopted or fallback baseline.
    baseline: MeasurementBaseline,
}

/// Pure replay fold over the durable event stream producing
/// usage totals, context pressure, and the projected
/// next-request token count.
///
/// Cheap to drive: hold one meter per session, call
/// [`TokenMeter::observe`] with the full log after every append
/// (only events past the internal cursor are folded), and read
/// [`TokenMeter::measure`] / [`TokenMeter::usage_totals`] /
/// [`TokenMeter::context_pressure`] at any boundary.
///
/// ```
/// use uva_session::{TokenMeter, typed_event_builders::usage};
///
/// let log = vec![usage(120, 30, 150, None, None, None)];
/// let mut meter = TokenMeter::new();
/// meter.observe(&log).expect("fold");
/// assert_eq!(meter.usage_totals().input_tokens, 120);
/// ```
#[derive(Clone, Debug)]
pub struct TokenMeter {
    /// Events folded so far — the replay cursor.
    consumed: usize,
    /// Current surface, one priced node per projected message.
    ///
    /// The fold's own engine, so the meter prices the surface the
    /// fold produces: the same order, the same replace rules, the
    /// same refusals.
    fold: SurfaceFold<SurfaceNode>,
    /// Running heuristic total over the surface's nodes.
    surface_tokens: u64,
    /// Surface tokens just before the latest assistant append —
    /// the surface that assistant's request saw.
    response_boundary_tokens: u64,
    /// Newest adopted (or fallback) anchor.
    anchor: Option<MeasurementAnchor>,
    /// Cumulative disjoint provider usage across the log.
    totals: UsageBuckets,
}

impl Default for TokenMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenMeter {
    /// Build an empty meter (zero events folded).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            consumed: 0,
            fold: SurfaceFold::new(),
            surface_tokens: 0,
            response_boundary_tokens: 0,
            anchor: None,
            totals: UsageBuckets {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        }
    }

    /// Catch the fold up to the current durable tail.
    ///
    /// Pass the **full** log each call; events before the cursor
    /// are skipped. On error the failing event stays unconsumed
    /// and already-folded state is untouched, so the same call
    /// can be retried once the caller has repaired the log.
    pub fn observe(
        &mut self,
        log: &[SessionEvent],
    ) -> Result<(), TokenMeterError> {
        if log.len() < self.consumed {
            return Err(TokenMeterError::LogShrank {
                folded: self.consumed,
                len: log.len(),
            });
        }
        for event in &log[self.consumed..] {
            self.fold_one(event)?;
            self.consumed += 1;
        }
        Ok(())
    }

    /// Measure current request pressure and surface through the
    /// folded tail.
    ///
    /// With an anchor, the projection is
    /// `max(0, anchor_baseline + signed_surface_delta)`. Without
    /// one it degenerates to the full heuristic surface price
    /// (`MeasurementBaseline::None` before any content).
    #[must_use]
    pub fn measure(&self) -> TokenMeasurement {
        let Some(anchor) = &self.anchor else {
            return self.unanchored_measurement();
        };
        let baseline_tokens = match anchor.baseline {
            MeasurementBaseline::Usage {
                pressure_tokens, ..
            } => pressure_tokens,
            MeasurementBaseline::Estimated { tokens } => tokens,
            MeasurementBaseline::None => 0,
        };
        let delta = to_i64(self.surface_tokens) - to_i64(anchor.surface_tokens);
        let total = (to_i64(baseline_tokens) + delta).max(0) as u64;
        TokenMeasurement {
            log_revision: self.consumed,
            baseline: anchor.baseline,
            surface_delta_tokens: delta,
            total_tokens: total,
            surface_tokens: self.surface_tokens,
        }
    }

    /// Cumulative disjoint provider usage across the folded log.
    ///
    /// Every resolvable usage report is added once. The emission
    /// contract is exactly one report per LLM call (the agent
    /// loop forwards the terminal usage frame only), so no
    /// step-level de-duplication is applied; a caller that
    /// replays a log twice into the same meter sees the prefix
    /// skipped by the cursor, not re-added.
    #[must_use]
    pub const fn usage_totals(&self) -> UsageBuckets {
        self.totals
    }

    /// Context-occupancy display projection fed from the newest
    /// adopted sample. `None` until a provider usage sample was
    /// adopted (dsh parity: absent rather than zero).
    #[must_use]
    pub fn context_pressure(
        &self,
        context_window: Option<u64>,
    ) -> Option<ContextPressure> {
        let anchor = self.anchor.as_ref()?;
        let MeasurementBaseline::Usage {
            pressure_tokens, ..
        } = anchor.baseline
        else {
            return None;
        };
        Some(ContextPressure {
            pressure_tokens,
            projected_tokens: self.measure().total_tokens,
            context_window,
        })
    }

    fn unanchored_measurement(&self) -> TokenMeasurement {
        let baseline = if self.surface_tokens == 0 {
            MeasurementBaseline::None
        } else {
            MeasurementBaseline::Estimated {
                tokens: self.surface_tokens,
            }
        };
        TokenMeasurement {
            log_revision: self.consumed,
            baseline,
            surface_delta_tokens: 0,
            total_tokens: self.surface_tokens,
            surface_tokens: self.surface_tokens,
        }
    }

    /// Fold exactly one event; every fallible part is validated
    /// before state mutates.
    fn fold_one(&mut self, event: &SessionEvent) -> Result<(), FoldError> {
        match event {
            SessionEvent::RequestHeader { .. } => {
                // Envelope drift: the anchored sample priced the
                // previous request config, not the new one.
                self.anchor = None;
            }
            SessionEvent::Usage { .. } => self.fold_usage(event),
            _ => {}
        }
        if let Some(data) = event.surface_payload() {
            self.apply_surface(event, data)?;
        }
        Ok(())
    }

    /// Adopt or reject one provider usage sample.
    fn fold_usage(&mut self, event: &SessionEvent) {
        let Some(buckets) = event.provider_usage() else {
            return;
        };
        self.totals.add(&buckets);
        let anchor_surface = self.response_boundary_tokens;
        let baseline = if buckets.prompt_side_tokens() >= anchor_surface {
            MeasurementBaseline::Usage {
                pressure_tokens: buckets.prompt_side_tokens(),
                usage: buckets,
            }
        } else {
            // The sample cannot have priced this surface (stale
            // or mismatched route); keep the conservative
            // heuristic baseline instead of undercounting.
            MeasurementBaseline::Estimated {
                tokens: anchor_surface,
            }
        };
        self.anchor = Some(MeasurementAnchor {
            surface_tokens: anchor_surface,
            baseline,
        });
    }

    /// Apply one surface-eligible event to the priced surface,
    /// mirroring [`crate::surface::fold_surface`]'s append /
    /// replace semantics.
    fn apply_surface(
        &mut self,
        event: &SessionEvent,
        data: &Value,
    ) -> Result<(), FoldError> {
        let seq = event.seq();
        let op = event
            .surface_op()
            .cloned()
            .unwrap_or_else(SurfaceOp::append);
        let tokens = estimate_message_tokens(data);
        let Some((kind, calls)) = row_view(event, data) else {
            // A surface-eligible row this fold cannot place: it is
            // neither priced nor folded, so the meter's figure does
            // not count a row the surface never carried.
            return Ok(());
        };
        if matches!(kind, RowKind::Assistant) && op.is_append() {
            self.response_boundary_tokens = self.surface_tokens;
        }
        let removed = self.fold.apply(SurfaceRow {
            seq,
            kind,
            payload: tokens,
            op,
            calls,
        })?;
        let removed: u64 = removed.iter().sum();
        self.surface_tokens += tokens;
        self.surface_tokens -= removed;
        Ok(())
    }
}

/// Loss-free u64 → i64 for projection arithmetic (saturates at
/// `i64::MAX`, far beyond any token count).
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
