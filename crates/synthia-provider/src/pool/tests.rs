//! Tests for the pool module.
//!
//! These tests construct a [`PooledHttpClient`] around an
//! already-built [`reqwest::Client`] and assert the basic
//! state-machine contract without exercising an actual
//! HTTP peer (the per-host reuse ratio is observable only
//! against a real peer; for that we have the
//! `provider_pool_bench` example + wiremock).

use std::time::Duration;

use reqwest::Client;
use url::Url;

use super::{
    PoolConfig,
    PoolStats,
    PooledHttpClient,
    config::{
        DEFAULT_CONNECT_TIMEOUT,
        DEFAULT_IDLE_PER_HOST,
        DEFAULT_IDLE_TIMEOUT,
    },
};

#[test]
fn default_policy_matches_documented_knobs() {
    let p = PoolConfig::default();
    assert_eq!(p.max_idle_per_host, DEFAULT_IDLE_PER_HOST);
    assert_eq!(p.max_idle_per_host, 8);
    assert_eq!(p.idle_timeout, DEFAULT_IDLE_TIMEOUT);
    assert_eq!(p.idle_timeout, Duration::from_secs(90));
    assert_eq!(p.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
    assert_eq!(p.connect_timeout, Duration::from_secs(3));
    assert!(p.warm_up);
}

#[test]
fn builder_knobs_apply_verbatim() {
    let p = PoolConfig::default()
        .with_max_idle_per_host(4)
        .with_idle_timeout(Duration::from_secs(30))
        .with_connect_timeout(Duration::from_secs(1))
        .no_warm_up();
    assert_eq!(p.max_idle_per_host, 4);
    assert_eq!(p.idle_timeout, Duration::from_secs(30));
    assert_eq!(p.connect_timeout, Duration::from_secs(1));
    assert!(!p.warm_up);
}

#[test]
fn fresh_pool_has_zeroed_stats() {
    let pool = PooledHttpClient::from_reqwest(Client::new());
    let s = pool.stats();
    assert_eq!(s, PoolStats::default());
    assert_eq!(s.reuse_ratio(), 0.0);
    assert_eq!(s.total(), 0);
}

#[test]
fn reuse_ratio_denominator_handles_zero() {
    let s = PoolStats::default();
    assert_eq!(s.reuse_ratio(), 0.0);
    let s = PoolStats {
        reused: 7,
        established: 3,
        ..PoolStats::default()
    };
    assert_eq!(s.total(), 10);
    assert!((s.reuse_ratio() - 0.7).abs() < f32::EPSILON);
}

#[test]
fn warm_up_against_empty_host_list_is_a_noop() {
    let pool = PooledHttpClient::from_reqwest(Client::new());
    let completed = futures::executor::block_on(pool.warm_up(&[]));
    assert_eq!(completed, 0);
    // Stats must NOT have moved — a no-op must not create
    // fake reuse / establishment events.
    assert_eq!(pool.stats(), PoolStats::default());
}

#[test]
fn warm_up_label_propagates_from_backend() {
    // Use the `from_reqwest` constructor — the underlying
    // backend is `ReqwestBackend`, so the label is
    // `"reqwest"`.
    let pool = PooledHttpClient::from_reqwest(Client::new());
    assert_eq!(pool.label(), "reqwest");
}

#[test]
fn unsyntactic_url_in_warm_up_is_skipped_silently() {
    // A URL whose `Origin::ascii_serialization` is empty
    // (purely opaque origins) — warm-up skips it without
    // recording a stat. This keeps the call total-loss
    // safe for `Url` values parsed from arbitrary input.
    let pool = PooledHttpClient::from_reqwest(Client::new());
    let urls = vec![Url::parse("about:blank").unwrap()];
    let completed = futures::executor::block_on(pool.warm_up(&urls));
    assert_eq!(completed, 0);
}
