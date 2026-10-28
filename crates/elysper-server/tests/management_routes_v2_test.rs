//! Integration tests for the four new management surfaces
//! (schedules / workflows / evals / tasks) wired in the
//! "前端贯通 backend" turn. One smoke test per domain —
//! asserts the route is registered, the wire shape round-trips,
//! and the basic CRUD verbs work end-to-end.

use std::sync::Arc;

use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use elysper_server::{create_router, state::AppState};
use tower::ServiceExt;

async fn make_app() -> axum::Router {
    let temp = tempfile::TempDir::new().unwrap();
    let session_manager = elysper::session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    );
    let state =
        AppState::for_test(session_manager, temp.path().to_path_buf()).await;
    create_router(Arc::new(state)).await
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

// --- Schedules -----------------------------------------------------

#[tokio::test]
async fn test_schedules_create_list_get_delete_round_trip() {
    let app = make_app().await;
    let name = format!("test_sched_{}", std::process::id());

    // POST /api/v1/schedules
    let create = Request::builder()
        .method("POST")
        .uri("/api/v1/schedules")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "name": name,
                "description": "smoke test schedule",
                "kind": { "kind": "once" },
                "payload": { "smoke": true }
            })
            .to_string(),
        ))
        .unwrap();
    let create_resp = app.clone().oneshot(create).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED, "create");
    let created = body_json(create_resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"].as_str(), Some(name.as_str()));

    // GET /api/v1/schedules/{id}
    let get = Request::builder()
        .uri(format!("/api/v1/schedules/{id}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let get_resp = app.clone().oneshot(get).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let detail = body_json(get_resp).await;
    assert_eq!(detail["name"].as_str(), Some(name.as_str()));

    // GET /api/v1/schedules
    let list = Request::builder()
        .uri("/api/v1/schedules?limit=10")
        .body(axum::body::Body::empty())
        .unwrap();
    let list_resp = app.clone().oneshot(list).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let listed = body_json(list_resp).await;
    assert!(listed["data"].as_array().is_some());

    // DELETE /api/v1/schedules/{id}
    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/schedules/{id}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let del_resp = app.clone().oneshot(del).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
}

// --- Workflows -----------------------------------------------------

#[tokio::test]
async fn test_workflows_create_get_run_delete_round_trip() {
    let app = make_app().await;

    // POST /api/v1/workflows
    let create = Request::builder()
        .method("POST")
        .uri("/api/v1/workflows")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "id": format!("smoke_{}", std::process::id()),
                "steps": [{
                    "kind": "agent",
                    "id": "step1",
                    "agent": "echo",
                    "prompt": "say hi"
                }],
                "phases": []
            })
            .to_string(),
        ))
        .unwrap();
    let create_resp = app.clone().oneshot(create).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED, "create workflow");
    let created = body_json(create_resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["steps"].as_array().unwrap().len(), 1);

    // GET /api/v1/workflows/{id}
    let get = Request::builder()
        .uri(format!("/api/v1/workflows/{id}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let get_resp = app.clone().oneshot(get).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let detail = body_json(get_resp).await;
    assert_eq!(detail["id"].as_str(), Some(id.as_str()));

    // POST /api/v1/workflows/{id}/run — dispatches the planned call
    // through the workflow host. The host is the real one: it
    // resolves the step's agent name against the deployment's
    // registry, and this step names an agent no deployment
    // registers — so the call settles as Failed with the
    // resolution reason (a run whose agent *is* registered comes
    // back `succeeded`; see `management_routes_test`).
    let run = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/workflows/{id}/run"))
        .body(axum::body::Body::empty())
        .unwrap();
    let run_resp = app.clone().oneshot(run).await.unwrap();
    assert_eq!(run_resp.status(), StatusCode::OK);
    let run_json = body_json(run_resp).await;
    assert!(run_json["run_id"].as_str().is_some());
    let calls = run_json["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["status"].as_str(), Some("failed"));
    let error = calls[0]["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("is not registered"),
        "the failure must name the resolution reason: {error}"
    );

    // DELETE /api/v1/workflows/{id}
    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/workflows/{id}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let del_resp = app.clone().oneshot(del).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
}

// --- Evals ----------------------------------------------------------

#[tokio::test]
async fn test_evals_create_run_delete_round_trip() {
    // Script the provider's one response: the case's keywords are
    // ["echo"], so the real harness agent's drained text must pass
    // the keyword metric end to end.
    let answer = elysper::provider::CompletionResponse {
        content: elysper::provider::Content::text("echo: hello"),
        ..elysper::provider::CompletionResponse::default()
    };
    let provider: Arc<dyn elysper::provider::traits::ModelProvider> =
        Arc::new(elysper::test_support::FakeProvider::new(vec![answer]));
    let temp = tempfile::TempDir::new().unwrap();
    let session_manager = elysper::session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    );
    let mut state =
        AppState::for_test(session_manager, temp.path().to_path_buf()).await;
    state.default_provider = provider;
    let app = create_router(Arc::new(state)).await;
    let name = format!("smoke_{}", std::process::id());

    // POST /api/v1/evals
    let create = Request::builder()
        .method("POST")
        .uri("/api/v1/evals")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "name": name,
                "cases": [{
                    "id": "case1",
                    "input": "echo",
                    "expected_keywords": ["echo"],
                    "expected_output": "echo: hello"
                }]
            })
            .to_string(),
        ))
        .unwrap();
    let create_resp = app.clone().oneshot(create).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let created = body_json(create_resp).await;
    assert_eq!(created["name"].as_str(), Some(name.as_str()));

    // POST /api/v1/evals/{name}/run — the real harness agent drives
    // the case against the scripted provider; the keyword metric
    // scores its drained text.
    let run = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/evals/{name}/run"))
        .body(axum::body::Body::empty())
        .unwrap();
    let run_resp = app.clone().oneshot(run).await.unwrap();
    assert_eq!(run_resp.status(), StatusCode::OK);
    let report = body_json(run_resp).await;
    assert_eq!(report["suite_name"].as_str(), Some(name.as_str()));
    assert_eq!(report["total"].as_u64(), Some(1));
    assert_eq!(report["passed"].as_u64(), Some(1));
    assert_eq!(
        report["results"][0]["actual_output"].as_str(),
        Some("echo: hello")
    );

    // DELETE /api/v1/evals/{name}
    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/evals/{name}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let del_resp = app.clone().oneshot(del).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_workflows_and_evals_survive_state_rebuild() {
    // The restart case: create a workflow and an eval suite, then
    // build a brand-new AppState over the same workspace root —
    // both must still be listed (stores under workflows/ + evals/).
    let temp = tempfile::TempDir::new().unwrap();
    let session_manager = elysper::session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    );
    let state =
        AppState::for_test(session_manager, temp.path().to_path_buf()).await;
    let app = create_router(Arc::new(state)).await;
    let wf_id = format!("persist_{}", std::process::id());
    let ev_name = format!("persist_eval_{}", std::process::id());

    let create_wf = Request::builder()
        .method("POST")
        .uri("/api/v1/workflows")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "id": wf_id,
                "steps": [{
                    "kind": "agent",
                    "id": "step1",
                    "agent": "echo",
                    "prompt": "say hi"
                }],
                "phases": []
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(create_wf).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create workflow");

    let create_ev = Request::builder()
        .method("POST")
        .uri("/api/v1/evals")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "name": ev_name,
                "cases": [{
                    "id": "case1",
                    "input": "echo",
                    "expected_keywords": ["echo"],
                    "expected_output": "echo: hello"
                }]
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(create_ev).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create eval");

    // Rebuild the whole state over the same workspace directory.
    let session_manager2 = elysper::session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    );
    let state2 =
        AppState::for_test(session_manager2, temp.path().to_path_buf()).await;
    let app2 = create_router(Arc::new(state2)).await;
    let list_wf = Request::builder()
        .uri("/api/v1/workflows")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app2.clone().oneshot(list_wf).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let listed = body_json(resp).await;
    let ids = listed["data"]
        .as_array()
        .expect("workflow list data is an array")
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&wf_id.as_str()),
        "workflow must survive a state rebuild: {ids:?}"
    );

    let list_ev = Request::builder()
        .uri("/api/v1/evals")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app2.clone().oneshot(list_ev).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let listed = body_json(resp).await;
    let names = listed["data"]
        .as_array()
        .expect("eval list data is an array")
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&ev_name.as_str()),
        "eval suite must survive a state rebuild: {names:?}"
    );
}

// --- Tasks ----------------------------------------------------------

#[tokio::test]
async fn test_tasks_list_returns_empty_when_no_sessions() {
    // A fresh `for_test` workspace has no sessions — list_tasks
    // walks `state.session_manager.list_all()` which returns empty.
    // The endpoint must respond 200 with an empty list, not 404.
    let app = make_app().await;
    let req = Request::builder()
        .uri("/api/v1/tasks?limit=10")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["data"].is_array());
}
