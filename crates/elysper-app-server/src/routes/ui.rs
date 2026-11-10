//! The `[ui]` projection: `ui/schema`.
//!
//! The response is data, not code. It carries the slots the `[ui]` rows
//! declare (`slots`) and, per row, the schema version and the
//! components a registered renderer draws (`schemas`). The server never
//! sends JavaScript or CSS, and an unknown component kind reaches the
//! client unchanged so its registry can show the fallback placeholder
//! instead of dropping the whole segment.

use serde::Serialize;

use crate::{
    state::AppState,
    ui::{schema::UiSchema, tree::SlotInfo},
};

/// `ui/schema` response body.
#[derive(Debug, Serialize)]
pub struct UiSchemaResponse {
    /// The slots this deployment's `[ui]` rows declare, in declaration
    /// order.
    pub slots: Vec<SlotInfo>,
    /// The schemas of every `[ui]` row, in assembly order.
    pub schemas: Vec<UiSchema>,
}

/// `ui/schema` — the resolved `[ui]` segment.
#[must_use]
pub fn list_ui_schemas(state: &AppState) -> UiSchemaResponse {
    let tree = state.ui_tree();
    UiSchemaResponse {
        slots: tree.slots(),
        schemas: tree.schemas(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper::kernel::RuntimeKind;
    use serde_json::json;

    use super::list_ui_schemas;
    use crate::{profile::Entry, state::AppState, ui::tree::load_ui_tree};

    fn entry(id: &str, config: serde_json::Value) -> Entry {
        Entry {
            id: id.to_owned(),
            runtime: RuntimeKind::UiBundle,
            config,
            capabilities: Vec::new(),
            enabled: crate::profile::Enabled::On,
        }
    }

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
        assert!(body.slots.is_empty());
        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({ "slots": [], "schemas": [] })
        );
    }

    #[tokio::test]
    async fn the_resolved_schemas_are_projected_in_order() {
        let (state, _dir) = state().await;
        *state.ui_tree.write() = load_ui_tree(&[entry(
            "ui.slot.chat.sidebar",
            json!({
                "slots": [{ "id": "chat.sidebar" }],
                "components": [{
                    "slot": "chat.sidebar",
                    "kind": "panel",
                    "props": { "title": "Notes" },
                }],
            }),
        )])
        .expect("the tree activates");

        let body = list_ui_schemas(&state);
        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({
                "slots": [{
                    "id": "chat.sidebar",
                    "cardinality": "many",
                    "owner": "ui.slot.chat.sidebar",
                }],
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
    async fn the_response_carries_the_declared_slots() {
        let (state, _dir) = state().await;
        *state.ui_tree.write() = load_ui_tree(&[
            entry(
                "ui.shell",
                json!({ "slots": [
                    { "id": "chat.sidebar", "cardinality": "many" },
                    { "id": "session.detail.extensions", "cardinality": "one" },
                ] }),
            ),
            entry(
                "ui.slot.notes",
                json!({ "components": [{
                    "slot": "chat.sidebar",
                    "kind": "panel",
                    "props": { "title": "Notes" },
                }] }),
            ),
        ])
        .expect("the tree activates");

        let body =
            serde_json::to_value(list_ui_schemas(&state)).expect("serializes");
        assert_eq!(
            body["slots"],
            json!([
                {
                    "id": "chat.sidebar",
                    "cardinality": "many",
                    "owner": "ui.shell",
                },
                {
                    "id": "session.detail.extensions",
                    "cardinality": "one",
                    "owner": "ui.shell",
                },
            ])
        );
        assert_eq!(body["schemas"][1]["components"][0]["kind"], json!("panel"));
    }

    #[tokio::test]
    async fn the_method_is_served_under_its_wire_name() {
        let (state, _dir) = state().await;
        *state.ui_tree.write() = load_ui_tree(&[entry(
            "ui.slot.versioned",
            json!({ "version": 3 }),
        )])
        .expect("the tree activates");
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
