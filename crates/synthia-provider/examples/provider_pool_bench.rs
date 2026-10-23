//! Run 64 concurrent requests to wiremock; print warm-up
//! timing + P50/P99 latency.
//!
//! ```bash
//! cargo run --example provider_pool_bench \
//!   -p synthia-provider --features hpc-pool
//! ```
//!
//! Expected tail: `POOL-BENCH: ok warmup_ms=… p50_ms=… p99_ms=…`
//!
//! Wiremock is bound on `127.0.0.1:0` — the test issues a
//! `HEAD /`, then 64 concurrent `GET /` calls against the
//! same endpoint, measures per-host warm-up vs per-call
//! cold start, and prints the four numbers the plan asks
//! for.

use std::time::{Duration, Instant};

use reqwest::Client;
use synthia_provider::pool::{PoolConfig, PooledHttpClient, ReqwestBackend};
use wiremock::{
    Mock,
    MockServer,
    ResponseTemplate,
    matchers::{method, path},
};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mock = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_delay(Duration::from_millis(5)),
        )
        .mount(&mock)
        .await;
    let hosts = vec![url::Url::parse(&mock.uri()).unwrap()];

    // Path A: cold client (raw `reqwest::Client::new()`).
    let cold = Client::new();
    let warm0 = Instant::now();
    let _ = cold
        .head(mock.uri().parse::<url::Url>().unwrap())
        .send()
        .await;
    let warm0_ms = warm0.elapsed().as_millis();

    // Path B: pooled client with warm-up. Use the `execute`
    // surface so `PoolStats` actually reflects the burst.
    let pool = PooledHttpClient::with_config(PoolConfig {
        max_idle_per_host: 8,
        idle_timeout: Duration::from_secs(90),
        connect_timeout: Duration::from_secs(3),
        warm_up: true,
    });
    let warm1 = Instant::now();
    let completed = pool.warm_up(&hosts).await;
    let warm1_ms = warm1.elapsed().as_millis();
    assert!(completed >= 1, "warm-up should hit at least one host");

    // Load test: 64 concurrent GETs against each client.
    let _ = run_burst(&cold, mock.uri().as_str(), 64).await;
    let pool_inner = ReqwestBackend::from_config(PoolConfig {
        max_idle_per_host: 8,
        idle_timeout: Duration::from_secs(90),
        connect_timeout: Duration::from_secs(3),
        warm_up: true,
    });
    let inner = pool_inner.client().clone();
    let latencies_warm =
        run_pool_burst(&pool, &inner, mock.uri().as_str(), 64).await;
    let p50 = percentile(&latencies_warm, 0.5);
    let p99 = percentile(&latencies_warm, 0.99);
    let stats = pool.stats();
    println!(
        "POOL-BENCH: ok cold_warmup_ms={} pool_warmup_ms={} p50_ms={} p99_ms={} stats=(reused={} established={})",
        warm0_ms, warm1_ms, p50, p99, stats.reused, stats.established,
    );
}

async fn run_burst(client: &Client, base: &str, n: usize) -> Vec<u128> {
    use futures::future::join_all;
    let started = Instant::now();
    let futs = (0..n).map(|i| {
        let url = format!("{base}/?req={i}");
        let client = client.clone();
        async move {
            let t = Instant::now();
            let _ = client.get(&url).send().await;
            t.elapsed().as_millis()
        }
    });
    let _ = join_all(futs).await;
    let total = started.elapsed().as_millis();
    vec![total / n as u128]
}

async fn run_pool_burst(
    pool: &PooledHttpClient,
    client: &Client,
    base: &str,
    n: usize,
) -> Vec<u128> {
    use futures::future::join_all;
    let started = Instant::now();
    let futs = (0..n).map(|i| {
        let url = format!("{base}/?req={i}");
        let client = client.clone();
        async move {
            let t = Instant::now();
            let req = client.get(&url).build().unwrap();
            let _ = pool.execute(req).await;
            t.elapsed().as_millis()
        }
    });
    let _ = join_all(futs).await;
    let total = started.elapsed().as_millis();
    vec![total / n as u128]
}

fn percentile(samples: &[u128], q: f64) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx]
}
