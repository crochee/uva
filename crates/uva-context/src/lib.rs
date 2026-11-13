//! # uva-context
//!
//! Context-window projection **and** the memory tier for the agent
//! runtime.
//!
//! Two seams: the [`ContextManager`] trait (what fits in the next
//! prompt) and the [`Memory`] trait (what happened / what we know).
//!
//! | Module | Contents |
//! |---|---|
//! | [`context_manager`] | `NoopContextManager` (estimate-only) + `TruncatingContextManager` (pairwise drop) |
//! | [`memory`] | the `Memory` trait, `InMemoryMemory`, `FileMemory`, `SqliteMemory` (feature `sqlite`) |
//!
//! Compaction is **not** here: the one compression engine is
//! `uva-session`'s `Compactor`, and a `ContextManager` only
//! projects or trims the request it is handed.
//!
//! ## Dependency direction
//!
//! ```text
//! uva-harness ──▶ uva-context ──▶ uva-provider
//! ```
//!
//! `uva-context` consumes the provider wire types
//! (`Message`, `ContentPart`, `ModelConfig`) and owns no runtime of
//! its own: every strategy is driven by the agent loop calling
//! [`ContextManager::prepare`] before each LLM sampling pass.
//!
//! Strategies are plain `Arc<dyn ContextManager>` values; the agent
//! layer picks one at construction time
//! (`ReActAgent::with_context_manager`). The default agent ships
//! `TruncatingContextManager`.

pub mod context_manager;
pub mod memory;

pub use context_manager::{
    AgentState,
    ContextManager,
    NoopContextManager,
    SharedContextManager,
    TruncatingContextManager,
    UsageMeter,
};
/// The `memory` kernel plugin: publishes [`KEY_MEMORY`].
pub mod plugin;
pub use memory::{
    FileMemory,
    FileMemoryConfig,
    FileMemoryEntry,
    FileMemoryError,
    InMemoryMemory,
    MAX_MEMORY_INDEX_LINES,
    Memory,
    MemoryEntry,
    MemoryError,
    MemoryKind,
    MemoryScope,
};
pub use plugin::{KEY_MEMORY, MemoryPlugin};
