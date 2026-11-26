//! The outbound spool: disk-backed backpressure for a slow reader.
//!
//! omp parity (`packages/coding-agent/src/modes/rpc/rpc-frame.ts`):
//! when the peer reads slower than the process produces, pending
//! output spills to a **private temporary file** and drains in
//! 64 KiB blocks, so queued output memory stays bounded at the cost
//! of disk I/O and temporary disk usage. The file is removed when
//! the backlog drains or the writer goes away (`rpc.md`: "Under
//! backpressure, the server spills pending bytes to a private
//! temporary file and drains it in 64 KiB blocks").
//!
//! The block size is the drain granularity: one `read`+`write` pair
//! per block, never the whole backlog in memory. The file lives in
//! the process temp directory under a unique name — two writers on
//! one machine never share a spool.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

/// A private temporary file holding overflowed outbound frames.
pub(crate) struct Spool {
    file: File,
    path: PathBuf,
    /// Bytes appended but not yet drained.
    pending: u64,
    /// The read cursor, from the file's start.
    cursor: u64,
}

impl Spool {
    /// The drain block size: the most a drain holds in memory.
    pub(crate) const BLOCK: usize = 64 * 1024;

    /// Create the spool's file.
    ///
    /// The name carries the pid and a counter-derived nonce — a
    /// second writer on one machine gets its own file, and a stale
    /// file from a crashed process is never appended to.
    ///
    /// # Errors
    ///
    /// The temp directory could not provide a file.
    pub(crate) fn create(tag: &str) -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("uva-{tag}-{}-{nonce}.spool", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            file,
            path,
            pending: 0,
            cursor: 0,
        })
    }

    /// Append one encoded frame.
    ///
    /// # Errors
    ///
    /// The spool's disk write failed — fatal for the writer that
    /// owns it.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        // The cursor trails the writes; append means the end.
        let end = self.cursor + self.pending;
        self.file.seek(SeekFrom::Start(end))?;
        self.file.write_all(bytes)?;
        self.pending += bytes.len() as u64;
        Ok(())
    }

    /// The spool's file path (its tests assert the file's fate).
    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Bytes appended but not yet drained.
    #[cfg(test)]
    pub(crate) fn pending_bytes(&self) -> u64 {
        self.pending
    }

    /// Whether every appended byte has been drained.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.pending == 0
    }

    /// Take the next block, advancing the read cursor.
    ///
    /// The synchronous half of a drain: the file read happens under
    /// the queue's lock, the caller writes the returned block to the
    /// peer without holding it. The block is the only memory a drain
    /// holds.
    ///
    /// # Errors
    ///
    /// The spool's disk read failed.
    pub(crate) fn take_block(&mut self) -> std::io::Result<Vec<u8>> {
        let take = self.pending.min(Self::BLOCK as u64) as usize;
        let mut block = vec![0u8; take];
        self.file.seek(SeekFrom::Start(self.cursor))?;
        self.file.read_exact(&mut block)?;
        self.cursor += take as u64;
        self.pending -= take as u64;
        Ok(block)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        // Best effort: a leftover file costs disk, not correctness —
        // the frames it held were already drained or the writer is
        // going away with them.
        let _ = std::fs::remove_file(&self.path);
    }
}
