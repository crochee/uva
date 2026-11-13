//! The fs backend's own contract: one batch in, the same rows out.

use std::sync::Arc;

use uva_core::blocking::ThreadBlocking;

use super::FsBackend;

#[test]
fn appends_then_reads_back() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("events.jsonl");
    let backend = FsBackend::new(Arc::new(ThreadBlocking));

    futures::executor::block_on(backend.append_lines(
        &path,
        Some("header"),
        &["a".to_string(), "b".to_string()],
    ))
    .unwrap();

    let bytes = futures::executor::block_on(backend.read_file(&path)).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("header\n"), "{text:?}");
    assert!(text.contains("a\n"), "{text:?}");
    assert!(text.contains("b\n"), "{text:?}");
}

#[test]
fn a_missing_file_reads_as_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("never-written.jsonl");
    let backend = FsBackend::new(Arc::new(ThreadBlocking));

    let bytes = futures::executor::block_on(backend.read_file(&path)).unwrap();
    assert!(bytes.is_empty(), "{bytes:?}");
}
