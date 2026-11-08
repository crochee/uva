//! The remaining surfaces: the probes, the model catalogue, the
//! plugin composition, the cross-domain search and the attachment
//! store.
use serde::Deserialize;

use super::{parse, parse_raw};
use crate::{
    routes::{
        attachment::{self, CreateAttachmentRequest},
        health,
        plugins,
        search::{self, SearchParams},
    },
    rpc::{RpcRouter, encode},
};

/// Register the remaining methods.
pub fn register(router: &mut RpcRouter) {
    router.register("health/ping", |_state, _raw| async move {
        Ok(serde_json::json!({
            "ok": true,
            "version": crate::build_info::VERSION,
        }))
    });
    router.register("health/version", |_state, _raw| async move {
        encode(health::version())
    });
    router.register("health/livez", |_state, _raw| async move {
        encode(health::livez())
    });
    router.register("health/readyz", |state, _raw| async move {
        encode(health::readyz(state))
    });
    router.register("models/list", |state, _raw| async move {
        encode(health::list_models(state))
    });
    router.register("plugins/list", |state, _raw| async move {
        encode(plugins::list_plugins(state).await?)
    });
    router.register("ui/schema", |state, _raw| async move {
        encode(crate::routes::ui::list_ui_schemas(&state))
    });
    router.register("anthropic/messages", |state, raw| async move {
        let request = parse_raw(raw)?;
        encode(crate::routes::messages::messages(state, request).await?)
    });
    router.register("search/all", |state, raw| async move {
        let params: SearchParams = parse(raw)?;
        let user_id = state.default_user_id().to_string();
        encode(search::search_all(state, user_id, params).await?)
    });
    router.register("attachments/create", |state, raw| async move {
        let request: CreateAttachmentRequest = parse(raw)?;
        encode(attachment::create_attachment(state, request).await?)
    });
    router.register("attachments/get", |state, raw| async move {
        let params: HashParams = parse(raw)?;
        encode(attachment::get_attachment(state, params.hash).await?)
    });
    router.register("attachments/delete", |state, raw| async move {
        let params: HashParams = parse(raw)?;
        encode(attachment::delete_attachment(state, params.hash).await?)
    });
}
/// The one-parameter shape: `{"hash": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct HashParams {
    hash: String,
}
