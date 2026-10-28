//! Session search — find an earlier conversation by its text.
//!
//! pi ships this as `SessionSearchService`
//! (`packages/agent/src/search/index.ts`): search the agent's *own*
//! history, not the model's knowledge. That is a product capability a
//! durable-transcript store earns the right to provide, and it was the one
//! capability the R62 reference-parity audit found missing here.
//!
//! Two pieces, in the order a consumer meets them:
//!
//! - [`SessionSearch`] — the seam. Four methods, runtime-neutral: a
//!   consumer can index in `SQLite`, in memory, or in a service.
//! - [`JsonlSessionSearch`] — the shipped implementation over the JSONL
//!   layout this crate already writes
//!   (`<root>/<user>/<session>/events.jsonl`). It folds each log through
//!   [`fold_log_surface`](crate::fold_log_surface) — the same function
//!   that reconstructs the model-visible surface — so it searches *what
//!   the model saw*, including compaction replacements, rather than raw
//!   log rows.
//!
//! ```text
//! <root>/alice/s_01/events.jsonl ─┐
//! <root>/alice/s_02/events.jsonl ─┼─ sync() ──▶ index ──▶ search_sessions()
//! <root>/bob/s_03/events.jsonl   ─┘                       search_entries()
//! ```
//!
//! The index is in memory and rebuilt on demand: a session log is
//! append-only, so `sync` only re-reads a file whose size changed, and a
//! deployment that never calls [`SessionSearch::sync`] pays nothing.
//!
//! # Tenancy
//!
//! The store holds every user's sessions side by side, and two users may
//! pick the same session id, so a session is addressed by the pair
//! `(user_id, session_id)` — the index key [`JsonlSessionSearch`] uses.
//! [`SearchQuery::user_id`] scopes a query: a scoped query reads only that
//! user's directory and answers only with that user's hits. Leaving it
//! `None` searches the whole store, which is deliberate for a caller that
//! owns all of it (an operator's admin index) and wrong for a request
//! handler — those resolve the caller and set the scope.
//!
//! # Scoring
//!
//! Deliberately simple and documented rather than tunable: an entry
//! scores `matched_terms / terms × 1 / (1 + ln(1 + terms_in_entry))` — term
//! coverage with a length penalty — and a session scores the sum of its
//! entry scores. That ranks "the conversation that talks about this a lot,
//! briefly" above a long transcript that mentions the word once, which is
//! the behaviour a user expects from a history search. A
//! deployment that wants BM25-style retrieval over session history
//! can implement [`SessionSearch`] and keep its own index.
//!
//! ## Module layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `query` | [`SearchQuery`] / [`EntryHit`] / [`SessionHit`] / [`SearchError`] + the [`SessionSearch`] seam. |
//! | `backend` | [`JsonlSessionSearch`] — the in-memory index over the JSONL store (`scan` / `index` / `sync` / `remove`). |
//! | `text` | The searchable text of a folded surface message. |
//! | `score` | Term coverage scoring, whole-word matching, and snippets. |

use std::{path::Path, time::SystemTime};

mod backend;
mod query;
mod score;
mod text;

pub use backend::{JsonlSessionSearch, jsonl_session_search};
pub use query::{
    DEFAULT_LIMIT,
    EntryHit,
    SearchError,
    SearchQuery,
    SessionHit,
    SessionSearch,
    SharedSessionSearch,
};

/// Modification time of a path, for callers that want to show "last
/// activity" next to a hit. `None` when the metadata is unavailable.
#[must_use]
pub fn modified_at(path: &Path) -> Option<SystemTime> {
    path.metadata().ok().and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests;
