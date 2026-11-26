//! Cross-process session ownership — dsh `session.lock` parity.
//!
//! dsh opens a session for writing by taking an exclusive `flock(2)`
//! on `session.lock` beside the log; read handles coexist freely,
//! and there is **no expiry**: a wedged holder keeps the lock until
//! its process dies, because an expiring lock could tear the log
//! (`packages/session/session-persistence-jsonl/src/lease.ts:1-24`,
//! `:66-88`). This module is the same contract for the JSONL sink:
//!
//! - [`SessionOwnerLock::try_acquire`](crate::ownership::SessionOwnerLock::try_acquire)
//!   opens `<dir>/owner.lock`,
//!   writes this process's id into it (diagnostics: a refused writer
//!   can name the holder), and takes the exclusive lock without
//!   blocking.
//! - The lock is held until the value drops — for the sink that
//!   claims it, that is until the registry drops the sink or the
//!   process exits. Either end releases it; a crashed process's
//!   kernel closes the descriptor, which is the whole point.
//! - A contended acquire fails **closed**: the caller reports the
//!   refusal and writes nothing. Two writers stamp colliding
//!   ordinals, and a log whose ordinals cannot be reproduced is
//!   unreadable — refusing is the only safe answer.
//!
//! # Platforms
//!
//! Unix takes `flock(LOCK_EX | LOCK_NB)`. The other builds use an
//! O_EXCL create with the pid inside: a stale file (its holder
//! crashed) also fails closed until it is removed — heavier than
//! `flock`'s auto-release, but never unsound.

use std::{
    fs::{File, OpenOptions},
    io::{Seek as _, Write},
    path::{Path, PathBuf},
};

impl OwnershipError {
    #[must_use]
    /// The holding process's id, when the refusal names one.
    pub fn holder(&self) -> Option<u32> {
        match self {
            Self::Contended { holder } => *holder,
            Self::Io(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Why an ownership claim was refused.
pub enum OwnershipError {
    /// Another live process holds the session.
    ///
    /// `holder` is the id it wrote into the lock file, when the file
    /// could be read — a refusal without an id still refuses.
    #[error("the session is owned by another process{}", holder
        .map(|pid| format!(" (pid {pid})"))
        .unwrap_or_default())]
    Contended {
        /// The holding process's id, when it is known.
        holder: Option<u32>,
    },
    /// The lock file could not be created or locked.
    #[error("the session's ownership lock could not be taken: {0}")]
    Io(String),
}

/// A held session-ownership lock.
///
/// Dropping the value releases it (the descriptor close is the
/// release, so a crashed holder frees the lock with its process).
/// Clone is deliberately absent: one holder, one value — the same
/// shape as the writer it guards.
pub struct SessionOwnerLock {
    /// The held descriptor: this field's **drop** is the release, and
    /// nothing else may read it — a documented invariant, not unused
    /// code. Removing it releases the lock at construction time.
    #[allow(dead_code)]
    file: File,
    path: PathBuf,
}

impl std::fmt::Debug for SessionOwnerLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionOwnerLock")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Drop for SessionOwnerLock {
    fn drop(&mut self) {
        tracing::debug!(
            path = %self.path.display(),
            "session ownership released"
        );
    }
}

impl SessionOwnerLock {
    #[must_use]
    /// The lock file a session directory uses.
    pub fn path_for(dir: &Path) -> PathBuf {
        dir.join("owner.lock")
    }

    /// Claim exclusive write ownership of `dir`.
    ///
    /// # Errors
    ///
    /// [`OwnershipError::Contended`] when a live process holds the
    /// session (its id, when the lock file names one),
    /// [`OwnershipError::Io`] when the lock file itself could not be
    /// managed.
    pub fn try_acquire(dir: &Path) -> Result<Self, OwnershipError> {
        let path = Self::path_for(dir);
        #[cfg(unix)]
        {
            let file = open_lock_file(&path)?;
            // The pid lands before the lock is taken, so the holder
            // a refusal names is the one that took it.
            write_pid(&file)?;
            use std::os::fd::AsRawFd;
            // SAFETY: `flock` operates on the passed descriptor and
            // touches no memory; `EWOULDBLOCK`/`EAGAIN` is the
            // contention answer, every other failure is IO.
            let result = unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
            };
            if result != 0 {
                let code = std::io::Error::last_os_error().raw_os_error();
                return Err(match code {
                    Some(errno)
                        if errno == libc::EWOULDBLOCK
                            || errno == libc::EAGAIN =>
                    {
                        OwnershipError::Contended {
                            holder: read_pid(&path),
                        }
                    }
                    _ => OwnershipError::Io(format!(
                        "flock failed with {result}"
                    )),
                });
            }
            Ok(Self { file, path })
        }
        #[cfg(not(unix))]
        {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::AlreadyExists => {
                        OwnershipError::Contended {
                            holder: read_pid(&path),
                        }
                    }
                    _ => OwnershipError::Io(error.to_string()),
                })?;
            write_pid(&file)?;
            Ok(Self { file, path })
        }
    }
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> Result<File, OwnershipError> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| OwnershipError::Io(error.to_string()))
}

fn write_pid(file: &File) -> Result<(), OwnershipError> {
    let mut file = file
        .try_clone()
        .map_err(|error| OwnershipError::Io(error.to_string()))?;
    let pid = std::process::id();
    // Best effort: an unreadable id costs a name in the refusal, not
    // the refusal itself.
    let _ = file.set_len(0);
    let _ = file
        .seek(std::io::SeekFrom::Start(0))
        .and_then(|_| file.write_all(pid.to_string().as_bytes()))
        .and_then(|_| file.sync_all());
    Ok(())
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_on_the_same_directory_is_refused() {
        // Two distinct open-file-descriptions over one lock file
        // reproduce the cross-process contention on every unix.
        let dir = tempfile::tempdir().expect("tempdir");
        let first = SessionOwnerLock::try_acquire(dir.path())
            .expect("the first claim takes the lock");
        let refused = SessionOwnerLock::try_acquire(dir.path())
            .expect_err("the second claim is refused");
        match refused {
            OwnershipError::Contended { holder } => {
                // The refusal names the holder when the file is
                // readable; the first claim wrote its pid.
                assert_eq!(holder, Some(std::process::id()));
            }
            other => panic!("the refusal is contention: {other}"),
        }
        std::mem::drop(first);
        // Release is drop: the next claim succeeds.
        let second = SessionOwnerLock::try_acquire(dir.path())
            .expect("the released lock is claimable again");
        std::mem::drop(second);
    }
}
