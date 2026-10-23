//! [`PoolConfig`] — knobs the pool exposes over `reqwest`'s
//! built-in connection pool.
//!
//! Defaults (`max_idle_per_host = 8`, `idle_timeout = 90 s`,
//! `connect_timeout = 3 s`, `warm_up = true`) match the
//! plan: an idle budget that lets ~8 concurrent in-flight
//! requests reuse a socket, a generous 90 s idle window so
//! short pauses do not pay a re-handshake, a 3 s connect
//! cap (the failure path is the same `reqwest` already
//! exposes for our adapters), and warm-up on by default so
//! the first user request after boot never pays
//! TCP/TLS.

use std::time::Duration;

/// Default cap on the `reqwest`-side
/// `pool_max_idle_per_host`.
pub const DEFAULT_IDLE_PER_HOST: usize = 8;
/// Default idle window — `reqwest` discards idle sockets
/// after this gap.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Default connect timeout — failure path matches the
/// adapters' own setting.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// [`PoolConfig`] is `Copy` + `Clone` so
/// `AnthropicProvider::with_hpc_pool(cfg)` can chain a single
/// builder call.
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Maps to [`reqwest::ClientBuilder::pool_max_idle_per_host`].
    pub max_idle_per_host: usize,
    /// Maps to [`reqwest::ClientBuilder::pool_idle_timeout`].
    pub idle_timeout: Duration,
    /// Maps to [`reqwest::ClientBuilder::connect_timeout`].
    pub connect_timeout: Duration,
    /// When `true`, [`PooledHttpClient::warm_up`] will be
    /// called once per listed host at boot to seed the
    /// pool.
    pub warm_up: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: DEFAULT_IDLE_PER_HOST,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            warm_up: true,
        }
    }
}

impl PoolConfig {
    /// Disable warm-up explicitly.
    #[must_use]
    pub const fn no_warm_up(mut self) -> Self {
        self.warm_up = false;
        self
    }

    /// Override the per-host idle cap.
    #[must_use]
    pub const fn with_max_idle_per_host(mut self, n: usize) -> Self {
        self.max_idle_per_host = n;
        self
    }

    /// Override the idle window.
    #[must_use]
    pub const fn with_idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    /// Override the connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }
}
