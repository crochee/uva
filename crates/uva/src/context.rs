//! [`uva_context`] — what fits in the next
//! prompt ([`ContextManager`]) and what is remembered between
//! prompts ([`Memory`]).
//!
//! Two context-manager strategies ship, both swappable at agent
//! construction time: [`NoopContextManager`] and
//! [`TruncatingContextManager`] (the agent default). A manager only
//! projects or trims the request it is handed: compression is
//! `uva::session`'s `Compactor`. The memory tier is
//! [`InMemoryMemory`], [`FileMemory`], and (behind the facade's
//! `sqlite` feature) `SqliteMemory`.

pub use uva_context::*;
