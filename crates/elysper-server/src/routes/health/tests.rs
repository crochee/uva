use super::*;

/// A hermetic `AppState` for route tests.
///
/// `AppState::new` loads the workspace configuration and builds a
/// real provider, so it fails without ambient provider
/// credentials by design (`WorkspaceConfig::load_from_dir` refuses
/// to start a server with no provider). Route behaviour has
/// nothing to do with which provider is configured, so these tests
/// build the state over in-memory components and a `FakeProvider`
/// instead — with a temp workspace whose lifetime the caller
/// keeps.
async fn test_state()
-> (std::sync::Arc<crate::state::AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = elysper::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state =
        crate::state::AppState::for_test(sessions, dir.path().to_path_buf())
            .await;
    (std::sync::Arc::new(state), dir)
}

#[test]
fn model_entry_serializes_all_fields() {
    let entry = ModelEntry {
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        context_window: 8192,
        supports_tools: true,
        supports_streaming: false,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["provider"], "openai");
    assert_eq!(json["model"], "gpt-4");
    assert_eq!(json["context_window"], 8192);
    assert_eq!(json["supports_tools"], true);
    assert_eq!(json["supports_streaming"], false);
}

#[test]
fn models_response_wraps_entries_with_defaults() {
    let resp = ModelsResponse {
        models: vec![],
        default_provider: "anthropic".to_string(),
        default_model: "claude-opus-4".to_string(),
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["models"].as_array().unwrap().len(), 0);
    assert_eq!(json["default_provider"], "anthropic");
    assert_eq!(json["default_model"], "claude-opus-4");
}

/// A minimal `/api/models` body for ETag tests.
fn body_for(default_model: &str, provider_model: &str) -> ModelsResponse {
    ModelsResponse {
        models: vec![ModelEntry {
            provider: "openai".to_string(),
            model: provider_model.to_string(),
            context_window: 128_000,
            supports_tools: true,
            supports_streaming: true,
        }],
        default_provider: "openai".to_string(),
        default_model: default_model.to_string(),
    }
}

#[test]
fn models_etag_is_stable_per_default_model() {
    // Same body → same output, byte-for-byte.
    assert_eq!(
        models_etag(&body_for("gpt-4", "gpt-4")),
        models_etag(&body_for("gpt-4", "gpt-4"))
    );
}

#[test]
fn models_etag_differs_when_default_model_changes() {
    assert_ne!(
        models_etag(&body_for("gpt-4", "gpt-4")),
        models_etag(&body_for("claude-opus-4", "gpt-4")),
        "changing the default model must invalidate the ETag"
    );
}

/// The tag hashes the whole body, so a provider entry that
/// moves while `default_model` stays put still invalidates the
/// client. Folding `default_model` alone (the old shape) let a
/// redeploy onto a different provider set keep answering `304`.
#[test]
fn models_etag_covers_providers_and_default_provider() {
    let base = body_for("gpt-4", "gpt-4");

    let mut other_model = base.clone();
    other_model.models[0].model = "gpt-4o".to_string();
    assert_ne!(
        models_etag(&base),
        models_etag(&other_model),
        "changing a provider entry must invalidate the ETag"
    );

    let mut other_provider = base.clone();
    other_provider.default_provider = "anthropic".to_string();
    assert_ne!(
        models_etag(&base),
        models_etag(&other_provider),
        "changing the default provider must invalidate the ETag"
    );
}

#[test]
fn models_etag_embeds_package_version() {
    // The binary version is a baked-in constant, so the tag
    // must include it as a deploy-busting component.
    let tag = models_etag(&body_for("gpt-4", "gpt-4"));
    assert!(
        tag.contains(env!("CARGO_PKG_VERSION")),
        "ETag must include CARGO_PKG_VERSION, got: {tag}"
    );
}

/// `If-None-Match` is a comma-separated list of entity-tags,
/// each of which may carry the `W/` prefix, and `*` selects any
/// current representation. Only such a selection may answer
/// `304`; anything else leaves the precondition unmet.
#[test]
fn if_none_match_selects_only_on_matching_tag() {
    let etag = models_etag(&body_for("gpt-4", "gpt-4"));
    let strong = etag.strip_prefix("W/").expect("weak prefix");

    for (value, expected) in [
        (etag.as_str(), true),
        (strong, true),
        ("*", true),
        ("* , W/\"other\"", true),
        ("W/\"other\", W/\"other-2\"", false),
        ("W/\"other\"", false),
        ("", false),
    ] {
        let mut req = HeaderMap::new();
        req.insert(IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
        assert_eq!(
            if_none_match_selects(&req, &etag),
            expected,
            "If-None-Match: {value:?}"
        );
    }

    // No header at all is not a selection either.
    assert!(!if_none_match_selects(&HeaderMap::new(), &etag));
}

/// The cache key is the body's ETag, not the default model
/// name: a body whose provider set changed while
/// `default_model` did not must never be answered from the
/// previous entry — that collision is what served a second
/// `AppState` the first state's models.
#[test]
fn models_cache_keys_on_body_content() {
    let base = body_for("gpt-4", "gpt-4");
    let mut swapped = base.clone();
    swapped.models[0].model = "gpt-4o".to_string();

    let first = cached_models(&base);
    let second = cached_models(&swapped);
    assert_eq!(first.etag, models_etag(&base));
    assert_eq!(second.etag, models_etag(&swapped));
    assert_ne!(first.body, second.body);

    // Equal bodies keep hitting the same key, whatever a
    // parallel test did to the single-entry cache meanwhile.
    assert_eq!(cached_models(&base).body, first.body);
}

/// `list_models` with no `If-None-Match` MUST return 200 with
/// the full JSON body and the new ETag + Cache-Control
/// headers. Pinning the wire shape here guards against a
/// missing-header-propagation regression.
#[tokio::test]
async fn list_models_emits_etag_and_cache_control() {
    let (state, _dir) = test_state().await;
    let expected_default = state.workspace_config.default_model.clone();
    let req = axum::http::HeaderMap::new();
    let resp = list_models(State(state), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(CACHE_CONTROL).unwrap(),
        "no-cache, max-age=60"
    );
    assert_eq!(
        resp.headers().get(CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let etag = resp.headers().get(ETAG).unwrap().to_str().unwrap();
    assert!(etag.starts_with("W/\"models-"), "ETag shape: {etag}");
    assert!(
        etag.contains(env!("CARGO_PKG_VERSION")),
        "ETag must embed CARGO_PKG_VERSION: {etag}"
    );

    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["models"].is_array(), "got: {body}");
    assert_eq!(body["default_model"], expected_default.as_str());
}

/// Conditional GET MUST compare rather than short-circuit: the
/// tag the handler itself just emitted revalidates to an empty
/// `304`, while any other tag is served the full `200`. The
/// handler used to answer `304` to *any* `If-None-Match`, which
/// — on top of `Cache-Control: no-cache` — pinned a browser
/// that had cached one model list to it forever.
#[tokio::test]
async fn list_models_returns_304_only_for_the_current_etag() {
    let (state, _dir) = test_state().await;

    let first = list_models(State(state.clone()), HeaderMap::new()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers().get(ETAG).unwrap().clone();
    let strong = {
        let tag = etag.to_str().unwrap();
        let tag = tag.strip_prefix("W/").unwrap_or(tag);
        HeaderValue::from_str(tag).unwrap()
    };

    for value in [etag, strong, HeaderValue::from_static("*")] {
        let mut req = HeaderMap::new();
        req.insert(IF_NONE_MATCH, value.clone());
        let resp = list_models(State(state.clone()), req).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_MODIFIED,
            "If-None-Match: {value:?} must revalidate"
        );
        // Body MUST be empty on a 304 (RFC 9110 §15.4.5).
        // `Body::size_hint` returns an upper bound; an empty
        // body reports exact size 0 via the `SizeHint` struct.
        use axum::body::HttpBody;
        let size = resp.into_body().size_hint().exact();
        assert_eq!(size, Some(0), "304 response body must be empty");
    }

    // A tag for a different representation is not a match: the
    // client gets the body, not a stale 304.
    let mut req = HeaderMap::new();
    req.insert(
        IF_NONE_MATCH,
        HeaderValue::from_static("W/\"models-test-irrelevant\""),
    );
    let resp = list_models(State(state), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
        "a mismatched If-None-Match must be served the body"
    );
}

/// `livez` MUST return 200 with `status: "ok"` and
/// `Cache-Control: no-store` — liveness is a process-alive
/// fact, so a cached verdict would outlive a crash-restart.
#[tokio::test]
async fn livez_emits_ok_and_no_store() {
    let resp = livez().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(CACHE_CONTROL).unwrap(), "no-store");
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], "ok");
    // `failed` is skipped when empty — the liveness body has
    // no failed-checks field at all.
    assert!(body.get("failed").is_none(), "got: {body}");
}

/// `readyz` MUST return 200 once every readiness check
/// passes. The agent-registry check passes because
/// `AppState::new` registers the canonical ReAct agent.
/// Reserved for the future; the chat surface is the only
/// check; it was retired (kept for the future).
#[tokio::test]
async fn readyz_is_200_after_state_initialized() {
    let (state, _dir) = test_state().await;
    let resp = readyz(State(state)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(CACHE_CONTROL).unwrap(), "no-store");
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], "ok");
    assert!(
        body.get("failed").is_none(),
        "readyz must report no failed checks once AppState is initialised, got: {body}"
    );
}

/// `ProbeResponse` with failed checks MUST serialize the
/// names so operators can see *why* the probe fails from the
/// response body alone.
#[test]
fn probe_response_serializes_failed_check_names() {
    let resp = ProbeResponse {
        status: "unavailable",
        failed: vec!["agent_registry"],
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["status"], "unavailable");
    assert_eq!(json["failed"][0], "agent_registry");
}

/// `/metrics` MUST return 200 with the Prometheus text
/// exposition MIME and a body containing the registered RED
/// metric families (after a labeled child has been observed).
/// Available only behind the `metrics` cargo feature. Uses
/// the global prometheus registry's lazy initialization by
/// observing a sample first — without a child,
/// `prometheus::gather()` drops childless families by design.
#[tokio::test]
async fn metrics_endpoint_serves_prometheus_text() {
    // Seed at least one labeled sample so the family has a
    // child — `prometheus::gather()` drops childless families
    // by design, and the test must observe a populated body.
    // Use a unique label so the test is order-independent
    // when the global registry is shared with other tests.
    let label_path = "/__metrics_endpoint_probe__";
    elysper::telemetry::HTTP_REQUESTS_TOTAL
        .with_label_values(&["GET", label_path])
        .inc();

    let response = metrics().await;
    assert_eq!(response.status(), StatusCode::OK);
    let mime = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("content-type");
    assert_eq!(mime, TEXT_EXPOSITION_CONTENT_TYPE);

    use http_body_util::BodyExt;
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&bytes).expect("utf-8");
    assert!(
        text.contains("http_requests_total"),
        "expected http_requests_total family in scrape, got: {text}"
    );
    assert!(
        text.contains(label_path),
        "expected labeled child `{label_path}` in scrape, got: {text}"
    );
}
