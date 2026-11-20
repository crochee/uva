//! The Prometheus RED counters the HTTP surface feeds.
//!
//! The families themselves live in `uva-telemetry`
//! (`HTTP_REQUESTS_TOTAL` / `HTTP_REQUESTS_DURATION_SECONDS`); this
//! module is the one place that writes them, so the label shape has
//! one owner: the request method, and the **matched route template**
//! — never the raw URI.
//!
//! Why the template and not the URI: the router's paths carry ids
//! (`/api/v1/chat/sessions/{id}/messages`), and the raw URI would
//! mint one time series per id. The matched template collapses them
//! into one.
//!
//! Why a constant for the unmatched case: the static front-end is
//! the router's **fallback**, so every client route
//! (`/chat/<uuid>`) matches no route and lands here. One series for
//! all of them is the point — the raw path would mint a series per
//! session.

use std::time::Instant;

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};

/// The path label for a request that matched no route.
///
/// The front-end's fallback answers every client route, so this one
/// label stands for all of them.
const UNMATCHED_PATH: &str = "unmatched";

/// Record one HTTP request: the counter, then the duration.
///
/// The request is measured around the inner service, so the
/// histogram says how long the request took, not how long the
/// bookkeeping took. Recording happens after the response is built,
/// which is why a cancelled request never lands in the counter.
pub(super) async fn track_request(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| UNMATCHED_PATH.to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed().as_secs_f64();
    uva::telemetry::HTTP_REQUESTS_TOTAL
        .with_label_values(&[method.as_str(), path.as_str()])
        .inc();
    uva::telemetry::HTTP_REQUESTS_DURATION_SECONDS
        .with_label_values(&[method.as_str(), path.as_str()])
        .observe(elapsed);
    response
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, http::Request, routing::get};
    use tower::{Layer as _, ServiceExt as _};

    use super::{UNMATCHED_PATH, track_request};

    /// One counter reading, for one label pair.
    fn counter_value(method: &str, path: &str) -> u64 {
        uva::telemetry::HTTP_REQUESTS_TOTAL
            .with_label_values(&[method, path])
            .get()
    }

    /// The histogram's sample count, for one label pair.
    fn duration_samples(method: &str, path: &str) -> u64 {
        uva::telemetry::HTTP_REQUESTS_DURATION_SECONDS
            .with_label_values(&[method, path])
            .get_sample_count()
    }

    /// A router with one route and one fallback, both behind the
    /// middleware.
    fn app() -> Router {
        Router::new()
            .route("/readyz", get(|| async { "ok" }))
            .fallback(|| async { "spa" })
            .layer(axum::middleware::from_fn(track_request))
    }

    #[tokio::test]
    async fn a_matched_route_is_counted_under_its_template() {
        let before = counter_value("GET", "/readyz");
        let samples = duration_samples("GET", "/readyz");

        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            counter_value("GET", "/readyz"),
            before + 1,
            "the matched route's counter moved"
        );
        assert_eq!(
            duration_samples("GET", "/readyz"),
            samples + 1,
            "the matched route's histogram observed the request"
        );
    }

    #[tokio::test]
    async fn an_unmatched_request_is_counted_under_one_label() {
        let before = counter_value("GET", UNMATCHED_PATH);

        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/chat/session-42/agent/agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            counter_value("GET", UNMATCHED_PATH),
            before + 1,
            "the fallback's requests share one series"
        );
        assert_eq!(
            counter_value("GET", "/chat/session-42/agent/agent"),
            0,
            "the raw URI is never a label: it would mint a series \
             per session"
        );
    }

    #[tokio::test]
    async fn the_static_service_the_front_end_is_served_from_is_counted() {
        // The front-end is served by a **service**, not a handler,
        // and a service sits outside the router's layer stack — so
        // the middleware has to be put around it by hand. Without
        // that, the front-end's requests would be invisible.
        //
        // `HEAD` keeps this test's label pair its own: the counters
        // are process-wide, and the other test in this module reads
        // the same "unmatched" series.
        let before = counter_value("HEAD", UNMATCHED_PATH);
        let directory = tempfile::tempdir().expect("a temp directory");
        std::fs::write(directory.path().join("index.html"), "<html></html>")
            .expect("the index file");
        let files = tower_http::services::ServeDir::new(directory.path())
            .fallback(tower_http::services::ServeFile::new(
                directory.path().join("index.html"),
            ));
        let counted = axum::middleware::from_fn(track_request).layer(files);
        let app = Router::new().fallback_service(counted);

        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/chat/session-42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(
            counter_value("HEAD", UNMATCHED_PATH),
            before + 1,
            "the static service's requests are counted too"
        );
    }
}
