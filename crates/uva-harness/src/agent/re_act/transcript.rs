//! The run's durable surface: the messages the model sees, the
//! ordinal each one was written under, the tool calls that are still
//! open, and the user turns the open window holds back.
//!
//! One object owns all four because they are one thing. A message
//! without its ordinal, or an ordinal without its message, is a
//! surface the compaction cannot cite — and the two used to drift
//! apart whenever a call site remembered one of them and forgot the
//! other.

use std::{ops::Range, sync::Arc};

use uva_provider::{Content, ContentPart, Message, Role};
use uva_session::{ProvenancedHistory, surface::ModelOrder};

/// The message list one request sends.
///
/// A request is a per-step projection of the transcript: the system
/// prompt, the part of the surface the context window keeps, this
/// turn's bytes and the hints. It is not the transcript — it carries
/// no provenance, and the loop drops it when the step ends.
pub(crate) type RequestMessages = Arc<Vec<Message>>;

/// The run's durable surface.
///
/// The messages the model sees and the ordinal each one was written
/// under. The two are index-parallel by construction: the fields are
/// private, so the only way to grow the surface is
/// [`Transcript::append_message`], and it grows both.
///
/// The surface also owns the tool-call window and the user turns the
/// window holds back: a request may not interleave a user turn
/// between a `tool_use` and its `tool_result`, and that rule belongs
/// with the messages it constrains. The rule itself is
/// [`ModelOrder`]’s — the durable fold applies the same one, and the
/// two surfaces must agree: a compaction's span is a position in the
/// order they share.
pub(crate) struct Transcript {
    // The messages, in the order the model sees them.
    messages: Vec<Message>,
    // The ordinal each message was written under, index-parallel with
    // `messages`. `0` means the message has no row of its own — an
    // inbox turn the host already wrote, whose ordinal this loop does
    // not know — and such a message is never cited as compaction
    // provenance.
    surface_seqs: Vec<u64>,
    // The order's rule: the open calls and the user turns they hold
    // back.
    order: ModelOrder<Message>,
    // Whether the ordinals this surface was seeded with name the row
    // each message came from, one row each. A surface they do not
    // describe has no provenance to cite, so a compaction must not
    // plan over it.
    citable: bool,
}

impl Transcript {
    /// A surface with no messages.
    pub(crate) fn empty() -> Self {
        Self {
            messages: Vec::new(),
            surface_seqs: Vec::new(),
            order: ModelOrder::new(),
            citable: false,
        }
    }

    /// The surface a run starts from: a projection of the durable log.
    ///
    /// The messages and their ordinals arrive together, so the surface
    /// and its provenance cannot disagree — including where the
    /// projection had to move a message: a steer released after the
    /// tool results keeps the ordinal of its own row at its new
    /// position.
    ///
    /// A history that is not one message per folded row carries no
    /// positional provenance to cite — the legacy one-part-per-row
    /// dialect folds one turn into several rows and the projection
    /// merges them into the one message a provider accepts — so it
    /// keeps its messages and cites nothing.
    pub(crate) fn from_projection(history: ProvenancedHistory) -> Self {
        let citable = history.is_one_message_per_row();
        let surface_seqs = if citable {
            history.ordinals().to_vec()
        } else {
            vec![0; history.messages().len()]
        };
        Self {
            messages: history.into_messages(),
            surface_seqs,
            order: ModelOrder::new(),
            citable,
        }
    }

    /// The surface a run without a log starts from.
    ///
    /// The messages have no durable row behind them, so a compaction
    /// can cite nothing: there is no log to rewrite.
    pub(crate) fn from_messages(messages: Vec<Message>) -> Self {
        Self {
            surface_seqs: vec![0; messages.len()],
            messages,
            order: ModelOrder::new(),
            citable: false,
        }
    }

    /// The assistant turn's one message: the single value both the
    /// durable row and the in-memory surface carry.
    ///
    /// This is the only construction of an assistant turn the loop
    /// uses. `Content::parts` decides the shape — a lone part is
    /// `Single` — and since the next request sends the message the
    /// transcript holds, that is the shape the log row keeps too:
    /// one construction, two homes, the same bytes.
    pub(crate) fn assistant_message(parts: Vec<ContentPart>) -> Message {
        Message::new(Role::Assistant, Content::parts(parts))
    }

    /// Whether the surface's ordinals may be cited by a compaction.
    ///
    /// A compaction names the rows it shadows, so it may only plan
    /// over a surface whose ordinals name its messages' rows. A
    /// surface seeded without a log, or from a history the legacy
    /// one-part-per-row dialect collapsed, has no such ordinals. A
    /// surface seeded from a log is paired by construction: a message
    /// the projection moved — a steer released after the tool results
    /// — carries its own ordinal, so the length of one list no longer
    /// says anything about the other.
    pub(crate) fn is_citable(&self) -> bool {
        self.citable
    }

    /// The messages, in the order the model sees them.
    pub(crate) fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The ordinal each message was written under, index-parallel with
    /// [`Self::messages`].
    pub(crate) fn surface_seqs(&self) -> &[u64] {
        &self.surface_seqs
    }

    /// Append a message and the ordinal it was written under.
    ///
    /// The two arrive together, so a message can never enter the
    /// surface without its provenance — or with someone else's.
    pub(crate) fn append_message(&mut self, message: Message, seq: u64) {
        self.messages.push(message);
        self.surface_seqs.push(seq);
    }

    /// Open a tool call: the window the next user turn has to wait
    /// for.
    pub(crate) fn open_call(&mut self, id: String) {
        self.order.open_calls([id]);
    }

    /// Answer a tool call. The window closes when the last open call
    /// is answered, and the user turns it held back are released.
    pub(crate) fn answer_call(&mut self, id: &str) {
        let released = self.order.answer_calls([id.to_string()]);
        self.append_released(released);
    }

    /// Deliver a user turn the host wrote while the run was in flight.
    ///
    /// The host already wrote the row, so the turn carries no ordinal
    /// of its own (`0`): it is never cited as compaction provenance.
    /// A turn that arrives while the window is open waits for the
    /// window to close — a request may not interleave a user turn
    /// between a `tool_use` and its `tool_result`.
    pub(crate) fn deliver_user(&mut self, message: Message) {
        if let Some(message) = self.order.admit(message) {
            self.append_message(message, 0);
        }
    }

    /// Whether a user turn is held back right now.
    ///
    /// A surface that holds one is not final: the turn joins it when
    /// the open call is answered, so a compaction must not plan over
    /// it — the durable fold refuses such a compaction too.
    pub(crate) fn holds_turn(&self) -> bool {
        self.order.is_holding()
    }

    // Add the released turns to the surface. A released turn has no
    // ordinal of its own: the host wrote its row, and this loop does
    // not know which one.
    fn append_released(&mut self, released: Vec<Message>) {
        for message in released {
            self.append_message(message, 0);
        }
    }

    /// Replace a span of the surface with the summary that stands for
    /// it.
    ///
    /// The summary takes the compaction row's ordinal, so the next
    /// plan sees the same positions the log's fold would reproduce —
    /// the span and its provenance move together.
    pub(crate) fn splice_compaction(
        &mut self,
        range: Range<usize>,
        replacement: Message,
        seq: u64,
    ) {
        self.messages.splice(range.clone(), [replacement]);
        self.surface_seqs.splice(range, [seq]);
    }

    /// Clear the reasoning signatures a compaction invalidated.
    ///
    /// A signed reasoning block is bound to the request prefix it was
    /// minted under — the system prompt, the tools and every earlier
    /// message. A compaction **rewrites** that prefix, so a signature
    /// that outlived one is void: a provider that enforces the
    /// binding rejects the whole request, not just the block. The
    /// signature is therefore cleared, which leaves the part unsigned
    /// — and an unsigned reasoning part is one the provider's own
    /// rule drops, so the turn keeps its text and loses the
    /// reasoning.
    ///
    /// `boundary` is the ordinal of the compaction: every message
    /// written before it had its prefix rewritten. The rule is
    /// deliberately coarse — it also voids the signatures of the
    /// messages *before* the replaced span, whose prefix did not
    /// change. Voiding a valid signature costs the model some
    /// context; keeping a void one costs the turn.
    ///
    /// Returns how many signatures were voided.
    pub(crate) fn void_signatures_before(&mut self, boundary: u64) -> usize {
        let mut voided = 0;
        for (index, seq) in self.surface_seqs.iter().enumerate() {
            if *seq >= boundary {
                continue;
            }
            let Some(message) = self.messages.get_mut(index) else {
                continue;
            };
            voided += clear_reasoning_signatures(&mut message.content);
        }
        voided
    }
}

// Clear the signature of every reasoning part of `content`.
//
// A `Content` is one part or many; both shapes are walked, and the
// count of the parts that carried a signature is returned.
fn clear_reasoning_signatures(content: &mut Content) -> usize {
    let mut voided = 0;
    let parts = match content {
        Content::Single(part) => std::slice::from_mut(part),
        Content::Multi(parts) => parts.as_mut_slice(),
    };
    for part in parts {
        if let ContentPart::Reasoning(reasoning) = part
            && reasoning.signature.take().is_some()
        {
            voided += 1;
        }
    }
    voided
}

#[cfg(test)]
mod tests {
    use uva_provider::{Content, ContentPart, Role};

    use super::*;

    // The surface and its provenance are one list of two: every
    // append path adds one to each, so neither can run ahead of the
    // other. The check runs after every path, not once at the end —
    // a path that forgets its ordinal is caught at the path that
    // forgets it.
    fn assert_in_step(transcript: &Transcript) {
        assert_eq!(
            transcript.messages().len(),
            transcript.surface_seqs().len(),
            "the surface and its provenance must stay in step"
        );
    }

    #[test]
    fn an_appended_message_carries_its_ordinal() {
        let mut transcript = Transcript::empty();
        assert_in_step(&transcript);

        transcript.append_message(Message::user("hello"), 7);
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.surface_seqs(), &[7]);

        transcript.append_message(Message::assistant("hi"), 8);
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 2);
        assert_eq!(transcript.surface_seqs(), &[7, 8]);
    }

    // A compaction voids the signatures minted before it — and only
    // those: a message written after the compaction kept its prefix.
    #[test]
    fn a_compaction_voids_only_the_signatures_before_it() {
        let mut transcript = Transcript::empty();
        transcript.append_message(signed_reasoning(5), 5);
        transcript.append_message(signed_reasoning(9), 9);

        let voided = transcript.void_signatures_before(7);
        assert_eq!(voided, 1, "one signature was void");
        let signatures: Vec<Option<String>> = transcript
            .messages()
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|part| match part {
                ContentPart::Reasoning(reasoning) => {
                    Some(reasoning.signature.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            signatures,
            vec![None, Some("sig".to_string())],
            "the message written before the compaction lost its \
             signature; the one after it kept it"
        );
    }

    // An assistant turn carrying one signed reasoning part.
    fn signed_reasoning(seq: u64) -> Message {
        let _ = seq;
        Message::new(
            Role::Assistant,
            Content::parts(vec![ContentPart::Reasoning(
                uva_provider::ReasoningContent {
                    text: "thinking".to_string(),
                    signature: Some("sig".to_string()),
                },
            )]),
        )
    }

    // A request may not interleave a user turn between a `tool_use`
    // and its `tool_result`, so a turn that arrives while the window
    // is open waits for the window to close.
    #[test]
    fn a_deferred_user_turn_waits_for_the_call_window_to_close() {
        let mut transcript = Transcript::empty();
        transcript.open_call("call-1".to_string());
        transcript.deliver_user(Message::user("steer"));

        // The window is open: the turn must not reach the surface.
        assert_in_step(&transcript);
        assert_eq!(
            transcript.messages().len(),
            0,
            "the turn waits while the call window is open"
        );
        assert!(
            transcript.holds_turn(),
            "the surface is not final while the turn waits"
        );

        // The window closes: the turn lands, in the order it arrived.
        transcript.answer_call("call-1");
        assert_in_step(&transcript);
        assert!(
            !transcript.holds_turn(),
            "the window closed, so the turn is on the surface"
        );
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.messages()[0], Message::user("steer"));
        assert_eq!(
            transcript.surface_seqs(),
            &[0],
            "the host wrote the row, so the turn has no ordinal of its own"
        );
    }

    // A turn that arrives while no call is open goes straight to the
    // surface.
    #[test]
    fn a_user_turn_arrives_while_no_call_is_open() {
        let mut transcript = Transcript::empty();
        transcript.deliver_user(Message::user("hello"));
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.messages()[0], Message::user("hello"));
    }

    // A compaction replaces a span of the surface with the summary
    // that stands for it, and the summary takes the compaction row's
    // ordinal.
    #[test]
    fn a_compaction_replaces_a_span_and_its_provenance() {
        let mut transcript = Transcript::empty();
        transcript.append_message(Message::user("one"), 1);
        transcript.append_message(Message::user("two"), 2);
        transcript.append_message(Message::user("three"), 3);

        transcript.splice_compaction(1..3, Message::assistant("summary"), 4);
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 2);
        assert_eq!(transcript.messages()[1], Message::assistant("summary"));
        assert_eq!(
            transcript.surface_seqs(),
            &[1, 4],
            "the summary takes the compaction row's ordinal"
        );
    }

    // Every path that grows the surface, in the order the loop takes
    // them: an assistant turn, a user turn the open window holds
    // back, the tool result that closes the window, and a
    // compaction's replace. The surface and its provenance stay in
    // step after each one.
    #[test]
    fn every_append_path_keeps_the_surface_and_its_provenance_in_step() {
        let mut transcript = Transcript::empty();
        assert_in_step(&transcript);

        // An assistant turn that opens the call window.
        transcript.open_call("call-1".to_string());
        transcript.append_message(Message::assistant("working"), 1);
        assert_in_step(&transcript);

        // A user turn that arrives while the window is open: it waits.
        transcript.deliver_user(Message::user("steer"));
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 1);

        // The tool result that answers the call: the window closes and
        // the waiting turn is released.
        transcript
            .append_message(Message::tool(Content::text("done"), "call-1"), 2);
        transcript.answer_call("call-1");
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 3);

        // A compaction's replace.
        transcript.splice_compaction(0..3, Message::assistant("summary"), 3);
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.surface_seqs(), &[3]);
    }

    // One log row: the payload the projection decodes from it.
    fn message_row(
        seq: u64,
        tag: &str,
        message: &Message,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": tag,
            "seq": seq,
            "data": serde_json::to_value(message)
                .expect("a message serializes"),
        })
    }

    // A legacy `Model` row: one streamed part, the pre-typed dialect.
    fn part_row(seq: u64, call_id: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "Model",
            "seq": seq,
            "data": {"type": "tool_use", "id": call_id, "name": "read",
                     "input": {"path": "a.rs"}},
        })
    }

    fn projected(rows: Vec<serde_json::Value>) -> Transcript {
        Transcript::from_projection(
            ProvenancedHistory::from_log(&rows).expect("the rows project"),
        )
    }

    // The ordinary seed: the surface keeps the ordinals it was folded
    // with.
    #[test]
    fn a_surface_keeps_the_ordinals_it_was_folded_with() {
        let transcript = projected(vec![
            message_row(3, "user_message", &Message::user("one")),
            message_row(4, "assistant_message", &Message::assistant("two")),
        ]);
        assert_in_step(&transcript);
        assert_eq!(
            transcript.messages(),
            &[Message::user("one"), Message::assistant("two")]
        );
        assert_eq!(transcript.surface_seqs(), &[3, 4]);
        assert!(transcript.is_citable());
    }

    // A steer row between a `tool_use` and its `tool_result`: the
    // projection releases the message after the result, and the
    // ordinal that rides with it is the steer's own. The surface and
    // its provenance stay paired through the move.
    #[test]
    fn a_seeded_surface_pairs_a_steer_with_its_own_row() {
        let tool_use =
            Transcript::assistant_message(vec![ContentPart::ToolUse(
                uva_provider::ToolUse {
                    id: "call-1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "a.rs"}),
                },
            )]);
        let transcript = projected(vec![
            message_row(1, "user_message", &Message::user("start")),
            message_row(2, "assistant_message", &tool_use),
            message_row(3, "user_message", &Message::user("steer")),
            message_row(
                4,
                "tool_result",
                &Message::tool(Content::text("ok"), "call-1"),
            ),
        ]);
        assert_in_step(&transcript);
        assert_eq!(
            transcript.surface_seqs(),
            &[1, 2, 4, 3],
            "the result keeps row 4, the steer keeps row 3"
        );
        assert_eq!(transcript.messages()[2].role, Role::Tool);
        assert_eq!(transcript.messages()[3], Message::user("steer"));
        assert!(transcript.is_citable());
    }

    // A history the legacy one-part-per-row dialect collapsed has no
    // positional provenance: two `tool_use` rows become the one
    // assistant turn a provider accepts, so a position in this surface
    // is not the position of a row.
    #[test]
    fn a_surface_the_legacy_dialect_collapsed_cites_nothing() {
        let transcript =
            projected(vec![part_row(1, "call_a"), part_row(2, "call_b")]);
        assert_in_step(&transcript);
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.surface_seqs(), &[0]);
        assert!(
            !transcript.is_citable(),
            "a plan over this surface would name rows it does not shadow"
        );
    }
}
