use std::{
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    http::{Request, StatusCode},
    response::Response,
};
use futures::future::BoxFuture;
use tower::Service;

use super::{
    path::{PUBLIC_PATHS, normalize_path},
    types::RequestUserId,
    user_id::{resolve_user_id_from_key, resolve_user_id_unconfigured},
};
use crate::config::AuthConfig;

/// Authentication middleware that validates API Key or Bearer Token.
///
/// Reads the credential from `Authorization` (with or without a
/// `Bearer ` prefix) or — for Anthropic-protocol clients, which send
/// `x-api-key` and no `Authorization` header at all — from `x-api-key`,
/// and validates it against a configured API key. If no API key is
/// configured (empty), the middleware allows all requests. Certain paths
/// (e.g., /livez, /readyz) are exempt from authentication.
///
/// `x-api-key` is accepted as an *alternative*, never as a bypass: the
/// value must equal the configured key, so a deployment that sets
/// `ELYSPER_API_KEY` stays closed whether the caller speaks Bearer or
/// the Anthropic protocol.
#[derive(Clone)]
pub struct AuthMiddleware<S> {
    inner: S,
    auth_config: Arc<AuthConfig>,
    /// API key captured at startup. Reading the env on every
    /// request is wasteful (the value never changes for the
    /// lifetime of the server), and the `Arc<str>` lets us
    /// share it across `Clone`d middleware instances for free.
    api_key: Arc<str>,
}

impl<S> AuthMiddleware<S> {
    pub fn new(inner: S, auth_config: Arc<AuthConfig>) -> Self {
        // Read the env once at construction; if the operator
        // rotates `ELYSPER_API_KEY` they must restart the server,
        // which matches the rest of the config model.
        let api_key: Arc<str> =
            std::env::var("ELYSPER_API_KEY").unwrap_or_default().into();
        Self {
            inner,
            auth_config,
            api_key,
        }
    }

    /// Check if a request path should bypass authentication.
    pub(super) fn is_public_path(path: &str) -> bool {
        let normalized = match normalize_path(path) {
            Some(p) => p,
            None => return false,
        };
        PUBLIC_PATHS.iter().any(|public| {
            if normalized == *public {
                return true;
            }
            if normalized.starts_with(&format!("{public}/")) {
                let remaining = &normalized[public.len() + 1..];
                return !remaining.contains('/');
            }
            false
        })
    }

    /// Validate the provided token against the configured API key.
    fn validate_token(token: &str, api_key: &str) -> bool {
        token == api_key
    }

    /// The credential a request presents, in precedence order.
    ///
    /// `Authorization` wins when both headers are present, so a client
    /// that sends a stale `x-api-key` alongside a valid bearer token is
    /// still authenticated by the header the rest of this server uses.
    /// A bare `Authorization` value (no `Bearer ` prefix) is accepted,
    /// matching the pre-existing behaviour.
    fn credential<B>(req: &Request<B>) -> Option<&str> {
        let authorization = req
            .headers()
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .map(|header| header.strip_prefix("Bearer ").unwrap_or(header));
        authorization.or_else(|| {
            req.headers()
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
    }
}

/// Path of the Anthropic Messages compatibility surface.
const MESSAGES_PATH: &str = "/v1/messages";

/// `true` when `path` is the Anthropic-protocol surface, which answers
/// with the protocol's error envelope on every failure.
fn is_anthropic_path(path: &str) -> bool {
    path == MESSAGES_PATH
}

/// The `401` a rejected request receives.
///
/// `/api/v1/*` keeps the application envelope byte-for-byte; the
/// Anthropic surface answers with `{"type":"error","error":{…}}` so an
/// Anthropic SDK raises a parseable authentication error instead of
/// choking on an unknown body.
fn unauthorized_response(anthropic: bool) -> Response {
    const MESSAGE: &str = "Invalid or missing authentication credentials";
    let body = if anthropic {
        serde_json::json!({
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": MESSAGE,
            }
        })
    } else {
        serde_json::json!({
            "status": "err",
            "error": {
                "code": "unauthorized",
                "message": MESSAGE
            }
        })
    };
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::new(body.to_string()))
        .unwrap()
}

impl<S, B> Service<Request<B>> for AuthMiddleware<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;
    type Response = S::Response;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let is_public_path = Self::is_public_path(req.uri().path());
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let auth_config = self.auth_config.clone();
        // Clone the Arc<str> once per call (cheap pointer + refcount bump)
        // rather than re-reading the env on every request.
        let api_key = Arc::clone(&self.api_key);

        // Allow public paths and unconfigured auth
        if is_public_path {
            // Still inject a user_id so handlers can find the namespace.
            req.extensions_mut()
                .insert(RequestUserId(resolve_user_id_unconfigured()));
            return Box::pin(async move { inner.call(req).await });
        }

        Box::pin(async move {
            if api_key.is_empty() {
                // Unconfigured: pass through with the server default
                // user_id. This preserves the historical "no auth" dev
                // path while keeping the §1 invariant (non-empty id).
                req.extensions_mut()
                    .insert(RequestUserId(resolve_user_id_unconfigured()));
                return inner.call(req).await;
            }

            let extracted_token = Self::credential(&req).map(str::to_string);

            let is_valid = match &extracted_token {
                Some(token) => Self::validate_token(token, &api_key),
                None => false,
            };

            if !is_valid {
                return Ok(unauthorized_response(is_anthropic_path(
                    req.uri().path(),
                )));
            }

            // Auth passed: resolve the user_id from the (validated) key.
            // The key in the header matches api_key, so it is in the
            // auth set; we still call resolve_user_id_from_key so that
            // the explicit map can override the default derivation.
            let user_id = match extracted_token.as_deref() {
                Some(token) => resolve_user_id_from_key(token, &auth_config)
                    .unwrap_or_else(resolve_user_id_unconfigured),
                None => resolve_user_id_unconfigured(),
            };
            req.extensions_mut().insert(RequestUserId(user_id));

            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests;
