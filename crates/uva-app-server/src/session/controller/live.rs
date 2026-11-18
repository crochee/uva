//! The in-progress step's transient projection.
//!
//! The durable log is the transcript's source of truth, but a step's
//! rows land at its boundaries: while the model streams, the only
//! evidence is the event tap. [`LiveTurn`] is that evidence — text,
//! reasoning and tool calls accumulated from the run's events — and
//! it is never written to disk.
//!
//! A projection belongs to one **step** (one model call): the next
//! step opens a new projection, and the previous step's content is
//! the durable rows' by then. Its identity is the log position where
//! the step began, so a client can tell which projection a durable row
//! supersedes by comparing ordinals — no id to match, no text to
//! guess. A process that dies mid-step therefore loses the projection
//! and leaves no half-written assistant row: the next run starts from
//! `turn/start`, which is the "one row per turn" trade-off.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use uva::{
    harness::{AgentEvent, SystemEvent},
    provider::{ContentPart, ToolResult},
};

/// The stack of a running call's live preview, in bytes of text.
///
/// A streamed call appends every chunk to the frame the client
/// holds; without a window the frame grows with the call's **total**
/// output, and a `follow` stream sends the accumulated text once per
/// chunk — O(n²) traffic for an n-byte capture. The window keeps the
/// newest bytes (the part a watcher is reading) and makes every frame
/// O(window). The projection's frame counter counts the streamed
/// bytes separately, so a chunk that lands after the window is full
/// still advances `live_seq` even though the carried text stopped
/// growing.
pub const MAX_LIVE_TOOL_OUTPUT_BYTES: usize = 32 * 1024;

/// Keep the last `max_bytes` bytes of `text`, on a char boundary.
fn tail_window(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let cut = text.len() - max_bytes;
    let from = (cut..text.len())
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or(text.len());
    text[from..].to_string()
}

/// One tool call the in-progress turn has produced.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LiveToolCall {
    /// The provider's call id.
    pub call_id: String,
    /// The tool the call addresses.
    pub name: String,
    /// The arguments the call was made with.
    pub arguments: Value,
    /// `running`, `completed` or `failed`.
    pub status: &'static str,
    /// The call's output: while it runs, the streamed chunks appended
    /// in arrival order; once it settles, the result text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// How many output bytes the call has streamed, including the
    /// ones the window dropped.
    ///
    /// Not on the wire: it is the projection's own fingerprint
    /// dimension, the one that keeps a same-length chunk advancing
    /// `live_seq` after the carried text reached its window.
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) streamed_bytes: u64,
}

/// The transient projection of the step that is running (or just
/// ended) in this process.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LiveTurn {
    /// The log position where this projection's step began.
    ///
    /// A durable content row whose ordinal is greater than this
    /// supersedes the projection: the row is that step's own answer.
    /// `0` means "no position" — a projection whose step began before
    /// this process knew the log's numbering (a remote owner's step,
    /// projected here).
    pub start_seq: u64,
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
    /// Whether the next model part opens a new step.
    ///
    /// Not on the wire: it is the projection's own bookkeeping about
    /// where it stands — the way the tool call's streamed byte count
    /// is.
    #[serde(skip)]
    #[schemars(skip)]
    step_open: bool,
}

impl Default for LiveTurn {
    fn default() -> Self {
        Self {
            start_seq: 0,
            live_seq: 0,
            running: false,
            text: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            step_open: true,
        }
    }
}

impl LiveTurn {
    /// Start a fresh projection for a new run.
    pub(super) fn reset(&mut self) {
        self.live_seq += 1;
        self.running = true;
        self.text.clear();
        self.reasoning.clear();
        self.tool_calls.clear();
        self.start_seq = 0;
        self.step_open = true;
    }

    /// Open the step a model part belongs to.
    ///
    /// The step's boundary is the model's own: a part after the
    /// previous response's end opens the next step. The projection's
    /// content is that step's alone — the durable row that lands at
    /// the step's end supersedes it — so the content clears here, and
    /// the projection's position becomes the log's position at this
    /// moment: the step's rows are flushed before its events are
    /// emitted, so this is where the step's content starts.
    fn open_step(&mut self, log_seq: u64) {
        if !self.step_open {
            return;
        }
        self.step_open = false;
        self.start_seq = log_seq;
        self.text.clear();
        self.reasoning.clear();
        self.tool_calls.clear();
    }

    /// Fold one run event into the projection, with no log position.
    ///
    /// A projection of another node's run carries no position: the
    /// owner's log numbering is not this node's to publish, so a client
    /// falls back to "any content row supersedes it".
    pub fn apply(&mut self, event: &AgentEvent) {
        self.apply_at(event, 0);
    }

    /// Fold one run event into the projection, stamping the log
    /// position its step began at.
    ///
    /// The same reducer serves the owner (which feeds it from the
    /// run's event tap) and a non-owner `follow` (which feeds it the
    /// events the tail bridge decoded from the owner's frames), so
    /// both sides project a run identically.
    ///
    /// `log_seq` is the log's durable position as of this event: the
    /// step that opens here began at that position, and it is what
    /// makes the projection's identity durable rather than arbitrary.
    pub fn apply_at(&mut self, event: &AgentEvent, log_seq: u64) {
        let before = self.signature();
        match event {
            AgentEvent::Model(ContentPart::Text(text)) => {
                self.open_step(log_seq);
                self.text.push_str(&text.text);
            }
            AgentEvent::Model(ContentPart::Reasoning(reasoning)) => {
                self.open_step(log_seq);
                self.reasoning.push_str(&reasoning.text);
            }
            AgentEvent::Model(ContentPart::ToolUse(call)) => {
                self.open_step(log_seq);
                self.ensure_call(call, "running");
            }
            AgentEvent::Model(ContentPart::ToolResult(result)) => {
                self.finish_result(result);
            }
            AgentEvent::ModelDone(result) => {
                // A response with no content opens no step: an empty
                // answer is the harness's "nothing more" signal, and
                // it must not wipe the step that is still live.
                let has_content = !result.text.is_empty()
                    || !result.reasoning.is_empty()
                    || !result.tool_calls.is_empty();
                if has_content {
                    // A provider that never streamed opens the step
                    // here: its answer arrives whole.
                    self.open_step(log_seq);
                    // A provider that streamed deltas has already put
                    // the text on the projection; only the unstreamed
                    // tail is added (the harness dedups the same way).
                    if !result.text.is_empty()
                        && !self.text.ends_with(&result.text)
                    {
                        self.text.push_str(&result.text);
                    }
                    // Same rule as the text: a provider that streamed
                    // reasoning deltas already put them on the
                    // projection.
                    if !result.reasoning.is_empty()
                        && !self.reasoning.ends_with(&result.reasoning)
                    {
                        self.reasoning.push_str(&result.reasoning);
                    }
                    for call in &result.tool_calls {
                        self.ensure_call(call, "running");
                    }
                }
                // The response is over: the next model part opens the
                // next step, and that step's projection is its own.
                self.step_open = true;
            }
            AgentEvent::System(SystemEvent::ToolProgress {
                tool_name,
                call_id,
                output,
            }) => {
                self.append_progress(tool_name, call_id, output);
            }
            AgentEvent::System(SystemEvent::SessionStarted { .. }) => {
                // A new run on this stream: the projection starts
                // from scratch, exactly as the owner's tap does.
                self.reset();
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
    /// the call count, the settled statuses, the output sizes
    /// **carried**, the output bytes **streamed**, and whether the
    /// turn is live.
    ///
    /// A running call's streamed chunks extend its output — and the
    /// window bounds that output, so once it fills, the carried size
    /// stops moving. The cumulative `streamed_bytes` is what keeps a
    /// chunk advancing `live_seq` after the window is full: two chunks
    /// of the same length always add to it, whatever the window kept.
    ///
    /// A tool that settles without changing any of the first three, or
    /// a run that ends without adding content, must still bump the
    /// frame counter; the status is counted as its own dimension, not
    /// as its length: `running` and `failed` differ by one character,
    /// so a length alone would let a call that settles with a
    /// one-character answer keep the frame the client already holds.
    fn signature(&self) -> (u64, usize, usize, usize, usize, usize, u64, bool) {
        let mut settled = 0usize;
        let mut output_bytes = 0usize;
        let mut streamed_bytes = 0u64;
        for call in &self.tool_calls {
            settled += match call.status {
                "completed" => 1,
                "failed" => 2,
                _ => 0,
            };
            output_bytes += call.output.as_deref().map(str::len).unwrap_or(0);
            streamed_bytes = streamed_bytes.saturating_add(call.streamed_bytes);
        }
        (
            self.start_seq,
            self.text.len(),
            self.reasoning.len(),
            self.tool_calls.len(),
            settled,
            output_bytes,
            streamed_bytes,
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
            streamed_bytes: 0,
        });
    }

    /// Append one streamed chunk to the call it belongs to.
    ///
    /// A progress chunk is **not** a result: the call is still
    /// running, and the chunk only extends what the client already
    /// holds. Settling here would mark the call done before its result
    /// exists — and would replace the output with the newest chunk, so
    /// two chunks of the same length would neither add content nor
    /// advance the frame counter. A call the projection never saw
    /// proposed (a resumed dispatch) is recorded as running, because a
    /// chunk is evidence the tool runs.
    ///
    /// The accumulated text is a **tail window**
    /// ([`MAX_LIVE_TOOL_OUTPUT_BYTES`]): a frame carries the newest
    /// bytes, so its size is bounded by the window instead of by the
    /// call's total output, and a `follow` stream is O(n) traffic
    /// rather than O(n²). The chunk still counts toward the
    /// projection's `streamed_bytes`, so dropping old bytes never
    /// freezes the frame counter.
    fn append_progress(
        &mut self,
        tool_name: &str,
        call_id: &str,
        output: &uva::tool::ToolOutput,
    ) {
        let chunk = output
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
            call.streamed_bytes =
                call.streamed_bytes.saturating_add(chunk.len() as u64);
            let mut accumulated = call.output.take().unwrap_or_default();
            accumulated.push_str(&chunk);
            call.output =
                Some(tail_window(accumulated, MAX_LIVE_TOOL_OUTPUT_BYTES));
            return;
        }
        self.tool_calls.push(LiveToolCall {
            call_id: call_id.to_string(),
            name: tool_name.to_string(),
            arguments: Value::Null,
            status: "running",
            output: Some(tail_window(
                chunk.clone(),
                MAX_LIVE_TOOL_OUTPUT_BYTES,
            )),
            streamed_bytes: chunk.len() as u64,
        });
    }

    /// Settle the call a tool result answers.
    ///
    /// The harness publishes every tool result -- a success, a
    /// failure and a veto alike -- as a model part, and the durable
    /// `tool_result` row is written from the same output. Reading
    /// the result here is what keeps the live transcript and the
    /// reloaded one telling the same story: a call that has been
    /// answered is not `running` any more.
    fn finish_result(&mut self, result: &ToolResult) {
        let failed = result.is_error.unwrap_or(false);
        let text = result
            .content
            .iter()
            .filter_map(|part| part.text())
            .collect::<Vec<_>>()
            .join("");
        let name = result.tool_name.clone().unwrap_or_default();
        self.settle_call(&name, &result.tool_use_id, failed, text);
    }

    /// Record a call's settled state, whatever told us about it.
    ///
    /// The settled text is bounded by the same window the streamed
    /// preview uses, so a frame's size cannot follow a tool's output
    /// even on the settle.
    fn settle_call(
        &mut self,
        tool_name: &str,
        call_id: &str,
        failed: bool,
        text: String,
    ) {
        let status = if failed { "failed" } else { "completed" };
        let text = tail_window(text, MAX_LIVE_TOOL_OUTPUT_BYTES);
        if let Some(call) = self
            .tool_calls
            .iter_mut()
            .find(|known| known.call_id == call_id)
        {
            call.status = status;
            call.streamed_bytes = call
                .streamed_bytes
                .max(u64::try_from(text.len()).unwrap_or(u64::MAX));
            call.output = Some(text);
            return;
        }
        // An answer for a call the projection never saw proposed (a
        // resumed dispatch, a provider without deltas): record it
        // with its settled status rather than dropping it.
        let streamed_bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        self.tool_calls.push(LiveToolCall {
            call_id: call_id.to_string(),
            name: tool_name.to_string(),
            arguments: Value::Null,
            status,
            output: Some(text),
            streamed_bytes,
        });
    }
}

#[cfg(test)]
mod tests {
    use uva::{
        harness::{AgentEvent, SessionEndReason, SystemEvent},
        provider::{TextContent, ToolResult, ToolUse},
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

    /// A tool result answers a call: the call is done, and the
    /// projection has to say so.
    ///
    /// The harness publishes every tool result — a success, a failure
    /// and a veto alike — as a model part, so the projection learns
    /// the call's fate from the same event the durable row is written
    /// from. A call that has been answered is not `running` any more:
    /// the live transcript and the reloaded one must tell the same
    /// story about it.
    #[test]
    fn a_tool_result_settles_the_call_it_answers() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "src/lib.rs"}),
        })));
        assert_eq!(live.tool_calls[0].status, "running");

        live.apply(&AgentEvent::Model(ContentPart::ToolResult(
            ToolResult::error("c1", "denied by policy"),
        )));

        assert_eq!(
            live.tool_calls[0].status, "failed",
            "an answered call is not running any more"
        );
        assert_eq!(
            live.tool_calls[0].output.as_deref(),
            Some("denied by policy"),
            "the answer's text reaches the client"
        );
    }

    /// The same answer, this time from a tool that ran: the call is
    /// completed, not failed.
    #[test]
    fn a_successful_tool_result_completes_the_call() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "src/lib.rs"}),
        })));

        live.apply(&AgentEvent::Model(ContentPart::ToolResult(
            ToolResult::new("c1", "file body"),
        )));

        assert_eq!(live.tool_calls[0].status, "completed");
        assert_eq!(live.tool_calls[0].output.as_deref(), Some("file body"));
    }
    /// A settle must reach the client, whatever the answer's size.
    ///
    /// The frame counter advances on what the frame carries, so a
    /// settle whose answer happens to be as long as the status word
    /// it replaces must still advance it -- otherwise the client
    /// keeps the frame that reads `running`.
    #[test]
    fn a_settle_advances_the_frame_whatever_the_answer_says() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({}),
        })));
        let before = live.live_seq;

        live.apply(&AgentEvent::Model(ContentPart::ToolResult(
            ToolResult::error("c1", "x"),
        )));

        assert_eq!(live.tool_calls[0].status, "failed");
        assert!(
            live.live_seq > before,
            "the settle must advance the frame counter"
        );
    }

    /// A streamed chunk extends the call; it does not answer it.
    ///
    /// The shell tool yields its output in chunks while the command
    /// runs. Each chunk must append to what the client already holds
    /// and leave the call `running`; settling here would show the
    /// command as finished before its result exists. Two chunks of the
    /// same length must still move the frame counter, because the
    /// accumulated text is what the frame carries.
    #[test]
    fn progress_chunks_append_and_the_call_keeps_running() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "cargo build"}),
        })));
        let proposed = live.live_seq;

        live.apply(&AgentEvent::System(SystemEvent::ToolProgress {
            tool_name: "shell".to_string(),
            call_id: "c1".to_string(),
            output: uva::tool::ToolOutput::text("ab"),
        }));
        let first = live.live_seq;
        assert!(first > proposed, "a chunk advances the frame counter");
        live.apply(&AgentEvent::System(SystemEvent::ToolProgress {
            tool_name: "shell".to_string(),
            call_id: "c1".to_string(),
            output: uva::tool::ToolOutput::text("cd"),
        }));

        assert!(
            live.live_seq > first,
            "a second chunk of the same length still advances: the frame \
             carries the accumulated text"
        );
        assert_eq!(
            live.tool_calls[0].status, "running",
            "a chunk is not a result: the command still runs"
        );
        assert_eq!(
            live.tool_calls[0].output.as_deref(),
            Some("abcd"),
            "chunks append in arrival order"
        );

        // The result is still the one settle, and it replaces the
        // streamed preview with the tool's committed output.
        live.apply(&AgentEvent::Model(ContentPart::ToolResult(
            ToolResult::new("c1", "ab\ncd\n[exit code: 0]"),
        )));
        assert_eq!(live.tool_calls[0].status, "completed");
        assert_eq!(
            live.tool_calls[0].output.as_deref(),
            Some("ab\ncd\n[exit code: 0]")
        );
    }

    /// A long stream's frames stay bounded, and every chunk still
    /// advances the projection: the frame carries a **tail window**,
    /// while the fingerprint counts the bytes streamed. Two chunks of
    /// the same length with different content therefore move
    /// `live_seq` even after the window is full.
    #[test]
    fn a_long_progress_stream_is_bounded_and_still_advances() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({}),
        })));
        let chunk_a = "a".repeat(8 * 1024);
        let chunk_b = "b".repeat(8 * 1024);
        let mut previous = live.live_seq;
        for index in 0..12 {
            let chunk = if index % 2 == 0 { &chunk_a } else { &chunk_b };
            live.apply(&AgentEvent::System(SystemEvent::ToolProgress {
                tool_name: "shell".to_string(),
                call_id: "c1".to_string(),
                output: uva::tool::ToolOutput::text(chunk),
            }));
            assert!(
                live.live_seq > previous,
                "chunk {index} advances the frame, even once the window \
                 is full"
            );
            previous = live.live_seq;
            let output =
                live.tool_calls[0].output.as_deref().expect("a preview");
            assert!(
                output.len() <= MAX_LIVE_TOOL_OUTPUT_BYTES,
                "chunk {index} keeps the preview bounded: {} bytes",
                output.len()
            );
            let frame = serde_json::to_string(&live).expect("serialises");
            assert!(
                frame.len() <= 4 * MAX_LIVE_TOOL_OUTPUT_BYTES,
                "chunk {index} keeps the frame bounded: {} bytes",
                frame.len()
            );
        }
        let output = live.tool_calls[0].output.as_deref().expect("a preview");
        assert!(
            output.ends_with(&"b".repeat(64)),
            "the window holds the newest bytes"
        );
    }

    /// The settle is bounded by the same window: a huge result cannot
    /// grow a frame past it either.
    #[test]
    fn a_settled_result_is_bounded_to_the_same_window() {
        let mut live = LiveTurn::default();
        live.apply(&AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({}),
        })));
        let huge = "y".repeat(MAX_LIVE_TOOL_OUTPUT_BYTES * 2);
        live.apply(&AgentEvent::Model(ContentPart::ToolResult(
            ToolResult::new("c1", &huge),
        )));

        assert_eq!(live.tool_calls[0].status, "completed");
        let output = live.tool_calls[0].output.as_deref().expect("the result");
        assert!(
            output.len() <= MAX_LIVE_TOOL_OUTPUT_BYTES,
            "the settled preview is bounded: {} bytes",
            output.len()
        );
        assert!(output.ends_with(&"y".repeat(64)), "the tail survives");
    }
}
