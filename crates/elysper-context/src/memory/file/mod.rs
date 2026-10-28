//! File-backed scoped long-term memory (pi-subagents parity).
//!
//! [`FileMemory`] is the persistent long-term tier the [`Memory`]
//! trait was designed for: instead of an in-process vec that dies
//! with the run, entries live in per-scope directories that
//! survive restarts and are shared across sessions.
//!
//! # Scopes
//!
//! | Scope | Directory | Lifetime intent |
//! |---|---|---|
//! | [`MemoryScope::Local`] | `<root>/.elysper/memory-local/<agent>` | checkout-private, never shared |
//! | [`MemoryScope::Project`] | `<root>/.elysper/memory/<agent>` | checked in with the repository |
//! | [`MemoryScope::User`] | `<home>/.elysper/memory/<agent>` | shared across every project on the machine |
//!
//! [`FileMemoryConfig::scopes`] lists the scopes to search in
//! precedence order; [`MemoryScope::ALL`] is nearest-first
//! (local → project → user). Earlier scopes shadow later ones when
//! two files carry the same ULID, and new entries are written to
//! the **first** configured scope.
//!
//! # Layout
//!
//! ```text
//! <scope dir>/
//! ├── MEMORY.md            bounded index, regenerated on write
//! └── entries/
//!     └── <ulid>.md       frontmatter (name, description, type) + body
//! ```
//!
//! `MEMORY.md` is a projection of `entries/`, not a source of
//! truth: every [`FileMemory::store_entry`] writes the entry file
//! first and then re-renders the index from disk, so a crash
//! between the two leaves a recallable entry rather than a
//! dangling link. The index is capped at
//! [`MAX_MEMORY_INDEX_LINES`] lines (the reference implementation
//! truncates `MEMORY.md` at 200); entries beyond the cap are
//! summarised by a trailing omission note and remain recallable
//! from disk.
//!
//! # Recall
//!
//! [`FileMemory::search`] scores every entry against the query's
//! keywords — 3× for a hit in the name, 2× in the description, 1×
//! in the body — and returns hits by descending score, breaking
//! ties on descending id: ULIDs are time-sortable, so the newest
//! entry wins. The name and description are exactly the text the
//! index carries, so this is the deterministic, dependency-free
//! equivalent of searching the index plus the detail bodies. An
//! empty query matches every entry.
//!
//! # Defenses
//!
//! The tier mirrors the reference implementation's filesystem
//! posture. Agent names are whitelisted (`[A-Za-z0-9_-]`, at most
//! 128 characters — no separators, no dots, no `..`), every path
//! below the caller-supplied root is checked for symlinks before
//! it is read or created, and reads never create directories. A
//! symlinked scope directory, index, or entry file is refused with
//! [`FileMemoryError::Symlink`] rather than followed.
//!
//! # Tiers
//!
//! Only the long-term tier is file-backed. Conversation and working
//! memory delegate to an inner [`Memory`] ([`InMemoryMemory`] by
//! default, or any sink-backed impl via
//! [`FileMemory::with_inner`]), so a [`FileMemory`] is a complete
//! [`Memory`] implementation rather than a long-term-only shim.
//!
//! ## Module layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `scope` | [`MemoryScope`] — the three scopes and the directories they resolve to. |
//! | `entry` | [`MemoryKind`] + [`FileMemoryEntry`] — the frontmatter shape and its rendering. |
//! | `config` | [`FileMemoryConfig`] + the resolved per-scope directories. |
//! | `error` | [`FileMemoryError`]. |
//! | `guards` | The traversal defenses: agent-name whitelist, symlink refusal, contained walks. |
//! | `text` | Frontmatter / one-line text helpers shared by writer and reader. |
//! | `index` | Disk scan, keyword scoring, and the bounded `MEMORY.md` render. |
//! | `store` | [`FileMemory`] — the tier itself. |

/// Entries subdirectory name (one file per entry).
const ENTRIES_DIR: &str = "entries";
/// Index file name inside a scope directory.
const INDEX_FILE: &str = "MEMORY.md";
/// Hard cap on the rendered `MEMORY.md` index length; mirrors the
/// reference implementation's 200-line `MAX_MEMORY_LINES`.
pub const MAX_MEMORY_INDEX_LINES: usize = 200;
/// Longest accepted agent name.
const MAX_AGENT_NAME_LEN: usize = 128;

mod config;
mod entry;
mod error;
mod guards;
mod index;
mod scope;
mod store;
mod text;

pub use config::FileMemoryConfig;
pub use entry::{FileMemoryEntry, MemoryKind};
pub use error::FileMemoryError;
pub use scope::MemoryScope;
pub use store::FileMemory;

#[cfg(test)]
mod tests;
