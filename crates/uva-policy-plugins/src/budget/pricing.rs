//! [`PricingTable`] — pluggable cost-per-token table.

use std::{collections::HashMap, sync::Arc};

/// Direction of a token usage row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Input,
    Output,
}

/// `PricingTable` — returns cost in **cents** (USD · 100) for a
/// `(model_id, input_tokens, output_tokens)` triple.
pub trait PricingTable: Send + Sync {
    /// Compute cost in cents. `None` when the model is unpriced;
    /// the wrapper logs `model_unpriced` and skips the cost gate.
    fn cost_cents(
        &self,
        model_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Option<u64>;
}

/// `StaticPricingTable` — a hand-built table. Used for the
/// Anthropic / OpenAI 2026 fixtures shipped with the crate.
#[derive(Clone, Default)]
pub struct StaticPricingTable {
    /// `(model_id, direction) -> cents per 1k tokens`.
    rates: HashMap<(String, Direction), u64>,
}

impl StaticPricingTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a rate in cents-per-1k-tokens for `(model_id, dir)`.
    #[must_use]
    pub fn with_rate(
        mut self,
        model_id: impl Into<String>,
        dir: Direction,
        cents_per_1k: u64,
    ) -> Self {
        self.rates.insert((model_id.into(), dir), cents_per_1k);
        self
    }

    /// Build a fixture table for Anthropic 2026 rates (per 1k
    /// tokens, USD-cents). Values are illustrative and MUST be
    /// refreshed before production deployment.
    #[must_use]
    pub fn anthropic_2026() -> Arc<Self> {
        let t = Self::new()
            .with_rate("claude-4-sonnet", Direction::Input, 300)
            .with_rate("claude-4-sonnet", Direction::Output, 1500)
            .with_rate("claude-3-5-sonnet", Direction::Input, 300)
            .with_rate("claude-3-5-sonnet", Direction::Output, 1500)
            .with_rate("claude-3-haiku", Direction::Input, 25)
            .with_rate("claude-3-haiku", Direction::Output, 125);
        Arc::new(t)
    }

    /// Build a fixture table for OpenAI 2026 rates.
    #[must_use]
    pub fn openai_2026() -> Arc<Self> {
        let t = Self::new()
            .with_rate("gpt-4o", Direction::Input, 250)
            .with_rate("gpt-4o", Direction::Output, 1000)
            .with_rate("gpt-4o-mini", Direction::Input, 15)
            .with_rate("gpt-4o-mini", Direction::Output, 60);
        Arc::new(t)
    }
}

impl PricingTable for StaticPricingTable {
    fn cost_cents(
        &self,
        model_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Option<u64> {
        let in_rate =
            self.rates.get(&(model_id.to_string(), Direction::Input))?;
        let out_rate =
            self.rates.get(&(model_id.to_string(), Direction::Output))?;
        let input = (input_tokens.saturating_mul(*in_rate)) / 1000;
        let output = (output_tokens.saturating_mul(*out_rate)) / 1000;
        Some(input.saturating_add(output))
    }
}
