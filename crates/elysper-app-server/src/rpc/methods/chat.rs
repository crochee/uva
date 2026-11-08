//! The chat surface: session creation, message dispatch, the
//! turn-level actions, usage and the operation endpoint.
use serde::Deserialize;
use serde_json::Value;
use validator::Validate;

use super::{parse, parse_raw};
use crate::{
    api::OperationRequest,
    routes::chat::{
        self,
        CreateSessionRequest,
        FeedbackRequest,
        SendMessageRequest,
    },
    rpc::{RpcRouter, encode},
};

/// `chat/message/send` — the session id beside the request body.
#[derive(Deserialize, validator::Validate)]
struct SendMessageParams {
    session_id: String,
    #[serde(flatten)]
    request: SendMessageRequest,
}
impl SendMessageParams {
    fn parse(raw: Value) -> Result<Self, crate::rpc::RpcError> {
        let parsed: Self = parse_raw(raw)?;
        parsed.request.validate()?;
        Ok(parsed)
    }
}
/// `chat/operation` — the session id beside the operation.
#[derive(Deserialize, validator::Validate)]
struct OperationParams {
    session_id: String,
    #[serde(flatten)]
    request: OperationRequest,
}
/// `chat/feedback` — the message id beside the feedback.
#[derive(Deserialize, validator::Validate)]
struct FeedbackParams {
    message_id: String,
    #[serde(flatten)]
    request: FeedbackRequest,
}
impl FeedbackParams {
    fn parse(raw: Value) -> Result<Self, crate::rpc::RpcError> {
        let parsed: Self = parse_raw(raw)?;
        parsed.request.validate()?;
        Ok(parsed)
    }
}
/// Register the chat methods.
pub fn register(router: &mut RpcRouter) {
    router.register("chat/session/create", |state, raw| async move {
        let request: CreateSessionRequest = parse(raw)?;
        encode(chat::create_session(state, request).await?)
    });
    router.register("chat/message/send", |state, raw| async move {
        let params = SendMessageParams::parse(raw)?;
        encode(
            chat::send_message(state, params.session_id, params.request)
                .await?,
        )
    });
    router.register("chat/cancel", |state, raw| async move {
        let params: CancelParams = parse(raw)?;
        encode(chat::cancel_session(state, params.session_id).await?)
    });
    router.register("chat/regenerate", |state, raw| async move {
        let params: CancelParams = parse(raw)?;
        encode(chat::regenerate(state, params.session_id).await?)
    });
    router.register("chat/feedback", |state, raw| async move {
        let params = FeedbackParams::parse(raw)?;
        encode(chat::feedback(state, params.message_id, params.request).await?)
    });
    router.register("chat/usage", |state, _raw| async move {
        encode(chat::usage(state).await)
    });
    router.register("chat/operation", |state, raw| async move {
        let params: OperationParams = parse_raw(raw)?;
        encode(chat::operation(state, params.session_id, params.request).await?)
    });
}
/// `chat/cancel` and `chat/regenerate` — the session id alone.
#[derive(Deserialize, validator::Validate)]
struct CancelParams {
    session_id: String,
}
