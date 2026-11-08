//! Content-Length codec tests: fragmentation, coalescing, bad
//! headers, the size ceiling, truncation, and non-ASCII payloads.
use std::{io::Cursor, time::Duration};

use serde_json::json;
use tokio::io::{AsyncWriteExt, duplex};

use super::framing::{
    DEFAULT_MAX_FRAME_SIZE,
    FrameError,
    read_frame,
    read_frame_blocking,
    write_frame,
};

/// Encode one frame the way a peer would put it on the wire.
fn frame_bytes(payload: &[u8]) -> Vec<u8> {
    let mut bytes =
        format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    bytes.extend_from_slice(payload);
    bytes
}

#[tokio::test]
async fn parses_a_fragmented_frame() {
    // A capacity below the header length forces the writer to hand
    // the frame over in pieces; the reader must wait for the whole
    // declared payload rather than treat a partial frame as bad.
    let (mut client, mut server) = duplex(8);
    let payload = br#"{"jsonrpc":"2.0","id":1,"method":"health/ping"}"#;
    let wire = frame_bytes(payload);
    let writer = tokio::spawn(async move {
        for chunk in wire.chunks(3) {
            client.write_all(chunk).await.expect("duplex accepts bytes");
            client.flush().await.expect("duplex flushes");
        }
        drop(client);
    });

    let frame = read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
        .await
        .expect("a fragmented frame is read once it is complete");
    assert_eq!(frame, payload);
    writer.await.expect("the writer finishes");
}

#[tokio::test]
async fn parses_coalesced_frames() {
    let (mut client, mut server) = duplex(1024);
    let first = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    let second = br#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
    let mut wire = frame_bytes(first);
    wire.extend_from_slice(&frame_bytes(second));
    client.write_all(&wire).await.expect("duplex accepts bytes");
    client.flush().await.expect("duplex flushes");

    assert_eq!(
        read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
            .await
            .expect("the first frame is read"),
        first
    );
    assert_eq!(
        read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
            .await
            .expect("the second frame follows the first"),
        second
    );
}

#[tokio::test]
async fn rejects_a_frame_without_content_length() {
    let (mut client, mut server) = duplex(128);
    client
        .write_all(b"X-Trace: 1\r\n\r\n{}")
        .await
        .expect("duplex accepts bytes");
    client.flush().await.expect("duplex flushes");

    let error = read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
        .await
        .expect_err("a frame without Content-Length has no boundary");
    assert_eq!(error.kind(), "missing_header");
    assert!(matches!(error, FrameError::MissingHeader));
}

#[tokio::test]
async fn rejects_an_oversized_frame_without_reading_it() {
    let (mut client, mut server) = duplex(64);
    client
        .write_all(b"Content-Length: 9000\r\n\r\n")
        .await
        .expect("duplex accepts the header");
    client.flush().await.expect("duplex flushes");
    // `client` stays open: if the codec waited for the 9000 bytes it
    // would hang instead of rejecting the frame.

    let error = tokio::time::timeout(
        Duration::from_secs(1),
        read_frame(&mut server, 100),
    )
    .await
    .expect("the header alone is enough to reject the frame")
    .expect_err("the declared length exceeds the ceiling");
    assert_eq!(error.kind(), "too_large");
    assert!(matches!(
        error,
        FrameError::TooLarge {
            length: 9000,
            max: 100
        }
    ));
}

#[tokio::test]
async fn rejects_a_truncated_frame() {
    let (mut client, mut server) = duplex(128);
    client
        .write_all(b"Content-Length: 10\r\n\r\n{}")
        .await
        .expect("duplex accepts the partial frame");
    client.flush().await.expect("duplex flushes");
    drop(client);

    let error = read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
        .await
        .expect_err("EOF before the declared length is an error");
    assert_eq!(error.kind(), "truncated");
    assert!(matches!(
        error,
        FrameError::Truncated {
            expected: 10,
            actual: 2
        }
    ));
}

#[tokio::test]
async fn round_trips_a_non_ascii_payload() {
    let payload = json!({"message": "héllo — 世界"}).to_string();
    let payload = payload.as_bytes().to_vec();
    // The declared length is the byte length, not the character
    // count; the round trip only closes if `write_frame` uses bytes.
    assert_ne!(
        payload.len(),
        String::from_utf8_lossy(&payload).chars().count()
    );

    let (mut client, mut server) = duplex(128);
    write_frame(&mut client, &payload)
        .await
        .expect("the frame is written");
    let frame = read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
        .await
        .expect("the frame is read back");
    assert_eq!(frame, payload);
}

#[test]
fn blocking_reader_reads_a_cursor_frame() {
    let payload = br#"{"jsonrpc":"2.0","method":"ready"}"#;
    let mut cursor = Cursor::new(frame_bytes(payload));
    assert_eq!(
        read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
            .expect("a buffered frame is read"),
        payload
    );
}

#[test]
fn blocking_reader_keeps_coalesced_frames_separate() {
    let first = b"one";
    let second = b"two";
    let mut wire = frame_bytes(first);
    wire.extend_from_slice(&frame_bytes(second));
    let mut cursor = Cursor::new(wire);
    assert_eq!(
        read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
            .expect("the first frame is read"),
        first
    );
    assert_eq!(
        read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
            .expect("the second frame follows"),
        second
    );
}

#[test]
fn blocking_reader_reports_a_clean_end_of_stream() {
    let mut cursor = Cursor::new(Vec::new());
    let error = read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
        .expect_err("an empty stream has no frame");
    assert_eq!(error.kind(), "io");
    assert!(error.is_end_of_stream());
}

#[test]
fn accepts_a_case_insensitive_header_and_extra_headers() {
    let wire = b"X-Trace: abc\r\ncontent-length: 2\r\n\r\n{}";
    let mut cursor = Cursor::new(wire);
    assert_eq!(
        read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
            .expect("the header name is case-insensitive"),
        b"{}"
    );
}

#[test]
fn rejects_a_duplicate_content_length() {
    let wire = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
    let mut cursor = Cursor::new(wire);
    let error = read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
        .expect_err("two lengths have no single boundary");
    assert_eq!(error.kind(), "bad_header");
}

#[test]
fn rejects_a_non_numeric_content_length() {
    let wire = b"Content-Length: two\r\n\r\n{}";
    let mut cursor = Cursor::new(wire);
    let error = read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
        .expect_err("a non-numeric length names no boundary");
    assert_eq!(error.kind(), "invalid_content_length");
}

#[test]
fn rejects_a_header_line_without_a_colon() {
    let wire = b"Content-Length 2\r\n\r\n{}";
    let mut cursor = Cursor::new(wire);
    let error = read_frame_blocking(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
        .expect_err("a header line is `name: value`");
    assert_eq!(error.kind(), "bad_header");
}
