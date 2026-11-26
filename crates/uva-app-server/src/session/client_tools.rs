//! The tools the client owns.
//!
//! A client can hand the agent its own tools: it registers a name, a
//! description and a schema, and the core asks it to execute every
//! call over the core's own JSON-RPC face (the omp
//! `set_host_tools` + `host_tool_call` / `host_tool_result` shape).
//! dsh has no counterpart — its client owns *capabilities* (files,
//! terminals), not tools.
//!
//! # Wire shape
//!
//! The registration is an ordinary request the client sends
//! (`tools/register` with `owner: "client"`); the execution is the
//! reverse direction — a request the **client** answers:
//!
//! ```text
//! → {"jsonrpc":"2.0","id":"core:internal:0","method":"tools/execute",
//!    "params":{"session_id":"s-1","name":"my_tool","arguments":{…}}}
//! ← {"jsonrpc":"2.0","id":"core:internal:0",
//!    "result":{"content":"…","is_error":false}}
//! ```
//!
//! The `session_id` in the request's params is what a gateway uses to
//! route the question to the connection that owns the session — the
//! same rule that carries the ACP permission question.
//!
//! # Ownership
//!
//! The registration names the session whose client executes the tool.
//! The node's registry is one for the whole deployment, so the
//! *name* is global (two clients cannot register the same name),
//! while the *execution* is the owning session's client's: a call
//! from a session that does not own the tool is refused — fail
//! closed — and the tool is advertised to every session's model,
//! because the registry the harness projects is the node's one.
//! Those are the known limits of the node-wide registry, recorded in
//! the ledger with this cut.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::Value;
use uva::tool::{Context, ExecutionMode, Tool, ToolOutput};

use crate::rpc::{ClientCallError, RpcRuntime};

/// The method the **client** answers when the core asks it to run a
/// tool it owns.
///
/// The name is not in the core's method table — that table lists the
/// methods the process *answers*; this one travels the other
/// direction, and a client that wants to own tools implements it.
pub const CLIENT_TOOL_EXECUTE: &str = "tools/execute";

/// How long the core waits for the client's answer before the call
/// fails as a structured timeout.
///
/// omp has no bound (a call lives until the turn is aborted); dsh
/// gives every tool a `timeoutMs`. A bound is the headless-safe
/// default — a client that walks away must not hang the turn — and
/// the model sees the structured `TOOL_TIMEOUT` failure and can
/// retry.
const CLIENT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// A tool whose implementation lives in the client that registered
/// it.
///
/// Every call travels to the owning session's client and back before
/// the model sees a result; the client's answer is the tool's output.
pub(crate) struct ClientOwnedTool {
    name: String,
    description: String,
    parameters: Value,
    session_id: String,
    rpc: Arc<RpcRuntime>,
}

impl ClientOwnedTool {
    /// Build the entry the registration installs.
    pub(crate) fn new(
        name: String,
        description: String,
        parameters: Value,
        session_id: String,
        rpc: Arc<RpcRuntime>,
    ) -> Self {
        Self {
            name,
            description,
            parameters,
            session_id,
            rpc,
        }
    }
}

#[async_trait]
impl Tool for ClientOwnedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    /// The client's execution is opaque to the core — it cannot know
    /// which calls are safe to overlap, and a guess is not fail
    /// closed. Every call runs alone.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn call(&self, input: Value, context: &Context) -> ToolOutput {
        if context.session_id != self.session_id {
            return ToolOutput::error(format!(
                "this tool belongs to session `{}`'s client; session \
                 `{}` cannot call it",
                self.session_id, context.session_id
            ));
        }
        let params = serde_json::json!({
            "session_id": self.session_id,
            "name": self.name,
            "arguments": input,
        });
        match self
            .rpc
            .call_client(CLIENT_TOOL_EXECUTE, params, CLIENT_TOOL_TIMEOUT)
            .await
        {
            Ok(value) => match serde_json::from_value::<
                crate::rpc::ClientToolResult,
            >(value)
            {
                Ok(result) if result.is_error => {
                    ToolOutput::error(result.content)
                }
                Ok(result) => ToolOutput::text(result.content),
                Err(error) => ToolOutput::error(format!(
                    "the client's answer was not a tool result: {error}"
                )),
            },
            Err(ClientCallError::Timeout) => {
                ToolOutput::timeout(CLIENT_TOOL_TIMEOUT)
            }
            Err(ClientCallError::Refused { message }) => {
                ToolOutput::error(message)
            }
            Err(ClientCallError::Closed) => ToolOutput::error(format!(
                "the client is gone; `{}` did not run",
                self.name
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A runtime with no transport: a call over it can only time out
    /// or be dropped — enough to prove the failure shapes without a
    /// peer.
    fn detached_runtime() -> Arc<RpcRuntime> {
        Arc::new(RpcRuntime::detached())
    }

    fn tool(session: &str, runtime: Arc<RpcRuntime>) -> ClientOwnedTool {
        ClientOwnedTool::new(
            "my_tool".to_string(),
            "runs on the client".to_string(),
            json!({"type": "object"}),
            session.to_string(),
            runtime,
        )
    }

    fn context(session: &str) -> Context {
        Context::new(session.to_string(), "/tmp".into())
    }

    #[tokio::test]
    async fn a_call_from_a_foreign_session_is_refused() {
        let owned = tool("s-1", detached_runtime());
        let output = owned.call(json!({}), &context("s-2")).await;
        assert_eq!(output.is_error, Some(true));
        let text = serde_json::to_string(&output.content).unwrap();
        assert!(
            text.contains("belongs to session `s-1`"),
            "the refusal names the owner: {text}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_never_answers_times_out() {
        let owned = tool("s-1", detached_runtime());
        let output = owned.call(json!({}), &context("s-1")).await;
        assert_eq!(
            output.error_code(),
            Some(uva::tool::ToolOutput::TIMEOUT_CODE),
            "the timeout is the structured failure: {output:?}"
        );
    }

    #[test]
    fn the_entry_advertises_what_the_client_declared() {
        let owned = ClientOwnedTool::new(
            "deploy".to_string(),
            "deploys the editor's build".to_string(),
            json!({"type": "object", "properties": {"target": {"type": "string"}}}),
            "s-1".to_string(),
            detached_runtime(),
        );
        assert_eq!(owned.name(), "deploy");
        assert_eq!(owned.description(), "deploys the editor's build");
        assert_eq!(
            owned.parameters()["properties"]["target"]["type"],
            "string"
        );
        assert!(matches!(owned.mode(), ExecutionMode::Sequential));
    }
}
