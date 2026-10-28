//! The repair entry point the harness and the server call.
//!
//! [`SessionLog::repair`] is the operation; this module is the free
//! function a caller holding `&dyn SessionLog` can name without
//! importing the trait, and it is where the crate documents that
//! repair runs **before** a resume fold: the fold must read a
//! balanced tail.

use crate::{
    SessionError,
    log::{RepairReport, SessionLog},
};

/// Close an open tail turn on `writer`'s log, then flush.
///
/// See [`SessionLog::repair`] for the contract.
///
/// # Errors
///
/// Returns the log's error when the rows cannot be read or the
/// closers cannot be written.
pub async fn repair_log(
    writer: &dyn SessionLog,
) -> Result<RepairReport, SessionError> {
    writer.repair().await
}
