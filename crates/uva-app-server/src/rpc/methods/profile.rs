//! Profile methods: the settings surface over JSON-RPC.
//!
//! Every write delegates to [`AppState::profile_apply`](crate::state::AppState::profile_apply) — the same
//! transaction the self-MCP `profile` tool calls. The actor is always
//! `"ui"`: a client on this transport is a human-facing surface, which
//! is audited but not approval-gated (the agent path goes through MCP,
//! whose destructive hint makes it `Risk::High`).

use serde::Deserialize;
use serde_json::Value;

use super::{parse, parse_raw};
use crate::{
    profile::{
        CapabilityAction,
        CapabilityDomain,
        Enabled,
        ProfileAction,
        RowEdit,
    },
    rpc::{RpcError, RpcRouter, encode},
};

/// Register the profile methods.
pub fn register(router: &mut RpcRouter) {
    router.register("profile/list", |state, _raw| async move {
        encode(state.profile_rows())
    });
    router.register("profile/get", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        match state.profile_row(&params.id) {
            Some(row) => encode(row),
            None => Err(RpcError::application(
                "not_found",
                format!("the profile has no row `{}`", params.id),
            )),
        }
    });
    router.register("profile/set", |state, raw| async move {
        let params: SetParams = parse_raw(raw)?;
        if params.enabled.is_none() && params.config.is_none() {
            return Err(RpcError::protocol(
                crate::rpc::INVALID_PARAMS,
                "profile/set requires `enabled` and/or `config`",
            ));
        }
        let edit = RowEdit::Set {
            id: params.id,
            enabled: params.enabled,
            config: params.config,
        };
        encode(state.profile_apply("ui", ProfileAction::Edit(edit)).await)
    });
    router.register("profile/create", |state, raw| async move {
        let params: CreateParams = parse_raw(raw)?;
        encode(
            state
                .profile_apply(
                    "ui",
                    ProfileAction::Capability(CapabilityAction::Create {
                        domain: params.domain,
                        definition: params.definition,
                    }),
                )
                .await,
        )
    });
    router.register("profile/delete", |state, raw| async move {
        let params: DeleteParams = parse_raw(raw)?;
        encode(
            state
                .profile_apply(
                    "ui",
                    ProfileAction::Capability(CapabilityAction::Delete {
                        domain: params.domain,
                        id: params.id,
                    }),
                )
                .await,
        )
    });
}

/// `{"id": "..."}`.
#[derive(Deserialize, validator::Validate)]
struct IdParams {
    id: String,
}

/// `{"id": "...", "enabled"?: ..., "config"?: {...}}`.
#[derive(Deserialize)]
struct SetParams {
    id: String,
    #[serde(default)]
    enabled: Option<Enabled>,
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
        state.profile_store = Some(ProfileStore {
            bundle: None,
            doc: dir.join("profile.toml"),
            home: None,
            overlay: None,
        });
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
            json!({ "id": "skills", "enabled": false }),
        )
        .await;
        assert_eq!(answer["result"]["state"], "applied");
        assert!(
            dir.path().join("profile.toml").exists(),
            "the transaction commits the document"
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
