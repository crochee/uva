//! The result transformer: one `synthia/transform` round trip per tool
//! result, for a plugin whose config and handshake both enable it.
//!
//! The matrix grants "rewrite a tool result" to the *default*
//! authority — it changes what one result says, not whether the call
//! happens — so a [`Authority::Reduced`](crate::Authority::Reduced)
//! plugin gets this registration too.
//!
//! This type cannot audit its own rewrite: the trait hands it no
//! context and no writer. It rewrites; the harness measures the
//! aggregate change and writes the durable `tool_result_transformed`
//! row at the call site, which runs before the loop commits the result.
//! The row is provenance — the log already holds the rewritten text.
//!
//! A failed transform costs a diagnostic, not the tool's result: the
//! output keeps what the tool returned, and the host records the
//! failure. The deadline is the connection's, so a wedged plugin costs
//! one timeout and is killed rather than stalling the turn.

use std::sync::Arc;

use async_trait::async_trait;
use synthia_harness::ResultTransformer;
use synthia_plugin_protocol::methods;
use synthia_tool::ToolOutput;

use crate::client::Connection;

/// One plugin's rewrite of a tool result.
pub(crate) struct RemoteTransformer {
    name: String,
    connection: Arc<Connection>,
}

impl RemoteTransformer {
    pub(crate) fn new(name: String, connection: Arc<Connection>) -> Self {
        Self { name, connection }
    }
}

#[async_trait]
impl ResultTransformer for RemoteTransformer {
    fn name(&self) -> &str {
        &self.name
    }

    async fn transform(&self, output: &mut ToolOutput) -> Result<(), String> {
        // The plugin reads the result as JSON and answers with the
        // rewritten result JSON. Nothing is written until the answer
        // parsed, so a failed rewrite leaves the tool's own output.
        let sent = serde_json::to_value(&*output).map_err(|error| {
            format!(
                "`{}`: the tool result is not serialisable: {error}",
                methods::TRANSFORM
            )
        })?;
        let answer = self
            .connection
            .request(methods::TRANSFORM, sent)
            .await
            .map_err(|error| error.to_string())?;
        // The whole output is replaced, `is_error` included: rewriting
        // the content is the granted power, and a plugin that could lie
        // about the text could lie about the flag just as easily, so
        // forcing it monotonic would buy nothing.
        let rewritten: ToolOutput =
            serde_json::from_value(answer).map_err(|error| {
                format!(
                    "`{}` answered with something that is not a tool \
                     result: {error}",
                    methods::TRANSFORM
                )
            })?;
        *output = rewritten;
        Ok(())
    }
}
