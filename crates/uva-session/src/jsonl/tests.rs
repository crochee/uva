use serde_json::json;
use tempfile::Builder;

use super::*;

fn temp_dir() -> PathBuf {
    let tmp = Builder::new().disable_cleanup(true).tempdir().unwrap();
    let p = tmp.path().to_path_buf();
    drop(tmp);
    p
}

/// Write one row the way a writer does: reserve its ordinal
/// first, then hand the row to the sink.
async fn write(sink: &JsonlSessionSink, row: Value) {
    sink.alloc_seq();
    sink.append(&row).await.expect("append");
}

// -----------------------------------------------------------------
// Lazy open
// -----------------------------------------------------------------

/// Opening a sink must stay O(1) in the log's length: the
/// listing opens one sink per durable session, so the whole-log
/// inspection the ordinal seed needs is deferred to the first
/// ordinal consumer. Observable on the counter itself: an eager
/// open would leave it seeded at the log's own numbering, a
/// deferred one leaves it untouched until an ordinal is asked
/// for — and the ask then continues that same numbering.
#[tokio::test]
async fn opening_a_sink_defers_the_whole_log_pass() {
    let dir = temp_dir().join("s_lazy_open");
    let sink = JsonlSessionSink::new("s_lazy_open", dir.clone());
    for index in 0..50 {
        write(&sink, json!({"type": "Model", "i": index})).await;
    }

    // Reopen over the 50-row log: no whole-file pass ran at
    // open, so the counter is still untouched.
    let reopened = JsonlSessionSink::new("s_lazy_open", dir);
    assert_eq!(
        reopened.next_seq.load(Ordering::SeqCst),
        0,
        "opening a sink must not read the whole log"
    );
    // The first ordinal consumer pays the deferred pass, once,
    // and the counter continues the log's own numbering.
    assert_eq!(reopened.alloc_seq(), 51);
    assert_eq!(reopened.alloc_seq(), 52);
    assert_eq!(reopened.next_seq.load(Ordering::SeqCst), 52);
}

// -----------------------------------------------------------------
// Cross-process ownership
// -----------------------------------------------------------------

/// A second sink over the same directory must not write: its
/// appends are refused with the holder's id, because two writers
/// stamp colliding ordinals and a log the reader cannot renumber is
/// unreadable (dsh `session.lock`; read handles coexist freely).
#[tokio::test]
async fn a_second_sink_over_the_same_directory_is_refused() {
    let dir = temp_dir().join("s_owner");
    let owner = JsonlSessionSink::new("s_owner", dir.clone());
    write(&owner, json!({"type": "Model", "text": "mine"})).await;

    // A second sink over the same directory: reading is fine, so
    // opening and the deferred ordinal pass must not claim anything.
    let other = JsonlSessionSink::new("s_owner", dir.clone());
    assert_eq!(other.alloc_seq(), 2, "reads coexist with the owner");

    // Writing is not: the refusal names the holder.
    let refused = other
        .append(&json!({"type": "Model", "text": "trespass"}))
        .await
        .expect_err("the second sink must not write");
    match refused {
        SessionError::OwnedByAnotherProcess { holder } => {
            assert_eq!(holder, Some(std::process::id()));
        }
        other_error => panic!("the refusal is ownership: {other_error}"),
    }

    // Release is drop: once the owner is gone, the second sink's
    // write succeeds — the crash case releases the same way, because
    // the kernel closes the descriptor.
    drop(owner);
    write(&other, json!({"type": "Model", "text": "mine now"})).await;
    let rows = other.read().await.expect("read");
    assert_eq!(rows.len(), 2, "both rows are in the log");
}

// -----------------------------------------------------------------
// Tail reads
// -----------------------------------------------------------------

#[tokio::test]
async fn read_tail_returns_exactly_the_last_n_rows() {
    let dir = temp_dir().join("s_tail_n");
    let sink = JsonlSessionSink::new("s_tail_n", dir);
    for i in 0..25 {
        write(&sink, json!({"i": i})).await;
    }
    let tail = sink.read_tail(5).await.expect("a tail");
    assert_eq!(tail.len(), 5);
    assert_eq!(
        tail.iter()
            .map(|row| row["i"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![20, 21, 22, 23, 24],
        "the window is the log's end, in order"
    );
}

#[tokio::test]
async fn read_tail_keeps_correct_rows_across_the_chunk_boundary() {
    // Rows wide enough that the window spans several read chunks,
    // so the partial-first-line handling is exercised.
    let dir = temp_dir().join("s_tail_wide");
    let sink = JsonlSessionSink::new("s_tail_wide", dir);
    let filler = "x".repeat(4000);
    for i in 0..40 {
        write(&sink, json!({"i": i, "text": filler})).await;
    }
    let tail = sink.read_tail(3).await.expect("a tail");
    assert_eq!(tail.len(), 3, "a partial first line must be dropped");
    assert_eq!(
        tail.iter()
            .map(|row| row["i"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![37, 38, 39]
    );
    for row in &tail {
        assert_eq!(
            row["text"].as_str().expect("the row's own payload"),
            filler,
            "no row may be a fragment of another"
        );
    }
}

#[tokio::test]
async fn read_tail_of_a_short_log_is_the_whole_log() {
    let dir = temp_dir().join("s_tail_short");
    let sink = JsonlSessionSink::new("s_tail_short", dir);
    write(&sink, json!({"i": 0})).await;
    write(&sink, json!({"i": 1})).await;
    let tail = sink.read_tail(200).await.expect("a tail");
    assert_eq!(tail.len(), 2, "the metadata header is not a row");
    assert_eq!(tail[0]["i"], 0);
    assert_eq!(tail[1]["i"], 1);
}

#[tokio::test]
async fn read_tail_of_an_empty_log_is_empty() {
    let dir = temp_dir().join("s_tail_empty");
    let sink = JsonlSessionSink::new("s_tail_empty", dir);
    assert!(sink.read_tail(10).await.expect("a tail").is_empty());
}

/// The performance claim, at the only layer that can prove it: the
/// bounded read returns no more bytes than it was asked for, so a
/// tail read of a long log cannot have read the whole file.
#[tokio::test]
async fn a_tail_read_never_returns_more_than_its_budget() {
    use crate::fs::FsBackend;

    let dir = temp_dir().join("s_tail_budget");
    let sink = JsonlSessionSink::new("s_tail_budget", dir.clone());
    let filler = "y".repeat(1000);
    for i in 0..1000 {
        write(&sink, json!({"i": i, "text": filler})).await;
    }
    let path = dir.join("events.jsonl");
    let len = std::fs::metadata(&path).expect("the log").len();
    assert!(len > 1_000_000, "the fixture log is long: {len} bytes");

    let fs = FsBackend::default();
    let chunk = fs.read_tail(&path, 4096).await.expect("a bounded read");
    assert_eq!(chunk.bytes.len(), 4096, "the read stops at its budget");
    assert!(chunk.start > 0, "a 1 MB file does not start at the tail");
    let whole = std::fs::read(&path).expect("the log");
    assert_eq!(
        chunk.bytes,
        whole[whole.len() - 4096..],
        "and it is the file's tail"
    );
}

/// A file exactly as long as the byte budget still starts at offset
/// 0, so its first line is whole. The budget comparison this
/// replaced called that "mid-file" and dropped a real row.
#[tokio::test]
async fn a_log_exactly_as_long_as_the_chunk_starts_at_offset_zero() {
    let dir = temp_dir().join("s_tail_exact");
    std::fs::create_dir_all(&dir).unwrap();
    // 1024 rows of exactly 8 bytes = 8192 = TAIL_READ_CHUNK.
    let mut body = String::new();
    for index in 0..1024 {
        body.push_str(&format!("{{\"a\":{}}}\n", index % 10));
    }
    assert_eq!(body.len() as u64, TAIL_READ_CHUNK, "one exact chunk");
    let header = format!(
        "{}\n",
        serde_json::to_string(&SessionMetadataHeader::fresh("s_tail_exact"))
            .unwrap()
    );
    std::fs::write(dir.join("events.jsonl"), format!("{header}{body}"))
        .unwrap();
    let sink = JsonlSessionSink::new("s_tail_exact", dir);

    let chunk = crate::fs::FsBackend::default()
        .read_tail(&sink.events_path(), TAIL_READ_CHUNK)
        .await
        .expect("a bounded read");
    assert_eq!(chunk.bytes.len() as u64, TAIL_READ_CHUNK);
    assert_eq!(
        chunk.start,
        header.len() as u64,
        "the read starts at a line boundary: right after the header"
    );
    assert_eq!(chunk.bytes, body.as_bytes(), "and it is the rows");

    let rows = sink.read_tail(2000).await.expect("a tail");
    assert_eq!(rows.len(), 1024, "every row is kept, the first included");
    assert_eq!(rows[0]["a"], 0);
    assert_eq!(rows[1023]["a"], 3);
}

#[tokio::test]
async fn read_head_returns_the_first_event_not_the_header() {
    let dir = temp_dir().join("s_head");
    let sink = JsonlSessionSink::new("s_head", dir);
    write(&sink, json!({"i": 0})).await;
    write(&sink, json!({"i": 1})).await;
    let head = sink.read_head().await.expect("a head");
    assert_eq!(head.expect("the log has a first row")["i"], 0);
    assert!(
        JsonlSessionSink::new("s_head_empty", temp_dir().join("s_head_e"))
            .read_head()
            .await
            .expect("a head")
            .is_none(),
        "an empty log has no first row"
    );
}

#[tokio::test]
async fn read_head_rows_returns_the_first_n_rows_in_order() {
    let dir = temp_dir().join("s_head_n");
    let sink = JsonlSessionSink::new("s_head_n", dir);
    for i in 0..10 {
        write(&sink, json!({"i": i})).await;
    }
    let head = sink.read_head_rows(3).await.expect("a head window");
    assert_eq!(
        head.iter()
            .map(|row| row["i"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "the window is the log's start, in order"
    );
}

#[tokio::test]
async fn read_head_rows_of_a_short_log_is_the_whole_log() {
    let dir = temp_dir().join("s_head_short");
    let sink = JsonlSessionSink::new("s_head_short", dir);
    write(&sink, json!({"i": 0})).await;
    write(&sink, json!({"i": 1})).await;
    let head = sink.read_head_rows(200).await.expect("a head window");
    assert_eq!(head.len(), 2, "the metadata header is not a row");
    assert_eq!(head[0]["i"], 0);
    assert_eq!(head[1]["i"], 1);
}

#[tokio::test]
async fn read_head_rows_grows_the_budget_for_wide_rows() {
    // Rows wider than the first chunk force a growth pass; the
    // window must still return whole rows in order, never a
    // fragment.
    let dir = temp_dir().join("s_head_wide");
    let sink = JsonlSessionSink::new("s_head_wide", dir);
    let filler = "z".repeat(4000);
    for i in 0..6 {
        write(&sink, json!({"i": i, "text": filler})).await;
    }
    let head = sink.read_head_rows(5).await.expect("a head window");
    assert_eq!(head.len(), 5);
    for (index, row) in head.iter().enumerate() {
        assert_eq!(row["i"], index, "order is the log's own");
        assert_eq!(
            row["text"].as_str().expect("the row's own payload"),
            filler,
            "no row may be a fragment of another"
        );
    }
}

#[tokio::test]
async fn jsonl_sink_round_trips_events_across_instances() {
    let dir = temp_dir().join("s1");
    let sink = JsonlSessionSink::new("s1", dir.clone());
    write(&sink, json!({"role": "user", "text": "hi"})).await;
    write(&sink, json!({"role": "assistant", "text": "hello"})).await;
    // Re-open the same dir — should see the same two events.
    let sink2 = JsonlSessionSink::new("s1", dir);
    let events = sink2.read().await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["text"], "hi");
    assert_eq!(events[1]["text"], "hello");
}

#[tokio::test]
async fn jsonl_sink_rejects_appends_after_close() {
    let dir = temp_dir().join("s2");
    let sink = JsonlSessionSink::new("s2", dir);
    sink.close(SessionEndReason::Completed).await.unwrap();
    let err = sink.append(&json!({"x": 1})).await.unwrap_err();
    assert_eq!(err, SessionError::Closed);
}

#[tokio::test]
async fn jsonl_sink_persists_across_appends_in_sequence_order() {
    let dir = temp_dir().join("s3");
    let sink = JsonlSessionSink::new("s3", dir);
    for i in 0..5 {
        write(&sink, json!({"i": i})).await;
    }
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 5);
    let events = sink.read().await.unwrap();
    for (idx, ev) in events.iter().enumerate() {
        assert_eq!(ev["i"], idx as i64);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jsonl_sink_concurrent_appends_preserve_all_records() {
    let dir = temp_dir().join("s_concurrent");
    let sink = JsonlSessionSink::new("s_concurrent", dir);
    let mut handles = Vec::new();
    for i in 0..50 {
        let s = sink.clone();
        handles.push(tokio::spawn(async move {
            s.alloc_seq();
            s.append(&json!({"i": i})).await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 50);
    let events = sink.read().await.unwrap();
    assert_eq!(events.len(), 50);
    let mut seen = std::collections::HashSet::new();
    for ev in &events {
        let i = ev["i"].as_i64().unwrap();
        assert!(seen.insert(i), "duplicate seq {i}");
    }
    assert_eq!(seen.len(), 50);
}

#[tokio::test]
async fn jsonl_sink_read_from_zero_matches_read() {
    let dir = temp_dir().join("s_from_zero");
    let sink = JsonlSessionSink::new("s_from_zero", dir);
    for i in 0..4 {
        write(&sink, json!({"i": i})).await;
    }
    let all = sink.read().await.unwrap();
    let from_zero = sink.read_from(0).await.unwrap();
    assert_eq!(
        all, from_zero,
        "`read_from(0)` must mirror the legacy `read()` payload"
    );
}

#[tokio::test]
async fn jsonl_sink_read_from_n_returns_strict_suffix() {
    let dir = temp_dir().join("s_from_n");
    let sink = JsonlSessionSink::new("s_from_n", dir);
    for i in 0..6 {
        write(&sink, json!({"i": i})).await;
    }
    let from_three = sink.read_from(3).await.unwrap();
    assert_eq!(from_three.len(), 3);
    assert_eq!(from_three[0]["i"], 3);
    assert_eq!(from_three[2]["i"], 5);
    let from_six = sink.read_from(6).await.unwrap();
    assert!(from_six.is_empty());
}

#[tokio::test]
async fn jsonl_sink_read_from_past_end_returns_empty() {
    let dir = temp_dir().join("s_past_end");
    let sink = JsonlSessionSink::new("s_past_end", dir);
    for i in 0..2 {
        write(&sink, json!({"i": i})).await;
    }
    let beyond = sink.read_from(u64::MAX).await.unwrap();
    assert!(beyond.is_empty());
}

#[tokio::test]
async fn jsonl_sink_read_from_resumes_across_instance() {
    let dir = temp_dir().join("s_resume");
    let sink = JsonlSessionSink::new("s_resume", dir.clone());
    for i in 0..3 {
        write(&sink, json!({"i": i})).await;
    }
    let snap = sink.snapshot().await.unwrap();
    drop(sink);
    let sink2 = JsonlSessionSink::new("s_resume", dir);
    let suffix = sink2.read_from(snap.last_event_seq).await.unwrap();
    assert!(
        suffix.is_empty(),
        "no events appended after the snapshot, so the suffix must be empty"
    );
}

// -----------------------------------------------------------------
// Schema versioning
// -----------------------------------------------------------------

#[tokio::test]
async fn jsonl_sink_writes_metadata_header_on_first_append() {
    let dir = temp_dir().join("s_meta_first");
    let sink = JsonlSessionSink::new("s_meta_first", dir.clone());
    write(&sink, json!({"i": 0})).await;
    let raw = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "header + 1 event");
    let header: SessionMetadataHeader = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header.meta.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(header.meta.id, "s_meta_first");
}

#[tokio::test]
async fn jsonl_sink_count_lines_skips_header() {
    let dir = temp_dir().join("s_meta_count");
    let sink = JsonlSessionSink::new("s_meta_count", dir.clone());
    for i in 0..3 {
        write(&sink, json!({"i": i})).await;
    }
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 3, "header must not be counted");
    drop(sink);
    let sink2 = JsonlSessionSink::new("s_meta_count", dir);
    let snap2 = sink2.snapshot().await.unwrap();
    assert_eq!(snap2.last_event_seq, 3, "re-open reads the same count");
}

/// Craft a log whose header carries `version` followed by
/// `rows`. The header is written the way the sink writes it,
/// except the schema version is caller-chosen.
fn write_versioned_log(dir: &Path, version: u32, rows: &[Value]) {
    std::fs::create_dir_all(dir).unwrap();
    let header = json!({
        "_meta": {
            "kind": "uva_session_meta",
            "schema_version": version,
            "id": "s_gate",
        }
    });
    let mut body = serde_json::to_string(&header).unwrap();
    body.push('\n');
    for row in rows {
        body.push_str(&serde_json::to_string(row).unwrap());
        body.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), body).unwrap();
}

#[tokio::test]
async fn jsonl_sink_refuses_a_v1_log() {
    let dir = temp_dir().join("s_v1");
    write_versioned_log(&dir, 1, &[json!({"i": 0}), json!({"i": 1})]);
    let sink = JsonlSessionSink::new("s_v1", dir);
    let error = sink.read().await.unwrap_err();
    assert_eq!(
        error,
        SessionError::UnsupportedSchemaVersion {
            found: 1,
            supported: 2
        }
    );
    assert!(
        error.to_string().contains("older"),
        "an older file says so: {error}"
    );
    assert!(
        error
            .to_string()
            .contains("export the session with a build that reads v1"),
        "the error names the way out: {error}"
    );
}

#[tokio::test]
async fn jsonl_sink_refuses_a_newer_log() {
    let dir = temp_dir().join("s_v3");
    write_versioned_log(&dir, 3, &[json!({"i": 0})]);
    let sink = JsonlSessionSink::new("s_v3", dir);
    let error = sink.read().await.unwrap_err();
    assert_eq!(
        error,
        SessionError::UnsupportedSchemaVersion {
            found: 3,
            supported: 2
        }
    );
    assert!(
        error.to_string().contains("newer"),
        "a newer file says so: {error}"
    );
    assert!(
        error.to_string().contains(
            "export or reopen the session with a build that \
             supports v3"
        ),
        "the error names the way out: {error}"
    );
}

/// The append path refuses a log this build cannot read: a row
/// written into a v1 file would leave a file whose header says
/// one thing and whose rows say another, and every later reader
/// — including the build the error names — would refuse it.
#[tokio::test]
async fn jsonl_sink_refuses_to_append_into_a_v1_log() {
    let dir = temp_dir().join("s_v1_append");
    write_versioned_log(&dir, 1, &[json!({"i": 0})]);
    let path = dir.join("events.jsonl");
    let before = std::fs::read_to_string(&path).unwrap();
    let sink = JsonlSessionSink::new("s_v1_append", dir);
    let error = sink.append(&json!({"i": 1})).await.unwrap_err();
    assert_eq!(
        error,
        SessionError::UnsupportedSchemaVersion {
            found: 1,
            supported: 2
        }
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "the v1 log is not written to"
    );
}

/// The same refusal on the batch path.
#[tokio::test]
async fn jsonl_sink_refuses_to_append_a_batch_into_a_v1_log() {
    let dir = temp_dir().join("s_v1_batch");
    write_versioned_log(&dir, 1, &[json!({"i": 0})]);
    let path = dir.join("events.jsonl");
    let before = std::fs::read_to_string(&path).unwrap();
    let sink = JsonlSessionSink::new("s_v1_batch", dir);
    let error = sink.append_many(&[json!({"i": 1})]).await.unwrap_err();
    assert_eq!(
        error,
        SessionError::UnsupportedSchemaVersion {
            found: 1,
            supported: 2
        }
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "the v1 log is not written to"
    );
}

/// The tail path is gated too.
///
/// A log larger than the tail chunk is read from mid-file, where
/// the header is not in the bytes — so a tail read that only
/// gated on the bytes it saw would recover a v1 log's rows
/// under this build's semantics. The sink knows the file's
/// version (it read it when it opened the log) and refuses
/// before a single row is read.
#[tokio::test]
async fn jsonl_sink_refuses_a_v1_log_on_the_tail_path() {
    let dir = temp_dir().join("s_v1_tail");
    let rows: Vec<Value> = (0..400)
        .map(|i| json!({"i": i, "pad": "x".repeat(64)}))
        .collect();
    write_versioned_log(&dir, 1, &rows);
    let path = dir.join("events.jsonl");
    assert!(
        std::fs::metadata(&path).unwrap().len() > 8 * 1024,
        "the fixture must be larger than the tail chunk, or the \
         read would start at offset 0"
    );
    let sink = JsonlSessionSink::new("s_v1_tail", dir);
    let error = sink.read_tail(10).await.unwrap_err();
    assert_eq!(
        error,
        SessionError::UnsupportedSchemaVersion {
            found: 1,
            supported: 2
        }
    );
}

#[tokio::test]
async fn jsonl_sink_refuses_a_headerless_v0_log() {
    // A pre-versioning file has no header; its implied
    // version is 0 and the reader refuses it too.
    let dir = temp_dir().join("s_legacy_v0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("events.jsonl"),
        "{\"i\":0}\n{\"i\":1}\n{\"i\":2}\n",
    )
    .unwrap();
    let sink = JsonlSessionSink::new("s_legacy_v0", dir);
    let error = sink.read().await.unwrap_err();
    assert_eq!(
        error,
        SessionError::UnsupportedSchemaVersion {
            found: 0,
            supported: 2
        }
    );
}

#[tokio::test]
async fn jsonl_sink_reads_a_v2_log() {
    let dir = temp_dir().join("s_v2");
    write_versioned_log(&dir, 2, &[json!({"i": 0}), json!({"i": 1})]);
    let sink = JsonlSessionSink::new("s_v2", dir);
    let events = sink.read().await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["i"], 0);
    assert_eq!(events[1]["i"], 1);
}

/// A `feedback` row written before the rating's target changed
/// meaning still reads.
///
/// The target is the rated row's ordinal now; a row written before
/// that change carries a client-side label in the same field (the
/// browser's UUID, the TUI's `msg-<n>` count). The row's **shape**
/// did not change — only what the field holds — so the log's version
/// does not move and the old row still decodes.
#[tokio::test]
async fn a_v2_feedback_row_written_before_the_meaning_changed_still_reads() {
    let dir = temp_dir().join("s_v2_feedback");
    write_versioned_log(
        &dir,
        2,
        &[json!({
            "type": "feedback",
            "seq": 3,
            "ts": "2026-10-27T09:14:04Z",
            "message_id": "msg-1",
            "thumbs_up": true,
        })],
    );
    let sink = JsonlSessionSink::new("s_v2_feedback", dir);
    let events = sink.read().await.unwrap();
    assert_eq!(events.len(), 1, "the old row is still readable");
    let event = crate::SessionEvent::from_value(&events[0])
        .expect("the old feedback row is still a typed row");
    assert!(
        matches!(
            &event,
            crate::SessionEvent::Feedback {
                message_id, ..
            } if message_id == "msg-1"
        ),
        "the old row keeps its client-side label: {event:?}"
    );
}

// -----------------------------------------------------------------
// Idempotency
// -----------------------------------------------------------------

#[tokio::test]
async fn jsonl_sink_idempotent_append_writes_the_row_once() {
    let dir = temp_dir().join("s_idem_basic");
    let sink = JsonlSessionSink::new("s_idem_basic", dir);
    sink.alloc_seq();
    sink.append_with_key(Some("evt-1"), &json!({"i": 0}))
        .await
        .unwrap();
    sink.alloc_seq();
    sink.append_with_key(Some("evt-1"), &json!({"i": 0}))
        .await
        .unwrap();
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 2, "both reservations were taken");
    let events = sink.read().await.unwrap();
    assert_eq!(events.len(), 1, "no second row was written");
}

#[tokio::test]
async fn jsonl_sink_idempotent_distinct_keys_both_written() {
    let dir = temp_dir().join("s_idem_distinct");
    let sink = JsonlSessionSink::new("s_idem_distinct", dir);
    sink.alloc_seq();
    sink.append_with_key(Some("a"), &json!({"i": 0}))
        .await
        .unwrap();
    sink.alloc_seq();
    sink.append_with_key(Some("b"), &json!({"i": 1}))
        .await
        .unwrap();
    sink.append_with_key(Some("a"), &json!({"i": 0}))
        .await
        .unwrap();
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 2);
    let events = sink.read().await.unwrap();
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn jsonl_sink_idempotent_none_key_legacy_behavior() {
    // `None` key skips the LRU entirely, preserving the
    // pre-idempotency contract for callers that do not care.
    let dir = temp_dir().join("s_idem_none");
    let sink = JsonlSessionSink::new("s_idem_none", dir);
    sink.alloc_seq();
    sink.append_with_key(None, &json!({"i": 0})).await.unwrap();
    sink.alloc_seq();
    sink.append_with_key(None, &json!({"i": 0})).await.unwrap();
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 2);
}

// -----------------------------------------------------------------
// Batched append
// -----------------------------------------------------------------

#[tokio::test]
async fn jsonl_sink_append_many_writes_batch_with_single_fsync() {
    let dir = temp_dir().join("s_many_basic");
    let sink = JsonlSessionSink::new("s_many_basic", dir.clone());
    let batch: Vec<Value> = (0..10).map(|i| json!({"i": i})).collect();
    for _ in &batch {
        sink.alloc_seq();
    }
    sink.append_many(&batch).await.unwrap();
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 10);
    let events = sink.read().await.unwrap();
    assert_eq!(events.len(), 10);
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev["i"].as_i64().unwrap(), i as i64);
    }
}

/// A batch that cannot be written leaves the log without the
/// batch: `append_many` promises that a refused batch wrote
/// nothing, and the writer re-queues those rows under the same
/// ordinals when it does.
#[tokio::test]
async fn jsonl_sink_append_many_leaves_no_partial_rows() {
    let dir = temp_dir().join("s_partial");
    std::fs::create_dir_all(&dir).unwrap();
    // The log path is a directory: the sink cannot open it for
    // writing, so the batch never reaches the disk.
    std::fs::create_dir_all(dir.join("events.jsonl")).unwrap();
    let sink = JsonlSessionSink::new("s_partial", dir.clone());

    let error = sink
        .append_many(&[json!({"i": 0}), json!({"i": 1})])
        .await
        .unwrap_err();
    assert!(matches!(error, SessionError::AppendFailed(_)));
    assert!(
        std::fs::read_dir(dir.join("events.jsonl"))
            .unwrap()
            .next()
            .is_none(),
        "a refused batch leaves no rows behind",
    );
}

#[tokio::test]
async fn jsonl_sink_append_many_empty_is_noop() {
    let dir = temp_dir().join("s_many_empty");
    let sink = JsonlSessionSink::new("s_many_empty", dir);
    sink.append_many(&[]).await.unwrap();
    let events = sink.read().await.unwrap();
    assert!(events.is_empty());
}

#[tokio::test]
async fn jsonl_sink_append_many_on_closed_returns_error() {
    let dir = temp_dir().join("s_many_closed");
    let sink = JsonlSessionSink::new("s_many_closed", dir);
    sink.close(SessionEndReason::Completed).await.unwrap();
    let err = sink.append_many(&[json!({"x": 1})]).await.unwrap_err();
    assert_eq!(err, SessionError::Closed);
}

#[tokio::test]
async fn jsonl_sink_append_many_combines_with_subsequent_appends() {
    let dir = temp_dir().join("s_many_combine");
    let sink = JsonlSessionSink::new("s_many_combine", dir.clone());
    write(&sink, json!({"i": 0})).await;
    for _ in 1..5 {
        sink.alloc_seq();
    }
    sink.append_many(&(1..5).map(|i| json!({"i": i})).collect::<Vec<_>>())
        .await
        .unwrap();
    write(&sink, json!({"i": 5})).await;
    let snap = sink.snapshot().await.unwrap();
    assert_eq!(snap.last_event_seq, 6);
    drop(sink);
    let sink2 = JsonlSessionSink::new("s_many_combine", dir);
    let events = sink2.read().await.unwrap();
    assert_eq!(events.len(), 6);
    assert_eq!(events[3]["i"], 3);
}

#[tokio::test]
async fn jsonl_sink_append_many_is_faster_than_per_row_appends() {
    // Soft assertion: batched path completes within a
    // generous wallclock budget that a per-row fsync path
    // would blow past on slow CI disks. The point is to
    // prove the single-fsync implementation is wired up,
    // not to enforce a hard SLA.
    let dir = temp_dir().join("s_many_perf");
    let sink = JsonlSessionSink::new("s_many_perf", dir);
    let batch: Vec<Value> = (0..200).map(|i| json!({"i": i})).collect();
    let started = std::time::Instant::now();
    sink.append_many(&batch).await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "200-row batch should complete well under 30s, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------
// A dropped awaiter cannot desync the mark from the file
// ---------------------------------------------------------------------

/// A blocking seam that starts the work eagerly on its own thread —
/// the write runs to completion even when nobody ever awaits the
/// answer, which is exactly the "bytes landed, caller cancelled"
/// window a compaction timeout opens on a flush.
struct RunsOutBlocking;

impl uva_core::blocking::Blocking for RunsOutBlocking {
    fn run(
        &self,
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> futures::future::BoxFuture<'static, ()> {
        std::thread::spawn(f);
        Box::pin(std::future::ready(()))
    }
}

fn stamped_rows(count: u64) -> Vec<Value> {
    (1..=count)
        .map(|seq| json!({"type": "Model", "seq": seq}))
        .collect()
}

/// The caller that awaits the write is dropped while the blocking
/// thread is still writing: the bytes land, but nobody absorbs them
/// into the ordinal mark. The next `append_many` must reconcile the
/// write's fate from the thread itself — a batch whose bytes landed
/// is skipped, never written a second time under the same ordinals.
#[tokio::test]
async fn a_landed_write_whose_caller_was_cancelled_is_not_written_twice() {
    let root = temp_dir();
    let sink = JsonlSessionSink::new("s1", root.clone())
        .with_blocking(std::sync::Arc::new(RunsOutBlocking));
    let rows = stamped_rows(3);

    // One poll starts the write and suspends at the result await;
    // `now_or_never` then drops the future — the cancellation.
    use futures::FutureExt as _;
    assert!(
        sink.append_many(&rows).now_or_never().is_none(),
        "the append suspends at the write's answer"
    );
    // No sleep follows: wherever the eager thread happens to be
    // (not yet scheduled, mid-write, or already done), the
    // retry's reconcile must wait the write out on `done` and
    // then read a mark that describes the file — the timing is
    // the test.

    // The retry: it must find the batch in the file and owe nothing.
    sink.append_many(&rows).await.expect("the retry succeeds");

    let on_disk = sink.read().await.expect("the log reads back");
    let mut seqs: Vec<u64> = on_disk
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .collect();
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "each ordinal exactly once: {on_disk:?}"
    );
}

/// What the seam defers: the write closures themselves.
type StashedWork = Vec<Box<dyn FnOnce() + Send + 'static>>;

/// A blocking seam that defers the closure entirely: `run` stashes
/// it and the test decides whether it ever runs. This holds the
/// window between "the write was dispatched" and "the closure's
/// first instruction" open as long as the test needs — the window a
/// scheduled-but-not-started thread lives in, which no flag can
/// tell apart from a closure that will never run.
#[derive(Clone)]
struct DeferredBlocking {
    stashed: Arc<std::sync::Mutex<StashedWork>>,
    /// While `true`, every closure is stashed; once released, the
    /// seam runs everything eagerly again — the sink's one fs seam
    /// serves the writes AND the reads, so a test that holds writes
    /// must let the final verification read go through.
    holding: Arc<std::sync::atomic::AtomicBool>,
}

impl Default for DeferredBlocking {
    fn default() -> Self {
        Self {
            stashed: Arc::new(std::sync::Mutex::new(Vec::new())),
            // Holding is the point of the seam: released only when
            // the test says so.
            holding: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

impl DeferredBlocking {
    /// Run every stashed closure on its own thread.
    fn start_all(&self) {
        let stashed: Vec<_> = self
            .stashed
            .lock()
            .expect("DeferredBlocking poisoned")
            .drain(..)
            .collect();
        for f in stashed {
            std::thread::spawn(f);
        }
    }

    /// Drop every stashed closure unrun — the sender inside goes
    /// with them, so the write's `done` resolves `Err`.
    fn discard_all(&self) {
        self.stashed
            .lock()
            .expect("DeferredBlocking poisoned")
            .clear();
    }

    /// Stop holding: later closures run eagerly.
    fn release(&self) {
        self.holding
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Wait until `n` closures sit in the stash.
    async fn wait_until_stashed(&self, n: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while self
                .stashed
                .lock()
                .expect("DeferredBlocking poisoned")
                .len()
                < n
            {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the write was dispatched");
    }
}

impl uva_core::blocking::Blocking for DeferredBlocking {
    fn run(
        &self,
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> futures::future::BoxFuture<'static, ()> {
        if self.holding.load(std::sync::atomic::Ordering::SeqCst) {
            self.stashed
                .lock()
                .expect("DeferredBlocking poisoned")
                .push(f);
        } else {
            std::thread::spawn(f);
        }
        Box::pin(std::future::ready(()))
    }
}

/// The residual window: the write was dispatched and its thread
/// queued, but the closure has not started — and the awaiter is
/// already gone. A reconcile that treats that as "never happened"
/// re-offers the batch and the thread then writes it a second time.
/// The retry must instead wait the write out and absorb its fate.
#[tokio::test]
async fn a_dispatched_write_held_before_its_closure_starts_is_not_re_offered() {
    use futures::FutureExt as _;

    let root = temp_dir();
    let deferred = DeferredBlocking::default();
    let sink = JsonlSessionSink::new("s1", root.clone())
        .with_blocking(Arc::new(deferred.clone()) as Arc<dyn Blocking>);
    let rows = stamped_rows(3);

    // One poll dispatches the write (the closure is stashed, not
    // started) and suspends at the result await; the drop is the
    // cancellation. The closure stays held — the exact window.
    assert!(
        sink.append_many(&rows).now_or_never().is_none(),
        "the append suspends at the write's answer"
    );

    let mut retry = tokio::spawn({
        let sink = sink.clone();
        async move { sink.append_many(&rows).await }
    });
    // While the write is held, the retry must wait for its fate, not
    // re-offer the batch — a reconcile that guessed "never happened"
    // would already have dispatched a second write.
    tokio::time::timeout(std::time::Duration::from_millis(100), &mut retry)
        .await
        .expect_err("the retry waits for the held write's fate");

    // Let the held write land: the retry reconciles it and owes
    // nothing — no second dispatch ever happens.
    deferred.start_all();
    deferred.release();
    retry
        .await
        .expect("the retry joins")
        .expect("the retry succeeds");

    let on_disk = sink.read().await.expect("the log reads back");
    let mut seqs: Vec<u64> = on_disk
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .collect();
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "each ordinal exactly once: {on_disk:?}"
    );
}

/// The other half: a dispatch whose closure is dropped before it
/// ever ran wrote nothing, and the re-offer must write the batch
/// exactly once — a reconcile that over-absorbed would skip rows the
/// file never held.
#[tokio::test]
async fn a_write_whose_closure_never_ran_is_written_once_by_the_retry() {
    use futures::FutureExt as _;

    let root = temp_dir();
    let deferred = DeferredBlocking::default();
    let sink = JsonlSessionSink::new("s1", root.clone())
        .with_blocking(Arc::new(deferred.clone()) as Arc<dyn Blocking>);
    let rows = stamped_rows(3);

    // Dispatched, then the awaiter is dropped; the closure is
    // discarded unrun — `done` resolves `Err`, the honest answer.
    assert!(
        sink.append_many(&rows).now_or_never().is_none(),
        "the append suspends at the write's answer"
    );
    deferred.discard_all();

    let retry = tokio::spawn({
        let sink = sink.clone();
        async move { sink.append_many(&rows).await }
    });
    deferred.wait_until_stashed(1).await;
    deferred.start_all();
    deferred.release();
    retry
        .await
        .expect("the retry joins")
        .expect("the retry writes the batch");

    let on_disk = sink.read().await.expect("the log reads back");
    let mut seqs: Vec<u64> = on_disk
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .collect();
    seqs.sort_unstable();
    assert_eq!(seqs, vec![1, 2, 3], "the batch lands once: {on_disk:?}");
}
