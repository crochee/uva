//! The in-progress turn's transient projection.
//!
//! The durable log is the transcript's source of truth, but a turn's
//! rows land at its boundaries: while the model streams, the only
//! evidence is the event tap. [`LiveTurn`] is that evidence — text,
//! reasoning and tool calls accumulated from the run's events — and
//! it is never written to disk.
//!
//! A `follow` client renders it until the durable row for the turn
//! arrives, at which point the row supersedes the projection. A
//! process that dies mid-turn therefore loses the projection and
//! leaves no half-written assistant row: the next run starts from
//! `turn/start`, which is the "one row per turn" trade-off.

use serde::Serialize;
use serde_json::Value;
use uva::{
    harness::{AgentEvent, SystemEvent},
    provider::ContentPart,
};

/// One tool call the in-progress turn has produced.
#[derive(Debug, Clone, Serialize)]
pub struct LiveToolCall {
    /// The provider's call id.
    pub call_id: String,
    /// The tool the call addresses.
    pub name: String,
    /// The arguments the call was made with.
    pub arguments: Value,
    /// `running`, `completed` or `failed`.
    pub status: &'static str,
    /// The result text once the call settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// The transient projection of the turn that is running (or just
/// ended) in this process.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LiveTurn {
    /// Identifies the turn the projection belongs to. A fresh run gets
    /// a fresh id, so a client can tell a superseded projection from
    /// the current one.
    pub turn_id: String,
    /// Monotone per-controller frame counter. A baseline carries the
    /// current value; a client discards an increment whose `live_seq`
    /// is not greater than its baseline.
    pub live_seq: u64,
    /// Whether the producing run is still live.
    pub running: bool,
    /// Text streamed (and committed) so far.
    pub text: String,
    /// Reasoning streamed so far.
    pub reasoning: String,
    /// Tool calls proposed by the turn, in call order.
    pub tool_calls: Vec<LiveToolCall>,
}

impl LiveTurn {
    /// Start a fresh projection for a new run.
    pub(super) fn reset(&mut self, turn_id: String) {
        self.turn_id = turn_id;
        self.live_seq += 1;
        self.running = true;
        self.text.clear();
        self.reasoning.clear();
        self.tool_calls.clear();
    }

    /// Fold one run event into the projection.
    ///
    /// The same reducer serves the owner (which feeds it from the
    /// run's event tap) and a non-owner `follow` (which feeds it the
    /// events the tail bridge decoded from the owner's frames), so
    /// both sides project a run identically.
    pub fn apply(&mut self, event: &AgentEvent) {
        let before = self.signature();
        match event {
            AgentEvent::Model(ContentPart::Text(text)) => {
                self.text.push_str(&text.text);
            }
            AgentEvent::Model(ContentPart::Reasoning(reasoning)) => {
                self.reasoning.push_str(&reasoning.text);
            }
            AgentEvent::Model(ContentPart::ToolUse(call)) => {
                self.ensure_call(call, "running");
            }
            AgentEvent::ModelDone(result) => {
                // A provider that streamed deltas has already put the
                // text on the projection; only the unstreamed tail is
                // added (the harness dedups the same way).
                if !result.text.is_empty() && !self.text.ends_with(&result.text)
                {
                    self.text.push_str(&result.text);
                }
                // Same rule as the text: a provider that streamed
                // reasoning deltas already put them on the projection.
                if !result.reasoning.is_empty()
                    && !self.reasoning.ends_with(&result.reasoning)
                {
                    self.reasoning.push_str(&result.reasoning);
                }
                for call in &result.tool_calls {
                    self.ensure_call(call, "running");
                }
            }
            AgentEvent::System(SystemEvent::ToolProgress {
                tool_name,
                call_id,
                output,
            }) => {
                self.finish_call(tool_name, call_id, output);
            }
            AgentEvent::System(SystemEvent::SessionStarted { session_id }) => {
                // A new run on this stream: the projection starts
                // from scratch, exactly as the owner's tap does.
                self.reset(format!("remote:{session_id}"));
            }
            AgentEvent::System(SystemEvent::SessionEnded { .. })
            | AgentEvent::System(SystemEvent::Suspended(_)) => {
                self.running = false;
            }
            _ => return,
        }
        if self.signature() != before {
            self.live_seq += 1;
        }
    }

    /// Everything a frame carries, cheaply: the text/reasoning lengths,
    /// the call count, the settled status/output sizes, and whether the
    /// turn is live. A tool that settles without changing any of the
    /// first three, or a run that ends without adding content, must
    /// still bump the frame counter.
    fn signature(&self) -> (usize, usize, usize, usize, bool) {
        let tool_bytes = self
            .tool_calls
            .iter()
            .map(|call| {
                call.status.len()
                    + call.output.as_deref().map(str::len).unwrap_or(0)
            })
            .sum();
        (
            self.text.len(),
            self.reasoning.len(),
            self.tool_calls.len(),
            tool_bytes,
            self.running,
        )
    }

    fn ensure_call(
        &mut self,
        call: &uva::provider::ToolUse,
        status: &'static str,
    ) {
        if self.tool_calls.iter().any(|known| known.call_id == call.id) {
            return;
        }
        self.tool_calls.push(LiveToolCall {
            call_id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.input.clone(),
            status,
            output: None,
        });
    }

    fn finish_call(
        &mut self,
        tool_name: &str,
        call_id: &str,
        output: &uva::tool::ToolOutput,
    ) {
        let failed = output.is_error.unwrap_or(false);
        let text = output
            .content
            .iter()
            .filter_map(|part| part.text())
            .collect::<Vec<_>>()
            .join("");
        if let Some(call) = self
            .tool_calls
            .iter_mut()
            .find(|known| known.call_id == call_id)
        {
            call.status = if failed { "failed" } else { "completed" };
            call.output = Some(text);
            return;
        }
        // A progress frame for a call the projection never saw
        // proposed (a resumed dispatch, a provider without deltas):
        // record it with its settled status rather than dropping it.
        self.tool_calls.push(LiveToolCall {
            call_id: call_id.to_string(),
            name: tool_name.to_string(),
            arguments: Value::Null,
            status: if failed { "failed" } else { "completed" },
            output: Some(text),
        });
    }
}

#[cfg(test)]
mod tests {
    use uva::{
        harness::{AgentEvent, SessionEndReason, SystemEvent},
        provider::TextContent,
    };

    use super::*;

    /// The same reducer serves the owner and a follower: feeding the
    /// owner's event sequence into a fresh projection yields the
    /// transient turn a `follow` stream on another node renders.
    #[test]
    fn a_follower_projects_the_owners_events() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::System(SystemEvent::SessionStarted {
            session_id: "session-1".to_string(),
        }));
        assert!(live.running);
        live.apply(&AgentEvent::Model(ContentPart::Text(TextContent {
            text: "hello".to_string(),
            cache_control: None,
        })));
        assert_eq!(live.text, "hello");
        let after_text = live.live_seq;
        live.apply(&AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }));
        assert!(!live.running);
        assert!(
            live.live_seq > after_text,
            "the terminal event advances the projection"
        );
    }
}
