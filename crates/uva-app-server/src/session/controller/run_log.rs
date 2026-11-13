//! One run's handle on the session's writer, plus the helpers
//! that shape prompt text for persistence.

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;
use uva::{
    attachment::AttachmentStore,
    provider::{ContentPart, Role},
    session::{AttachmentRefRow, LogWriter, SessionError},
};

/// One run's handle on the session's one writer.
///
/// Every row the run persists goes through here — the writer is
/// the session's, so the run's rows, the op loop's out-of-band
/// rows and a checkpoint's own rows all draw from one ordinal
/// sequence and every row is stamped with the ordinal it will be
/// read back under. The append methods only enqueue: the flush
/// cadence is the caller's — the harness flushes at the step
/// boundary before each provider call and once more after a tool
/// result's row (before that result's event goes out), and the run
/// task flushes before it resolves the checkpoint and again before
/// it ends.
pub(crate) struct RunLog {
    writer: Arc<LogWriter>,
}

impl RunLog {
    /// Wrap the session's writer.
    pub(crate) fn new(writer: Arc<LogWriter>) -> Self {
        Self { writer }
    }

    /// Reserve an ordinal for one row, stamp it, and enqueue it.
    ///
    /// The row is on disk only after a [`Self::flush`]; the
    /// ordinal it was given is the one the reader will reproduce
    /// from its `seq` field, and the ledger the writer feeds
    /// records it under the same number — the source of truth a
    /// durable compaction checkpoint resolves its provenance
    /// against. The row is taken by value: the writer stamps it
    /// in place, so a caller that still needs it clones it
    /// itself.
    pub(crate) async fn append(
        &mut self,
        value: Value,
    ) -> Result<u64, SessionError> {
        self.writer.append(value).await
    }

    /// Reserve an ordinal for one typed row, stamp it, and
    /// enqueue it.
    ///
    /// The writer replaces the row's `seq` and `ts`: the ordinal
    /// is the writer's to hand out, and a typed event's builder
    /// leaves `seq: 0` behind. The stamp is what a typed reader
    /// reproduces — the row's own `seq` field is the ordinal the
    /// log holds it under, which is also how the fold numbers a
    /// fully stamped log.
    pub(crate) async fn append_typed(
        &mut self,
        value: Value,
    ) -> Result<(), SessionError> {
        self.writer.append(value).await.map(|_| ())
    }

    /// Serialise one typed event and append it as a row.
    ///
    /// For the variants that hold their payload in fields rather than
    /// an opaque `data` `Value` (`park_consumed` is one): they have no
    /// builder, and the controller should not hand-roll their wire
    /// shape at the call site. `seq: 0` / `ts: ""` in, the writer's
    /// stamp out — same as [`Self::append_typed`].
    pub(crate) async fn append_event(
        &mut self,
        event: &uva::session::SessionEvent,
    ) -> Result<(), SessionError> {
        let row = serde_json::to_value(event).map_err(|e| {
            SessionError::AppendFailed(format!("serialise the row: {e}"))
        })?;
        self.append_typed(row).await
    }
}

/// Concatenate the textual component of a multimodal prompt.
///
/// `PromptMulti` carries the user's typed text as a `Text` part
/// alongside any image/audio parts, so the text the durability
/// layer records is not on the run's `prompt` variable. Only text
/// parts contribute — the binary bytes are deliberately not
/// persisted (see the persistence block in the run task).
pub(super) fn prompt_text_of_parts(parts: &[ContentPart]) -> String {
    let mut out = String::new();
    for part in parts {
        if let ContentPart::Text(t) = part {
            out.push_str(&t.text);
        }
    }
    out
}

/// Store a multimodal prompt's payloads and describe them as
/// re-readable references.
///
/// The bytes themselves are deliberately never written into the log:
/// base64 images would bloat the per-session file and nothing reads
/// them back as text. They go into the deployment's
/// [`AttachmentStore`] instead — content-addressed by their SHA-256,
/// deduped, and readable by hash — and the returned rows carry that
/// hash. That hash is what makes the row a **reference**: the metadata
/// this function used to emit (kind / name / mime / byte length) let a
/// reloaded transcript show that a turn carried something but gave it
/// no way to fetch it, so a parked prompt, or a regenerated turn, could
/// not put the attachment back.
///
/// A part whose payload the store will not take (an unsupported MIME
/// the image path enforces, a payload over the size cap, a remote URL
/// with no local bytes) degrades to a metadata-only row rather than
/// failing: losing a reference is bad, losing the prompt is worse.
///
/// `kind` mirrors the composer's own vocabulary (`"image"` / `"audio"`
/// / `"file"`), so a client renders the same chip either way. `name` is
/// omitted when the part carries none rather than being invented here —
/// the renderer owns the fallback label, because only it knows what
/// reads well.
pub(super) fn attachment_refs_of_parts(
    parts: &[ContentPart],
    store: &AttachmentStore,
) -> Vec<AttachmentRefRow> {
    let mut refs = Vec::new();
    for part in parts {
        let (kind, name, mime_type, raw) = match part {
            // `data` holds either inline base64 or a remote URL, so a
            // URL part has no local bytes to store — it is a reference
            // to something this deployment does not own.
            ContentPart::Image(image) => {
                ("image", None, Some(image.mime_type.clone()), &image.data)
            }
            ContentPart::Audio(audio) => {
                ("audio", None, Some(audio.mime_type.clone()), &audio.data)
            }
            // A resource's payload is a `data:` URI (inline bytes) or a
            // URL; only the former is storable.
            ContentPart::Resource(link) => (
                "file",
                Some(link.name.clone()),
                link.mime_type.clone(),
                &link.uri,
            ),
            // A text part is the prompt itself, already in `text`.
            _ => continue,
        };
        let (saved, unstored) = match inline_bytes(raw) {
            Ok(bytes) => {
                let mime = mime_type.as_deref().unwrap_or_default();
                // An image keeps the store's image allow-list: it is the
                // gate an uploaded image goes through, and a doctored
                // MIME must not turn it into a blob. Everything else a
                // multimodal prompt carries (audio, a binary file) is
                // content-addressed without a MIME gate.
                let result = if kind == "image" {
                    store.save_image(&bytes, mime)
                } else {
                    store.save_blob(&bytes, mime)
                };
                match result {
                    Ok(saved) => (Some(saved), None),
                    Err(error) => (None, Some(error.to_string())),
                }
            }
            Err(reason) => (None, Some(reason.to_string())),
        };
        refs.push(AttachmentRefRow {
            kind: kind.to_string(),
            hash: saved.as_ref().map(|saved| saved.hash.clone()),
            name: name.filter(|name| !name.is_empty()),
            mime_type: mime_type
                .or_else(|| saved.as_ref().map(|s| s.mime_type.clone()))
                .filter(|mime| !mime.is_empty()),
            byte_len: saved.as_ref().map(|saved| saved.byte_len as u64),
            unstored,
        });
    }
    refs
}

/// The decoded bytes of a media part's payload, or the reason it has no
/// local bytes.
///
/// Both media arms accept either an inline payload (bare base64 or a
/// `data:` URI) or a remote URL, so the decision lives here once: a URL
/// has no local bytes to store, and pretending its text were the
/// payload would put a URL where the model expects an image. The reason
/// is what the ref row records, so a reader can tell "never local" from
/// "the store refused it".
fn inline_bytes(data: &str) -> Result<Vec<u8>, &'static str> {
    if data.starts_with("http://") || data.starts_with("https://") {
        return Err("the payload is a remote URL");
    }
    let payload = data
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(',').map(|(_, payload)| payload))
        .unwrap_or(data);
    BASE64
        .decode(payload)
        .map_err(|_| "the payload is not inline base64")
}

/// The `SurfaceOp::Replace` a rerun's prompt row carries.
///
/// `regenerate` re-queues the last user turn, so a plain append
/// would leave the durable log holding the turn twice
/// (`prompt, answer1, prompt, answer2`) and every log-verbatim
/// projection — the session-detail page, "Continue chat" — would
/// show the prompt twice while the chat page collapses it. The
/// rerun's prompt row instead shadows the turn it replaces: the
/// span from the last user-role message to the tail of the
/// current fold, cited by the shadowed rows' seqs. Every
/// `fold_log_surface` consumer (the next run's history via
/// `derive_history`, session search, the regenerate route's own
/// prompt recovery) then folds the log to the replacement story,
/// and the `SurfaceLedger` splices the same span so a later
/// compaction still resolves.
///
/// `None` (plain append) when the fold holds no user message —
/// the regenerate route refuses that case up front, so this is a
/// defensive fallback, not a second contract.
pub(super) fn rerun_replace_op(
    events: &[serde_json::Value],
) -> Option<uva::session::SurfaceOp> {
    let folded = uva::session::fold_log_surface(events);
    let start = folded.messages.iter().rposition(|row| {
        serde_json::from_value::<uva::provider::Message>(row.clone())
            .is_ok_and(|message| message.role == Role::User)
    })?;
    Some(uva::session::SurfaceOp::Replace {
        start,
        end: folded.messages.len(),
        source_event_seqs: folded.surface_seqs[start..].to_vec(),
    })
}

/// Truncate a string to at most `max_chars` Unicode scalar values,
/// appending an ellipsis marker when truncation actually happened.
/// Used only for log previews so a 100kB user prompt does not
/// produce a 100kB log line.
///
/// The truncation is a thin wrapper over
/// [`uva::core::text::truncate_chars`] so the workspace has a
/// single canonical char-based truncator. The presentation
/// concern (the `…` marker) stays here because the controller's
/// log previews are the only call site that uses this exact
/// shape; other sites that need a different marker (or no
/// marker) call the core helper directly.
pub(super) fn truncate(s: &str, max_chars: usize) -> String {
    let (kept, was_truncated) = uva::core::text::truncate_chars(s, max_chars);
    if was_truncated {
        format!("{kept}…")
    } else {
        kept
    }
}

#[cfg(test)]
mod tests {
    use uva::provider::{
        ImageContent,
        ResourceLink,
        types::{AudioContent, AudioFormat, TextContent},
    };

    use super::*;

    fn text(body: &str) -> ContentPart {
        ContentPart::Text(TextContent {
            text: body.to_string(),
            cache_control: None,
        })
    }

    /// A text part is the prompt itself, so it produces no chip; every
    /// media part — image, audio, file — produces a re-readable ref.
    #[test]
    fn text_is_not_an_attachment_and_every_media_part_is() {
        let store = AttachmentStore::default();
        let parts = vec![
            text("look at this"),
            ContentPart::Audio(AudioContent {
                data: "QUJD".to_string(),
                mime_type: "audio/wav".to_string(),
                format: Some(AudioFormat::Wav),
            }),
        ];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs.len(), 1, "the text part is not an attachment");
        assert_eq!(refs[0].kind, "audio");
        assert!(
            refs[0].hash.is_some(),
            "an audio payload is stored, so the ref resolves"
        );
        assert_eq!(
            store
                .read_base64_raw(refs[0].hash.as_deref().expect("hash"))
                .expect("readable"),
            b"ABC",
            "the stored bytes are the decoded payload"
        );
    }

    /// An image reference carries the composer's `image` kind, the mime
    /// type, the DECODED size — and the hash that makes it re-readable.
    #[test]
    fn image_ref_reports_the_decoded_byte_length_and_its_hash() {
        let store = AttachmentStore::default();
        // "AAAAAA==" decodes to 4 bytes.
        let parts = vec![ContentPart::Image(ImageContent {
            data: "AAAAAA==".to_string(),
            mime_type: "image/png".to_string(),
            detail: None,
        })];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "image");
        assert_eq!(refs[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(refs[0].byte_len, Some(4), "6 chars = 4 bytes");
        assert!(
            refs[0].name.is_none(),
            "an image part carries no filename; the renderer owns the label"
        );
        let hash = refs[0].hash.as_deref().expect("the ref carries a hash");
        assert_eq!(
            store.read_base64_raw(hash).expect("readable").len(),
            4,
            "the bytes are in the store, not in the row"
        );
    }

    /// A resource reference keeps the filename the wire gave it, decodes
    /// a `data:` URI, and stores those bytes.
    #[test]
    fn resource_ref_keeps_the_name_and_decodes_a_data_uri() {
        let store = AttachmentStore::default();
        let parts = vec![ContentPart::Resource(ResourceLink {
            uri: "data:application/pdf;base64,QUJD".to_string(),
            name: "probe.pdf".to_string(),
            title: None,
            description: None,
            mime_type: Some("application/pdf".to_string()),
        })];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "file");
        assert_eq!(refs[0].name.as_deref(), Some("probe.pdf"));
        assert_eq!(refs[0].mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(refs[0].byte_len, Some(3));
        assert!(
            refs[0].hash.is_some(),
            "an inline payload is storable, so it is stored"
        );
    }

    /// A remote URL has no local bytes: no size to report, no hash to
    /// resolve, and an empty name is dropped rather than persisted as
    /// `""` for every renderer to special-case.
    #[test]
    fn a_url_resource_omits_the_size_the_hash_and_an_empty_name() {
        let store = AttachmentStore::default();
        let parts = vec![ContentPart::Resource(ResourceLink {
            uri: "https://example.com/a.pdf".to_string(),
            name: String::new(),
            title: None,
            description: None,
            mime_type: None,
        })];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "file");
        assert!(refs[0].byte_len.is_none());
        assert!(
            refs[0].hash.is_none(),
            "a URL is not bytes this deployment stores"
        );
        assert!(refs[0].name.is_none());
        assert!(refs[0].mime_type.is_none());
    }

    /// An image part's `data` is inline base64 OR a remote URL — the same
    /// dual shape the resource arm has. A URL has no decodable size and
    /// nothing to store, so the ref reports neither.
    #[test]
    fn a_remote_image_url_reports_no_byte_length_and_no_hash() {
        let store = AttachmentStore::default();
        let parts = vec![ContentPart::Image(ImageContent {
            data: "https://example.com/a.png".to_string(),
            mime_type: "image/png".to_string(),
            detail: None,
        })];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "image");
        assert_eq!(refs[0].mime_type.as_deref(), Some("image/png"));
        assert!(
            refs[0].byte_len.is_none(),
            "a URL is not base64 and has no decoded size"
        );
        assert!(refs[0].hash.is_none());
    }

    /// A `data:`-wrapped payload still reports its decoded size, so the
    /// URL guard above does not swallow the inline case.
    #[test]
    fn a_data_uri_image_still_reports_its_size() {
        let store = AttachmentStore::default();
        let parts = vec![ContentPart::Image(ImageContent {
            data: "data:image/png;base64,QUJD".to_string(),
            mime_type: "image/png".to_string(),
            detail: None,
        })];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs[0].byte_len, Some(3));
        assert!(refs[0].hash.is_some());
    }

    /// A payload the store refuses (over the size cap) degrades to a
    /// metadata-only ref: losing the reference is bad, losing the prompt
    /// is worse.
    #[test]
    fn an_unstorable_payload_keeps_a_metadata_only_ref() {
        let store = AttachmentStore::default();
        let oversize =
            BASE64.encode(vec![0u8; uva::attachment::DEFAULT_MAX_BYTES + 1]);
        let parts = vec![ContentPart::Image(ImageContent {
            data: oversize,
            mime_type: "image/png".to_string(),
            detail: None,
        })];
        let refs = attachment_refs_of_parts(&parts, &store);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "image");
        assert!(
            refs[0].hash.is_none(),
            "nothing was stored, so there is no hash to claim"
        );
        assert!(refs[0].byte_len.is_none());
    }
}
