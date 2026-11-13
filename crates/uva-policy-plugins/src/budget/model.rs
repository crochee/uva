//! Budget types.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use uva_core::Clock;

use crate::budget::pricing::PricingTable;

/// What kind of budget a [`BudgetExceeded`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    Tokens,
    Latency,
    Cost,
}

/// `Budget` is the composition of three independent budgets.
pub struct Budget {
    /// Token-count budget.
    pub tokens: Option<TokenBudget>,
    /// Wall-clock budget.
    pub latency: Option<LatencyBudget>,
    /// USD-cost budget.
    pub cost: Option<CostBudget>,
    /// Outcome when a budget trips.
    pub on_exceeded: BudgetOutcome,
}

impl Clone for Budget {
    fn clone(&self) -> Self {
        // CostBudget is `!Clone` (the `used_cents` AtomicU64 is
        // clone-able, but `pricing: Arc<dyn PricingTable>` would
        // require cloning the trait object — not worth it). Build a
        // fresh `CostBudget` that mirrors the limit and pricing but
        // resets the counter to 0; callers that need the running
        // total should read it before cloning.
        let cost = self.cost.as_ref().map(|c| CostBudget {
            used_cents: AtomicU64::new(0),
            limit_cents: c.limit_cents,
            pricing: c.pricing.clone(),
        });
        Budget {
            tokens: self.tokens.clone(),
            latency: self.latency.clone(),
            cost,
            on_exceeded: self.on_exceeded,
        }
    }
}

impl std::fmt::Debug for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("tokens", &self.tokens.as_ref().map(|t| t.used()))
            .field("latency", &self.latency.as_ref().map(|l| l.limit))
            .field("cost", &self.cost.as_ref().map(|c| c.used_cents()))
            .field("on_exceeded", &self.on_exceeded)
            .finish()
    }
}

/// Outcome when a budget trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetOutcome {
    /// Soft: emit a `Tool` error (model-readable) and let the run
    /// finish naturally.
    #[default]
    SoftFinalize,
    /// Hard: return an error and abort the call.
    HardStop,
}

impl Budget {
    /// Builder.
    #[must_use]
    pub fn builder() -> BudgetBuilder {
        BudgetBuilder::default()
    }
}

/// Builder for [`Budget`].
#[derive(Default)]
pub struct BudgetBuilder {
    tokens: Option<TokenBudget>,
    latency: Option<LatencyBudget>,
    cost: Option<CostBudget>,
    on_exceeded: BudgetOutcome,
}

impl BudgetBuilder {
    #[must_use]
    pub fn tokens(mut self, t: TokenBudget) -> Self {
        self.tokens = Some(t);
        self
    }

    #[must_use]
    pub fn latency(mut self, l: LatencyBudget) -> Self {
        self.latency = Some(l);
        self
    }

    #[must_use]
    pub fn cost(mut self, c: CostBudget) -> Self {
        self.cost = Some(c);
        self
    }

    #[must_use]
    pub fn on_exceeded(mut self, o: BudgetOutcome) -> Self {
        self.on_exceeded = o;
        self
    }

    #[must_use]
    pub fn build(self) -> Budget {
        Budget {
            tokens: self.tokens,
            latency: self.latency,
            cost: self.cost,
            on_exceeded: self.on_exceeded,
        }
    }
}

/// Token budget. `used` increments atomically.
pub struct TokenBudget {
    /// Atomic counter — hot-path, no allocation.
    pub used: AtomicU64,
    /// Hard limit.
    pub limit: u64,
    /// Pluggable source — by default the budget owns a fresh
    /// atomic, but `uva_session::TokenMeter` plugs in via the
    /// [`TokenSource`] trait.
    pub source: Arc<dyn TokenSource>,
}

impl Clone for TokenBudget {
    fn clone(&self) -> Self {
        // The counter resets on clone (callers that need the
        // running total should read it before cloning); the
        // source and limit round-trip.
        Self {
            used: AtomicU64::new(0),
            limit: self.limit,
            source: self.source.clone(),
        }
    }
}

/// `TokenSource` — pluggable source for the "used tokens" count.
pub trait TokenSource: Send + Sync {
    /// Current cumulative token count.
    fn current(&self) -> u64;
    /// Increment by `delta` (e.g. after a successful provider call
    /// that reported `usage.output_tokens`).
    fn add(&self, delta: u64);
}

/// Built-in `TokenSource` impl backed by an `AtomicU64`. The
/// default; production code plugs `uva_session::TokenMeter` via
/// a wrapper that implements this trait.
pub struct AtomicTokenSource {
    inner: AtomicU64,
}

impl AtomicTokenSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: AtomicU64::new(0),
        }
    }
}

impl Default for AtomicTokenSource {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenSource for AtomicTokenSource {
    fn current(&self) -> u64 {
        self.inner.load(Ordering::SeqCst)
    }

    fn add(&self, delta: u64) {
        self.inner.fetch_add(delta, Ordering::SeqCst);
    }
}

impl TokenBudget {
    /// Build a fresh token budget backed by an
    /// [`AtomicTokenSource`].
    #[must_use]
    pub fn new(limit: u64) -> Self {
        Self {
            used: AtomicU64::new(0),
            limit,
            source: Arc::new(AtomicTokenSource::new()),
        }
    }

    /// Build with a custom [`TokenSource`].
    #[must_use]
    pub fn with_source(limit: u64, source: Arc<dyn TokenSource>) -> Self {
        Self {
            used: AtomicU64::new(0),
            limit,
            source,
        }
    }

    /// Current used count.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.source.current()
    }

    /// Add to the source.
    pub fn add(&self, delta: u64) {
        self.source.add(delta);
    }

    /// True when the budget has tripped.
    #[must_use]
    pub fn exceeded(&self) -> bool {
        self.source.current() >= self.limit
    }
}

/// Wall-clock latency budget. `started_at` is captured by
/// [`LatencyBudget::new`]; tests inject a [`Clock`] to advance
/// time deterministically.
#[derive(Clone)]
pub struct LatencyBudget {
    pub started_at: DateTime<Utc>,
    pub limit: Duration,
    pub clock: Arc<dyn Clock>,
}

impl LatencyBudget {
    #[must_use]
    pub fn new(limit: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            started_at: clock.now(),
            limit,
            clock,
        }
    }

    /// Reset the start instant to `now`. Use after a successful
    /// provider call to give the next call a fresh window.
    pub fn reset(&mut self) {
        self.started_at = self.clock.now();
    }

    /// True when the elapsed time has reached the limit.
    #[must_use]
    pub fn exceeded(&self) -> bool {
        let now = self.clock.now();
        let elapsed = now.signed_duration_since(self.started_at);
        let chrono_limit = chrono::Duration::from_std(self.limit)
            .unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX));
        elapsed >= chrono_limit
    }

    /// Elapsed since start.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        let now = self.clock.now();
        let elapsed = now.signed_duration_since(self.started_at);
        elapsed.to_std().unwrap_or(Duration::ZERO)
    }
}

/// USD cost budget. Costs are tracked in **cents** (USD · 100) to
/// avoid float arithmetic on the hot path.
pub struct CostBudget {
    pub used_cents: AtomicU64,
    pub limit_cents: u64,
    pub pricing: Arc<dyn PricingTable>,
}

impl CostBudget {
    #[must_use]
    pub fn new(limit_cents: u64, pricing: Arc<dyn PricingTable>) -> Self {
        Self {
            used_cents: AtomicU64::new(0),
            limit_cents,
            pricing,
        }
    }

    /// Used cents.
    #[must_use]
    pub fn used_cents(&self) -> u64 {
        self.used_cents.load(Ordering::SeqCst)
    }

    /// Add the cost of `usage` for the given `model_id`.
    pub fn record(
        &self,
        model_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        if let Some(c) =
            self.pricing
                .cost_cents(model_id, input_tokens, output_tokens)
        {
            self.used_cents.fetch_add(c, Ordering::SeqCst);
        } else {
            tracing::warn!(
                model = model_id,
                "model_unpriced: skipping cost gate"
            );
        }
    }

    /// True when the cost gate has tripped.
    #[must_use]
    pub fn exceeded(&self) -> bool {
        self.used_cents.load(Ordering::SeqCst) >= self.limit_cents
    }
}

/// `BudgetExceeded` event payload — what the harness reads when
/// the wrapper reports the trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub kind: BudgetKind,
    pub used: u64,
    pub limit: u64,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "budget exceeded (kind={:?}, used={}, limit={})",
            self.kind, self.used, self.limit
        )
    }
}
