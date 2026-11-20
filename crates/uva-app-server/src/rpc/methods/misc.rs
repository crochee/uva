//! The remaining surfaces: the probes, the model catalogue, the
//! cross-domain search and the attachment store.
use serde::Deserialize;

use super::{names, parse, parse_raw};
use crate::{
    routes::{
        attachment::{self, CreateAttachmentRequest},
        health,
        search::{self, SearchParams},
    },
    rpc::{RpcRouter, encode},
};

/// Register the remaining methods.
pub fn register(router: &mut RpcRouter) {
    router.register(names::HEALTH_PING, |_state, _raw| async move {
        Ok(serde_json::json!({
            "ok": true,
            "version": crate::build_info::VERSION,
        }))
    });
    router.register(names::HEALTH_VERSION, |_state, _raw| async move {
        encode(health::version())
    });
    router.register(names::HEALTH_LIVEZ, |_state, _raw| async move {
        encode(health::livez())
    });
    router.register(names::HEALTH_READYZ, |state, _raw| async move {
        encode(health::readyz(state))
    });
    router.register(names::MODELS_LIST, |state, _raw| async move {
        encode(health::list_models(state))
    });
    router.register(names::UI_SCHEMA, |state, _raw| async move {
        encode(crate::routes::ui::list_ui_schemas(&state))
    });
    router.register(names::ANTHROPIC_MESSAGES, |state, raw| async move {
        let request = parse_raw(raw)?;
        encode(crate::routes::messages::messages(state, request).await?)
    });
    router.register(names::SEARCH_ALL, |state, raw| async move {
        let params: SearchParams = parse(raw)?;
        let user_id = state.default_user_id().to_string();
        encode(search::search_all(state, user_id, params).await?)
    });
    router.register(names::ATTACHMENTS_CREATE, |state, raw| async move {
        let request: CreateAttachmentRequest = parse(raw)?;
        encode(attachment::create_attachment(state, request).await?)
    });
    router.register(names::ATTACHMENTS_GET, |state, raw| async move {
        let params: HashParams = parse(raw)?;
        encode(attachment::get_attachment(state, params.hash).await?)
    });
    router.register(names::ATTACHMENTS_DELETE, |state, raw| async move {
        let params: HashParams = parse(raw)?;
        encode(attachment::delete_attachment(state, params.hash).await?)
    });
    router.register(names::METRICS_GATHER, |_state, _raw| async move {
        // The exposition of **this** process. The core runs the agent
        // loop, so the tool counters live here; the gateway holds the
        // HTTP counters and merges this text into its own `/metrics`.
        let text = String::from_utf8_lossy(&uva::telemetry::gather_text())
            .into_owned();
        Ok(serde_json::json!({ "text": text }))
    });
}
/// The one-parameter shape: `{"hash": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct HashParams {
    hash: String,
}
