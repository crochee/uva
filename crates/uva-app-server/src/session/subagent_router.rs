//! Sub-agent session opener: every delegated child runs in its own
//! first-class session instead of inlining its trace into the parent.
//!
//! The `task` tool drives the child run; the child's own harness
//! writes its model facts into the child session's log, and this
//! module opens that session: the `subagent_enter` header row, the
//! dispatch prompt, and the writer the child's run writes through.
//!
//! The opener is the **writer half** of the old sub-agent router. The
//! routing half — the one that intercepted a wrapped child event and
//! copied it into a session — is gone with the delegation channel: the
//! child session is opened up front and the child's harness writes to
//! it directly, so there is no event to intercept and nothing to route.
//! The parent's log keeps only the ordinary `tool_call` / `tool_result`
//! rows the tool dispatch writes; the child session is the one
//! full-fidelity record.
//!
//! Per child (keyed by the child session id the `task` tool mints):
//!
//! 1. a `subagent_enter` header row + the dispatch prompt open the
//!    child's own session log — created through the same
//!    [`SessionRegistry`] every top-level session uses, so the child
//!    shows up in `GET /api/v1/sessions` and replays through
//!    `GET /api/v1/sessions/{id}` exactly like a user conversation;
//! 2. the child's model facts — one `assistant_message` row per
//!    turn, one `tool_result` row per result — are written by the
//!    child's **own harness**, through the child session's writer
//!    this module resolves for it
//!    ([`SubagentWriters`](uva::harness::SubagentWriters));
//! 3. the `task` tool appends the matching `subagent_exit` footer
//!    when the child run ends.
//!
//! Every row this module writes goes through that session's one
//! writer, so each log keeps one ordinal sequence and every row is
//! stamped with its ordinal.

use std::sync::Arc;

use dashmap::DashSet;
use uva::{
    provider::Message,
    session::{manager::SessionRegistry, subagent_enter, user_message},
};

/// Opens the session a delegated child runs in.
pub struct SubagentRouter {
    registry: Arc<SessionRegistry>,
    /// Child session ids whose own header + prompt rows are written.
    /// Ids are ULIDs minted per delegation, so an id is never
    /// legitimately reused.
    opened: DashSet<String>,
}

impl SubagentRouter {
    /// Build an opener over the process-wide session registry.
    #[must_use]
    pub fn new(registry: Arc<SessionRegistry>) -> Self {
        Self {
            registry,
            opened: DashSet::new(),
        }
    }

    /// Open the child session and return its writer.
    ///
    /// Called through [`SessionSubagentWriters`] before the child run
    /// starts: the enter header and dispatch prompt are stamped now, so
    /// the child's own first row — its `request_header`, its first
    /// assistant turn — lands after them. The rows take the
    /// writer's synchronous path; the child's first flush lands
    /// them.
    pub(crate) fn open_child_session(
        &self,
        user_id: &str,
        parent_session_id: &str,
        child_session_id: &str,
        depth: usize,
        agent_name: &str,
        prompt: Option<&str>,
    ) -> Arc<uva::session::LogWriter> {
        let child_id = child_session_id.trim().to_string();
        let writer = self.registry.writer(user_id, &child_id);
        if !is_safe_session_id(&child_id) {
            return writer;
        }
        if self.opened.insert(child_id.clone()) {
            let depth: u32 = depth.try_into().unwrap_or(u32::MAX);
            writer.append_event_nowait(&subagent_enter(
                &child_id,
                parent_session_id,
                depth,
                agent_name,
            ));
            if let Some(prompt) = prompt.filter(|p| !p.is_empty()) {
                match serde_json::to_value(Message::user(prompt)) {
                    Ok(data) => {
                        writer.append_event_nowait(&user_message(data, None));
                    }
                    // A log write must never stop the run: the child's
                    // header landed, and a missing prompt degrades the
                    // record rather than aborting the delegation.
                    Err(error) => tracing::error!(
                        target: "uva.session",
                        child_session_id = %child_id,
                        %error,
                        "the dispatch prompt could not be encoded; \
                         the child log keeps its header only"
                    ),
                }
            }
        }
        writer
    }

    /// Session registry the router creates child sinks through.
    #[must_use]
    pub fn registry(&self) -> &Arc<SessionRegistry> {
        &self.registry
    }
}

/// [`SubagentWriters`](uva::harness::SubagentWriters) that resolves the
/// owning user **from the parent session id** the caller passes with
/// the child.
///
/// The `task` tool is a process-wide registry tool, so it cannot hold a
/// writer source bound to one run's `(user, parent)` pair. This source
/// is bound to the process instead:
/// [`open_child`](uva::harness::SubagentWriters::open_child) reads
/// `parent_session_id`, looks the owner up in the session registry, and
/// opens the child through the router with that pair — the same rows a
/// per-run source would write, resolved from the tool's own context.
pub struct SessionSubagentWriters {
    router: Arc<SubagentRouter>,
    registry: Arc<SessionRegistry>,
}

impl SessionSubagentWriters {
    /// Open children of any session `registry` knows, through `router`.
    pub fn new(
        router: Arc<SubagentRouter>,
        registry: Arc<SessionRegistry>,
    ) -> Self {
        Self { router, registry }
    }
}

impl uva::harness::SubagentWriters for SessionSubagentWriters {
    fn open_child(
        &self,
        child_session_id: &str,
        parent_session_id: &str,
        depth: usize,
        agent_name: &str,
        prompt: Option<&str>,
    ) -> Arc<dyn uva::session::SessionLog> {
        let parent = parent_session_id.trim();
        // A tool context without a session (an embedder driving a bare
        // run) has no user to resolve; the child still gets a log under
        // the registry root rather than no log at all.
        let user_id = self.registry.user_of(parent).unwrap_or_default();
        let writer = self.router.open_child_session(
            &user_id,
            parent,
            child_session_id,
            depth,
            agent_name,
            prompt,
        );
        writer as Arc<dyn uva::session::SessionLog>
    }
}

/// A writer source that is filled **after** the kernel mount.
///
/// The `tool-task` row builds its tool during the mount, while the
/// router needs the `sessions` service the same mount publishes. The
/// boot therefore hands the row this slot and installs the real source
/// immediately after the mount; a child opened before that (nothing can
/// be, in boot order) falls back to a log that discards its rows rather
/// than panicking inside a tool call.
#[derive(Clone, Default)]
pub struct SubagentWriterSlot {
    inner: Arc<
        parking_lot::RwLock<Option<Arc<dyn uva::harness::SubagentWriters>>>,
    >,
}

impl SubagentWriterSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the real source.
    pub fn set(&self, writers: Arc<dyn uva::harness::SubagentWriters>) {
        *self.inner.write() = Some(writers);
    }
}

impl std::fmt::Debug for SubagentWriterSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentWriterSlot")
            .field("installed", &self.inner.read().is_some())
            .finish()
    }
}

impl uva::harness::SubagentWriters for SubagentWriterSlot {
    fn open_child(
        &self,
        child_session_id: &str,
        parent_session_id: &str,
        depth: usize,
        agent_name: &str,
        prompt: Option<&str>,
    ) -> Arc<dyn uva::session::SessionLog> {
        match self.inner.read().as_ref() {
            Some(writers) => writers.open_child(
                child_session_id,
                parent_session_id,
                depth,
                agent_name,
                prompt,
            ),
            None => Arc::new(DiscardingLog::new(child_session_id.to_string())),
        }
    }
}

/// The fallback log of a child opened before the boot installed its
/// writer source: every row is dropped, which is the documented
/// "children stream live but persist nothing" shape.
struct DiscardingLog {
    id: String,
}

impl DiscardingLog {
    fn new(id: String) -> Self {
        Self { id }
    }
}

#[async_trait::async_trait]
impl uva::session::SessionLog for DiscardingLog {
    fn id(&self) -> &str {
        &self.id
    }

    fn last_seq(&self) -> u64 {
        0
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, uva::session::SessionError> {
        Ok(Vec::new())
    }

    fn set_ledger(&self, _ledger: Arc<uva::session::SurfaceLedger>) {}

    fn seed_ledger(&self, _rows: &[serde_json::Value]) {}

    async fn append_event(
        &self,
        _event: &uva::session::SessionEvent,
    ) -> Result<u64, uva::session::SessionError> {
        Ok(0)
    }

    fn append_event_nowait(&self, _event: &uva::session::SessionEvent) -> u64 {
        0
    }

    async fn flush(&self) -> Result<(), uva::session::SessionError> {
        Ok(())
    }

    async fn close(
        &self,
        _reason: uva::session::SessionEndReason,
    ) -> Result<(), uva::session::SessionError> {
        Ok(())
    }

    async fn repair(
        &self,
    ) -> Result<uva::session::RepairReport, uva::session::SessionError> {
        Ok(uva::session::RepairReport {
            closers: 0,
            last_seq: 0,
        })
    }
}

/// The same shape [`SessionRegistry`] validates ids with, checked
/// here defensively: the child id reaches `sessions_root`'s path via
/// `sink()`, which does not validate on its own. ULIDs always pass;
/// this guards a future caller that mints something else.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
        && !id.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_types(events: &[serde_json::Value]) -> Vec<String> {
        events
            .iter()
            .map(|ev| {
                ev.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string()
            })
            .collect()
    }

    /// The text of one user row. The typed `user_message` carries a
    /// provider `Message`, so decode it back and ask for its text; a
    /// log written before the typed writer kept `{"text": …}` in the
    /// payload instead.
    fn uva_session_row_text(row: &serde_json::Value) -> Option<String> {
        if row.get("type").and_then(|t| t.as_str()) == Some("user_message") {
            let message = row.get("data").cloned().and_then(|data| {
                serde_json::from_value::<uva::provider::Message>(data).ok()
            })?;
            return message.content.extract_text();
        }
        row.pointer("/data/text")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }

    /// The contract the opener hangs on: a delegated child's own
    /// session opens with the enter header and the dispatch prompt,
    /// and the child's harness writes its rows through that writer —
    /// the child's trace never inlines into the parent transcript.
    #[tokio::test]
    async fn the_child_session_opens_with_its_header_and_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let router = SubagentRouter::new(Arc::clone(&registry));

        let child_writer = router.open_child_session(
            "u1",
            "parent-1",
            "child-1",
            1,
            "scout",
            Some("map the repo"),
        );
        child_writer.append_nowait(serde_json::json!({
            "type": "assistant_message",
            "data": {"role": "assistant",
                     "content": {"Multi": [
                         {"type": "text", "text": "step one"}
                     ]}},
        }));
        registry
            .writer("u1", "child-1")
            .flush()
            .await
            .expect("child flush");

        let child_events = registry
            .sink("u1", "child-1")
            .read()
            .await
            .expect("child read");
        assert_eq!(
            row_types(&child_events),
            vec!["subagent_enter", "user_message", "assistant_message"],
            "child log: header, prompt, the child's own assistant row — \
             the child's model facts come from the child's writer"
        );
        let prompt = uva_session_row_text(&child_events[1]);
        assert_eq!(prompt.as_deref(), Some("map the repo"));
        assert_eq!(
            child_events[0]
                .pointer("/data/agent_name")
                .and_then(|a| a.as_str()),
            Some("scout")
        );
        assert_eq!(
            child_events[0]
                .pointer("/data/parent_session_id")
                .and_then(|p| p.as_str()),
            Some("parent-1")
        );
        assert_eq!(
            child_events[0]
                .pointer("/data/depth")
                .and_then(|d| d.as_u64()),
            Some(1)
        );
    }

    /// Opening the same child twice must not stamp the header twice:
    /// the opener is idempotent per child id.
    #[tokio::test]
    async fn opening_a_child_twice_stamps_the_header_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let router = SubagentRouter::new(Arc::clone(&registry));

        router
            .open_child_session("u1", "parent-1", "child-1", 1, "scout", None);
        router
            .open_child_session("u1", "parent-1", "child-1", 1, "scout", None);
        registry
            .writer("u1", "child-1")
            .flush()
            .await
            .expect("child flush");

        let child_events = registry
            .sink("u1", "child-1")
            .read()
            .await
            .expect("child read");
        assert_eq!(
            row_types(&child_events),
            vec!["subagent_enter"],
            "the header lands once, whatever the number of opens"
        );
    }
}
