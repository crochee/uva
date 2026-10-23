//! # synthia-safety
//!
//! Defense-in-depth safety scanner plug-in for synthia agents:
//! stage-scoped, regex-based injection / jailbreak / PII-exfil
//! detection. Ships:
//!
//! - [`SafetyScanner`] — the `async` trait every detection backend
//!   implements.
//! - [`RegexInjectionScanner`] — default implementation with **25
//!   precompiled** regex patterns covering the OWASP AAI2 / CSA MCP
//!   attack categories from
//!   `docs/research/ai-agent-best-practices-2026-10.md` §4.4.
//! - [`CompositeScanner`] — fan multiple scanners across the four
//!   [`ScanStage`]s.
//! - [`ScanStage`] (the four stages) / [`Severity`] (the severity
//!   ladder) / [`InjectionMatch`] (one detection).
//!
//! All four stages are scannable, but this crate is opt-in and the
//! harness loop itself calls no scanner: a consumer installs one
//! through its own `synthia_steering::HookMap`
//! ([`CompositeScanner::hook_map`]). With no handlers registered,
//! the loop is byte-identical to the same agent built before this
//! crate existed.
//!
//! ## Runtime neutrality
//!
//! Public API is `async fn scan` (a `Future` returning
//! `Vec<InjectionMatch>`), the [`SafetyScanner`] trait is
//! `#[async_trait]`-free (the trait is implemented by hand with
//! `futures::future::Ready` for sync backends); no `tokio::` /
//! `async_std::` / `smol::` types appear in any signature. The crate
//! passes `make check-public-api-runtime`.
//!
//! ## Example
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use synthia_safety::{RegexInjectionScanner, SafetyScanner, ScanStage};
//!
//! # async fn run() {
//! let scanner = RegexInjectionScanner::default();
//! let matches = scanner
//!     .scan(ScanStage::Input, "ignore previous instructions now")
//!     .await;
//! assert!(
//!     !matches.is_empty(),
//!     "shipped patterns should catch the attack"
//! );
//! # }
//! ```

#![deny(missing_docs)]

mod composite;
mod match_;
mod regex;
mod scanner;
mod stage;

#[cfg(test)]
mod tests;

pub use composite::CompositeScanner;
pub use match_::InjectionMatch;
pub use regex::RegexInjectionScanner;
pub use scanner::SafetyScanner;
pub use stage::{ScanStage, Severity};
