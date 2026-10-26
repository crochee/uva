//! One remote tool, as a [`Tool`].
//!
//! The descriptor comes from the plugin's `synthia/tools/list` answer;
//! a call is one `synthia/tools/call` round trip. Two things are
//! deliberately *not* the plugin's to decide: the risk of a call (the
//! host config names it — see [`Tool::risk`]) and whether two
//! calls may run at once ([`RemoteTool::mode`] answers one at a time,
//! because the plugin's side effects are opaque to the host).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use synthia_plugin_protocol::{ToolSpec, methods};
use synthia_tool::{
    Context,
    ExecutionMode,
    Risk,
    Tool,
    ToolAnnotations,
    ToolOutput,
};

use crate::client::Connection;

/// A tool the plugin offers, registered into the harness's `Host`.
///
/// Constructed by [`RemotePlugin::install`](crate::RemotePlugin::install);
/// the host only ever sees it as an `Arc<dyn Tool>`, so the type stays
/// inside this crate.
pub(crate) struct RemoteTool {
    name: String,
    description: String,
    parameters: Value,
    risk: Risk,
    connection: Arc<Connection>,
}

impl RemoteTool {
    pub(crate) fn new(
        spec: ToolSpec,
        risk: Risk,
        connection: Arc<Connection>,
    ) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            parameters: spec.input_schema,
            risk,
            connection,
        }
    }
}

#[async_trait]
impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    /// One call at a time.
    ///
    /// The host cannot see what a plugin process touches, so it must not
    /// assume two calls are independent: a plugin that is safe to call
    /// concurrently is the plugin's claim to make, and the wire has no
    /// way to make it yet.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    /// The plugin's annotations are display-only, and this wire version
    /// carries none.
    ///
    /// If a later wire version adds them, they stay display-only: they
    /// must never feed [`Tool::risk`], because a plugin can claim
    /// `destructive_hint: false` to dodge the approval it needs.
    fn annotations(&self) -> Option<ToolAnnotations> {
        None
    }

    /// The risk the **host** configured for this plugin.
    ///
    /// The default implementation would derive the risk from
    /// [`Tool::annotations`] — that is, from what the plugin says about
    /// itself. A remote plugin could then advertise
    /// `destructive_hint: false` and be treated as safe, which is exactly
    /// the hole the reduced-authority policy exists to close. The
    /// approval policy therefore reads this value and never the plugin's
    /// self-report.
    fn risk(&self, _input: &Value) -> Risk {
        self.risk
    }

    async fn call(&self, input: Value, _context: &Context) -> ToolOutput {
        let params = serde_json::json!({
            "name": self.name,
            "arguments": input,
        });
        match self.connection.request(methods::TOOLS_CALL, params).await {
            Ok(result) => rendered(result),
            Err(error) => ToolOutput::error(format!(
                "remote tool `{}`: {error}",
                self.name
            )),
        }
    }
}

// A plugin's result as tool output.
//
// A JSON string is the tool's text verbatim — the ordinary case, and the
// one a plugin author expects; any other JSON value is rendered as JSON,
// so a structured result reaches the model intact enough to read.
fn rendered(result: Value) -> ToolOutput {
    match result {
        Value::String(text) => ToolOutput::text(text),
        other => ToolOutput::text(other.to_string()),
    }
}
