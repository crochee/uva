//! Profile methods: the settings surface over JSON-RPC.
//!
//! Every write delegates to [`AppState::profile_apply`](crate::state::AppState::profile_apply) — the same
//! transaction the self-MCP `profile` tool calls. The actor is always
//! `"ui"`: a client on this transport is a human-facing surface, which
//! is audited but not approval-gated (the agent path goes through MCP,
//! whose destructive hint makes it `Risk::High`).

use serde::Deserialize;
use serde_json::Value;

use super::{names, parse, parse_raw};
use crate::{
    api::v1::profile::ProfileChange,
    profile::{CapabilityAction, CapabilityDomain, ProfileAction, RowEdit},
    rpc::{RpcError, RpcRouter, encode},
};

/// Register the profile methods.
///
/// The responses are named types ([`ProfileRow`](crate::api::v1::profile::ProfileRow)
/// for the two reads, [`ProfileChange`] for the three writes), not
/// bare JSON: the schema emitter derives a schema from them.
pub fn register(router: &mut RpcRouter) {
    router.register(names::PROFILE_LIST, |state, _raw| async move {
        encode(state.profile_rows())
    });
    router.register(names::PROFILE_AUDIT, |state, _raw| async move {
        encode(state.profile_audit())
    });
    router.register(names::PROFILE_GET, |state, raw| async move {
        let params: IdParams = parse(raw)?;
        match state.profile_row(&params.id) {
            Some(row) => encode(row),
            None => Err(RpcError::application(
                "not_found",
                format!("the profile has no row `{}`", params.id),
            )),
        }
    });
    router.register(names::PROFILE_SET, |state, raw| async move {
        let params: SetParams = parse_raw(raw)?;
        if params.disabled.is_none() && params.config.is_none() {
            return Err(RpcError::protocol(
                crate::rpc::INVALID_PARAMS,
                "profile/set requires `disabled` and/or `config`",
            ));
        }
        let edit = RowEdit::Set {
            id: params.id,
            disabled: params.disabled,
            config: params.config,
        };
        encode(ProfileChange::from(
            state.profile_apply("ui", ProfileAction::Edit(edit)).await,
        ))
    });
    router.register(names::PROFILE_CREATE, |state, raw| async move {
        let params: CreateParams = parse_raw(raw)?;
        encode(ProfileChange::from(
            state
                .profile_apply(
                    "ui",
                    ProfileAction::Capability(CapabilityAction::Create {
                        domain: params.domain,
                        definition: params.definition,
                    }),
                )
                .await,
        ))
    });
    router.register(names::PROFILE_DELETE, |state, raw| async move {
        let params: DeleteParams = parse_raw(raw)?;
        encode(ProfileChange::from(
            state
                .profile_apply(
                    "ui",
                    ProfileAction::Capability(CapabilityAction::Delete {
                        domain: params.domain,
                        id: params.id,
                    }),
                )
                .await,
        ))
    });
}

/// `{"id": "..."}`.
#[derive(Deserialize, validator::Validate)]
struct IdParams {
    id: String,
}

/// `{"id": "...", "disabled"?: ..., "config"?: {...}}`.
#[derive(Deserialize)]
struct SetParams {
    id: String,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    config: Option<Value>,
}

/// `{"domain": "agent" | "skill", "definition": {...}}`.
#[derive(Deserialize)]
struct CreateParams {
    domain: CapabilityDomain,
    definition: Value,
}

/// `{"domain": "agent" | "skill", "id": "..."}`.
#[derive(Deserialize)]
struct DeleteParams {
    domain: CapabilityDomain,
    id: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use uva::session::manager::SessionRegistry;

    use crate::{profile::ProfileStore, rpc::RpcRouter, state::AppState};

    async fn state(dir: &std::path::Path) -> Arc<AppState> {
        let mut state = AppState::for_test(
            SessionRegistry::new(dir.join("sessions")),
            dir.to_path_buf(),
        )
        .await;
        state.assembly.set_store(Some(ProfileStore {
            bundle: None,
            doc: dir.join("profile.toml"),
            home: None,
            overlay: None,
        }));
        Arc::new(state)
    }

    fn router() -> RpcRouter {
        let mut router = RpcRouter::new();
        super::register(&mut router);
        router
    }

    async fn call(
        router: &RpcRouter,
        state: Arc<AppState>,
        id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        router
            .dispatch_value(state, frame)
            .await
            .expect("a request answers")
    }

    #[tokio::test]
    async fn list_and_get_read_the_profile_rows() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let listed =
            call(&router, Arc::clone(&state), "1", "profile/list", json!({}))
                .await;
        let rows = listed["result"].as_array().expect("a row array");
        assert!(rows.iter().any(|row| row["id"] == "skills"));
        assert!(rows.iter().all(|row| row.get("status").is_some()));

        let got = call(
            &router,
            state,
            "2",
            "profile/get",
            json!({ "id": "skills" }),
        )
        .await;
        assert_eq!(got["result"]["id"], "skills");
    }

    #[tokio::test]
    async fn set_returns_the_transaction_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let answer = call(
            &router,
            state,
            "1",
            "profile/set",
            json!({ "id": "skills", "disabled": true }),
        )
        .await;
        assert_eq!(answer["result"]["state"], "applied");
        assert!(
            dir.path().join("profile.toml").exists(),
            "the transaction commits the document"
        );
    }

    /// The row's wire shape is the settings page's contract: it reads
    /// these fields by name, so an added, removed, or renamed field is
    /// a breaking change.
    #[tokio::test]
    async fn a_row_keeps_the_shape_the_settings_page_reads() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let got = call(
            &router,
            state,
            "1",
            "profile/get",
            json!({ "id": "skills" }),
        )
        .await;
        let row = got["result"].as_object().expect("a row object");
        let mut keys: Vec<&str> = row.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "capabilities",
                "config",
                "disabled",
                "group",
                "id",
                "inject",
                "name",
                "plugin_state",
                "segment",
                "status",
            ]
        );
        assert_eq!(row["id"], "skills");
        assert_eq!(row["segment"], "core");
        assert_eq!(row["name"], "skills");
        assert_eq!(row["disabled"], false);
        assert_eq!(row["status"], "mounted");
    }

    /// The receipt's wire shape: the four states, the reasons, and the
    /// diff the mount produced.
    #[tokio::test]
    async fn a_receipt_keeps_the_shape_the_settings_page_renders() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let answer = call(
            &router,
            state,
            "1",
            "profile/set",
            json!({ "id": "skills", "disabled": true }),
        )
        .await;
        let receipt = answer["result"].as_object().expect("a receipt");
        let mut keys: Vec<&str> = receipt.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["diff", "reasons", "state"]);
        assert_eq!(receipt["state"], "applied");
        assert!(receipt["reasons"].is_array(), "{receipt:?}");
        assert_eq!(receipt["diff"]["removed"], json!(["skills"]));
    }

    /// A capability write answers with the same receipt, minus the
    /// diff: it changes files, not the assembly.
    #[tokio::test]
    async fn a_capability_write_answers_with_the_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let answer = call(
            &router,
            state,
            "1",
            "profile/create",
            json!({ "domain": "agent", "definition": { "name": "x" } }),
        )
        .await;
        let receipt = answer["result"].as_object().expect("a receipt");
        let mut keys: Vec<&str> = receipt.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["reasons", "state"]);
        assert_eq!(receipt["state"], "failed");
    }

    /// `profile/delete` answers with the same receipt.
    #[tokio::test]
    async fn a_delete_answers_with_the_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let answer = call(
            &router,
            state,
            "1",
            "profile/delete",
            json!({ "domain": "agent", "id": "nobody" }),
        )
        .await;
        let receipt = answer["result"].as_object().expect("a receipt");
        let mut keys: Vec<&str> = receipt.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["reasons", "state"]);
        let state = receipt["state"].as_str().expect("a state");
        assert!(
            ["applied", "restart-required", "overridden", "failed"]
                .contains(&state),
            "{receipt:?}"
        );
    }

    #[tokio::test]
    async fn set_without_a_field_is_a_params_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let router = router();
        let answer = call(
            &router,
            state,
            "1",
            "profile/set",
            json!({ "id": "skills" }),
        )
        .await;
        assert_eq!(answer["error"]["code"], -32602);
    }
}
