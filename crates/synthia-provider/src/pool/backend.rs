//! [`HttpClientBackend`] — runtime-neutral seam over the
//! HTTP client. [`PooledHttpClient`] wraps an
//! `Arc<dyn HttpClientBackend>` and exposes the three
//! seams a production adapter wants (per-host cap,
//! warm-up, observable stats) without taking a runtime
//! dependency.
//!
//! Two backends ship in this module:
//!
//! - [`ReqwestBackend`] — wraps a `reqwest::Client`; the
//!   default backend, the only one the adapters will ever
//!   need.
//! - `PooledHttpClient` — the wrapper that the adapters
//!   store as their `client` field; the pool's perf gains
//!   live behind it.

use std::sync::Arc;

use parking_lot::Mutex;
use reqwest::{Client, Request, Response};
use tracing::{debug, info};
use url::Url;

use super::{PoolConfig, PoolStats};

/// Runtime-neutral HTTP backend. The pool only sees this
/// trait — concrete implementations encapsulate the
/// `reqwest::Client` (or a mock for tests).
#[async_trait::async_trait]
pub trait HttpClientBackend: Send + Sync {
    /// Issue `req` against the underlying client.
    async fn execute(&self, req: Request) -> Result<Response, reqwest::Error>;

    /// Cheap identifier for tracing — `reqwest`, `mock`,
    /// …
    fn label(&self) -> &'static str;

    /// Downcast hook so the pool can reach into the
    /// concrete backend for capabilities it cannot drive
    /// through the trait (warm-up needs the inner
    /// `reqwest::Client` to build a `HEAD` request). The
    /// default returns `None`; only backends that need
    /// to be downcastable override it.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

/// `reqwest`-backed [`HttpClientBackend`]. This is the
/// only backend the adapters construct in production;
/// tests can swap it for a recording fake.
pub struct ReqwestBackend {
    client: Client,
    label: &'static str,
}

impl ReqwestBackend {
    /// Wrap an already-built [`Client`].
    pub fn new(client: Client) -> Self {
        Self {
            client,
            label: "reqwest",
        }
    }

    /// Build a fresh [`Client`] configured with the pool's
    /// defaults (`cfg.warm_up` is honoured later by
    /// [`PooledHttpClient::warm_up`], not here).
    pub fn from_config(cfg: PoolConfig) -> Self {
        let client = Client::builder()
            .pool_max_idle_per_host(cfg.max_idle_per_host)
            .pool_idle_timeout(cfg.idle_timeout)
            .connect_timeout(cfg.connect_timeout)
            .build()
            .expect("reqwest client build is infallible under our config");
        Self::new(client)
    }

    /// Borrow the inner [`Client`] for ad-hoc requests
    /// (warm-up itself, mostly).
    pub fn client(&self) -> &Client {
        &self.client
    }
}

#[async_trait::async_trait]
impl HttpClientBackend for ReqwestBackend {
    async fn execute(&self, req: Request) -> Result<Response, reqwest::Error> {
        self.client.execute(req).await
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

/// The wrapper the adapters actually store. Behaviour is
/// `Arc<dyn HttpClientBackend>` plus an in-memory stat
/// counter; the underlying [`Client`] carries the keep-alive
/// pool, so the wrapper itself adds no extra caching.
///
/// Construction is one of:
///
/// - [`PooledHttpClient::new`] — wrap an
///   already-configured backend.
/// - [`PooledHttpClient::with_config`] — build a
///   [`ReqwestBackend`] from a [`PoolConfig`].
/// - [`PooledHttpClient::from_reqwest`] — wrap a raw
///   [`Client`] under the default policy.
pub struct PooledHttpClient {
    backend: Arc<dyn HttpClientBackend>,
    stats: Mutex<PoolStats>,
    label: &'static str,
}

impl PooledHttpClient {
    /// Wrap an existing backend.
    pub fn new(backend: Arc<dyn HttpClientBackend>) -> Self {
        let label = backend.label();
        Self {
            backend,
            stats: Mutex::new(PoolStats::default()),
            label,
        }
    }

    /// Build a [`ReqwestBackend`] under `cfg` and wrap it.
    pub fn with_config(cfg: PoolConfig) -> Self {
        Self::new(Arc::new(ReqwestBackend::from_config(cfg)))
    }

    /// Wrap a raw [`Client`] under [`PoolConfig::default`].
    pub fn from_reqwest(client: Client) -> Self {
        Self::new(Arc::new(ReqwestBackend::new(client)))
    }

    /// Issue `req` against the wrapped backend, then
    /// credit the stats counter. We treat any
    /// non-connect-error response as a successful socket
    /// use — the network stack decides whether the socket
    /// was reused (counted as `reused`) or freshly
    /// established (counted as `established`); from this
    /// side of the API we cannot distinguish. We optimistically
    /// count `reused`; future per-socket instrumentation
    /// will split the two counts.
    pub async fn execute(
        &self,
        req: Request,
    ) -> Result<Response, reqwest::Error> {
        let res = self.backend.execute(req).await;
        {
            let mut s = self.stats.lock();
            match &res {
                Ok(_) => {
                    s.reused = s.reused.saturating_add(1);
                }
                Err(_) => {
                    s.established = s.established.saturating_add(1);
                }
            }
        }
        res
    }

    /// Snapshot the live counters. Lock is held only for
    /// the duration of the copy.
    pub fn stats(&self) -> PoolStats {
        *self.stats.lock()
    }

    /// Label string for tracing.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Borrow the inner backend (used by adapter-level
    /// warm-ups that need to issue a `HEAD /`).
    pub fn backend(&self) -> Arc<dyn HttpClientBackend> {
        Arc::clone(&self.backend)
    }

    /// Issue a `HEAD /` against every host in `hosts` to
    /// seed the keep-alive pool before the first user
    /// request pays a TCP/TLS handshake.
    ///
    /// Warm-up issues the request through the wrapped
    /// backend directly — the only place we touch the
    /// inner client. Test backends that do not implement
    /// the `Any` downcast silently skip the warm-up; the
    /// caller still gets a working pool.
    ///
    /// Returns the number of warm-up requests that
    /// completed (failed warm-ups do not abort the boot —
    /// the pool still works at first user request, it
    /// just pays the handshake that one time).
    pub async fn warm_up(&self, hosts: &[Url]) -> usize {
        let n = hosts.len();
        if n == 0 {
            return 0;
        }
        info!(
            backend = self.label,
            hosts = n,
            "PooledHttpClient::warm_up: seeding pool"
        );
        // Downcast to `ReqwestBackend` to call its
        // `client()`. Test fakes that are not
        // `ReqwestBackend` are simply skipped — the
        // pool still works, just without warm-up.
        let reqwest_backend = self
            .backend
            .as_any()
            .and_then(|a| a.downcast_ref::<ReqwestBackend>());
        let inner = match reqwest_backend {
            Some(b) => b.client().clone(),
            None => return 0,
        };
        warm_up_with(&inner, hosts).await
    }
}

/// Issue a `HEAD /` against every host using the supplied
/// `client`. Free function so non-`PooledHttpClient`
/// adapters (the Anthropic / OpenAI providers when they
/// store a raw `reqwest::Client` configured with the
/// pool knobs) can also seed the pool.
pub async fn warm_up_with(client: &Client, hosts: &[Url]) -> usize {
    let n = hosts.len();
    if n == 0 {
        return 0;
    }
    let mut completed = 0_usize;
    for url in hosts {
        let origin = match url.origin() {
            url::Origin::Opaque(_) => continue,
            o => o,
        };
        let head_url = match Url::parse(&origin.ascii_serialization()) {
            Ok(u) => u,
            Err(_) => url.clone(),
        };
        let req = match client.head(head_url).build() {
            Ok(r) => r,
            Err(_) => continue,
        };
        if client.execute(req).await.is_ok() {
            completed = completed.saturating_add(1);
        }
        debug!(
            host = %url,
            "warm_up_with: head request done"
        );
    }
    completed
}
