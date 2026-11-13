//! The server's runtime seams: the tokio adapters from
//! `uva-core-tokio`.
//!
//! The harness default is `uva_core::spawn::InlineSpawner` (std
//! thread + `block_on`), which is what keeps the library
//! runtime-neutral — but a loop detached that way has **no tokio
//! reactor**, so any in-tree tool that awaits `tokio::fs` (the `read`
//! and `write` bricks) panics. The server is a tokio application, so
//! every assembly site here installs [`TokioSpawner`], the same
//! adapter an external consumer installs.
//!
//! The same reasoning covers the two seams `uva-session` takes as
//! `Arc<dyn …>`: [`TokioBlocking`] (fs work off the worker thread)
//! and [`TokioSleeper`] (the summary deadline). Their defaults are
//! runtime-free and correct but pay one OS thread per call, so every
//! assembly site in this application installs the tokio-backed
//! adapter instead.

use std::sync::Arc;

use uva::{
    core::{blocking::Blocking, sleeper::Sleeper, spawn::Spawner},
    harness_tokio::{TokioBlocking, TokioSleeper, TokioSpawner},
};

/// The shared spawner every assembly site installs.
pub fn server_spawner() -> Arc<dyn Spawner> {
    Arc::new(TokioSpawner)
}

/// The shared blocking seam for every [`uva::session`] sink and
/// index this application builds.
pub fn server_blocking() -> Arc<dyn Blocking> {
    Arc::new(TokioBlocking)
}

/// The shared sleeper for every deadline this application arms
/// through a seam.
pub fn server_sleeper() -> Arc<dyn Sleeper> {
    Arc::new(TokioSleeper)
}
