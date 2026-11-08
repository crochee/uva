//! The `[ui]` projection: `ui/schema`.
//!
//! The response is data, not code. Each entry carries the schema
//! version and the components a registered renderer draws; the server
//! never sends JavaScript or CSS, and an unknown component kind reaches
//! the client unchanged so its registry can show the fallback
//! placeholder instead of dropping the whole segment.

use serde::Serialize;

use crate::{state::AppState, ui::schema::UiSchema};

/// `ui/schema` response body.
#[derive(Debug, Serialize)]
pub struct UiSchemaResponse {
    /// The schemas of every `[ui]` row, in assembly order.
    pub schemas: Vec<UiSchema>,
}

/// `ui/schema` — the resolved `[ui]` segment.
#[must_use]
pub fn list_ui_schemas(state: &AppState) -> UiSchemaResponse {
    UiSchemaResponse {
        schemas: state.ui_schemas(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::list_ui_schemas;
    use crate::{
        state::AppState,
        ui::schema::{UiComponent, UiSchema},
    };

    async fn state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = elysper::session::manager::SessionRegistry::new(
            dir.path().join("sessions"),
        );
        let state =
            AppState::for_test(registry, dir.path().to_path_buf()).await;
        (state, dir)
    }

    #[tokio::test]
    async fn an_empty_segment_reports_no_schemas() {
        let (state, _dir) = state().await;
        let body = list_ui_schemas(&state);
        assert!(body.schemas.is_empty());
        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({ "schemas": [] })
        );
    }

    #[tokio::test]
    async fn the_resolved_schemas_are_projected_in_order() {
        let (state, _dir) = state().await;
        *state.ui_schemas.write() = vec![UiSchema {
            version: 1,
            components: vec![UiComponent {
                slot: "chat.sidebar".to_owned(),
                kind: "panel".to_owned(),
                props: json!({ "title": "Notes" }),
            }],
        }];

        let body = list_ui_schemas(&state);
        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({
                "schemas": [{
                    "version": 1,
                    "components": [{
                        "slot": "chat.sidebar",
                        "kind": "panel",
                        "props": { "title": "Notes" },
                    }],
                }],
            })
        );
    }

    #[tokio::test]
    async fn the_method_is_served_under_its_wire_name() {
        let (state, _dir) = state().await;
        *state.ui_schemas.write() = vec![UiSchema {
            version: 3,
            components: Vec::new(),
        }];
        let state = Arc::new(state);

        let mut router = crate::rpc::RpcRouter::new();
        crate::rpc::methods::register_all(&mut router);
        let response = router
            .dispatch(state, r#"{"jsonrpc":"2.0","id":7,"method":"ui/schema"}"#)
            .await
            .expect("a request frame is answered");
        assert_eq!(response["jsonrpc"], json!("2.0"));
        assert_eq!(response["id"], json!(7));
        assert!(
            response.get("error").is_none(),
            "the method answers with a result: {response}"
        );
        assert_eq!(response["result"]["schemas"][0]["version"], json!(3));
    }
}
