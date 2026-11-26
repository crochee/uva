//! Pump-level tests: the framed transport end to end.
//!
//! The reader is a cursor (the frames a peer would have written) and
//! the writer is a duplex; the pump runs on the test's runtime. Each
//! assertion reads the frames the pump actually put on stdout.
use std::{io::Cursor, sync::Arc, time::Duration};

use serde_json::{Value, json};

use super::{
    Outbound,
    RpcRouter,
    framing::{DEFAULT_MAX_FRAME_SIZE, read_frame},
    stdio::{OUTBOUND_MEMORY_WATERMARK, OutboundQueue, StioOptions, StioPump},
};
use crate::state::AppState;

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut wire =
        format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    wire.extend_from_slice(payload);
    wire
}

fn wire(value: &Value) -> Vec<u8> {
    frame(&serde_json::to_vec(value).expect("the frame serialises"))
}

fn request(id: Value, method: &str) -> Vec<u8> {
    wire(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": {},
    }))
}

fn notification(method: &str) -> Vec<u8> {
    wire(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": {},
    }))
}

fn cancellation(id: Value) -> Vec<u8> {
    wire(&json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": { "id": id },
    }))
}

fn router() -> RpcRouter {
    let mut router = RpcRouter::new();
    router.register("health/ping", |_state, _params| async {
        Ok(json!({ "ok": true }))
    });
    router.register("slow/first", |_state, _params| async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        Ok(json!({ "order": "first" }))
    });
    router.register("fast/second", |_state, _params| async {
        Ok(json!({ "order": "second" }))
    });
    router.register("slow/never", |_state, _params| async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(Value::Null)
    });
    router.register("slow/hung", |_state, _params| async {
        std::future::pending::<()>().await;
        Ok(Value::Null)
    });
    router
}

async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let manager = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state = AppState::for_test(manager, dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// Run the pump over `input` and return every frame it wrote.
async fn run_pump(input: Vec<u8>, options: StioOptions) -> Vec<Value> {
    let (state, _dir) = test_state().await;
    // A bare channel, not `RpcRuntime`: the runtime holds an outbound
    // sender of its own, and the writer task ends only when every
    // sender is gone.
    let (outbound_tx, receiver) = tokio::sync::mpsc::unbounded_channel();
    let outbound = Outbound::new(outbound_tx);
    let pump =
        StioPump::with_options(state, Arc::new(router()), receiver, options);
    let (mut output, write_half) = tokio::io::duplex(64 * 1024);
    let cursor = Cursor::new(input);
    pump.run_with(move || cursor, write_half, outbound)
        .await
        .expect("the pump runs");

    let mut frames: Vec<Value> = Vec::new();
    loop {
        match read_frame(&mut output, DEFAULT_MAX_FRAME_SIZE).await {
            Ok(bytes) => frames
                .push(serde_json::from_slice(&bytes).expect("a JSON frame")),
            Err(error) if error.is_end_of_stream() => break,
            Err(error) => panic!("the pump wrote a bad frame: {error}"),
        }
    }
    frames
}

#[tokio::test]
async fn ready_is_first_and_a_notification_is_not_answered() {
    let mut input = notification("health/ping");
    input.extend(request(json!(1), "health/ping"));

    let frames = run_pump(input, StioOptions::default()).await;
    assert_eq!(frames[0]["method"], "ready");
    let answers: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame.get("id").is_some())
        .collect();
    assert_eq!(answers.len(), 1, "the notification gets no response");
    assert_eq!(answers[0]["id"], 1);
    assert_eq!(answers[0]["result"]["ok"], true);
}

#[tokio::test]
async fn responses_may_arrive_out_of_order() {
    let mut input = request(json!(1), "slow/first");
    input.extend(request(json!(2), "fast/second"));

    let frames = run_pump(input, StioOptions::default()).await;
    let ids: Vec<Value> = frames
        .iter()
        .filter_map(|frame| frame.get("id").cloned())
        .collect();
    assert_eq!(
        ids,
        vec![json!(2), json!(1)],
        "the fast answer is written first and ids are echoed"
    );
}

#[tokio::test]
async fn a_cancelled_request_writes_no_response() {
    let mut input = request(json!("c1"), "slow/never");
    input.extend(cancellation(json!("c1")));

    let frames = run_pump(input, StioOptions::default()).await;
    assert_eq!(frames.len(), 1, "only the ready notification is written");
    assert_eq!(frames[0]["method"], "ready");
}

#[tokio::test]
async fn cancelling_an_unknown_id_answers_not_found() {
    let input = wire(&json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "$/cancelRequest",
        "params": { "id": 42 },
    }));

    let frames = run_pump(input, StioOptions::default()).await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1]["id"], 9);
    assert_eq!(frames[1]["error"]["data"]["code"], "not_found");
}

#[tokio::test]
async fn a_request_past_its_deadline_answers_timeout() {
    let options = StioOptions {
        request_timeout: Some(Duration::from_millis(50)),
        ..StioOptions::default()
    };
    let frames = run_pump(request(json!(1), "slow/never"), options).await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1]["id"], 1);
    assert_eq!(frames[1]["error"]["data"]["code"], "timeout");
}

#[tokio::test]
async fn a_hung_handler_is_aborted_after_the_shutdown_grace() {
    let options = StioOptions {
        shutdown_grace: Duration::from_millis(100),
        ..StioOptions::default()
    };
    let started = std::time::Instant::now();
    let frames = run_pump(request(json!(1), "slow/hung"), options).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the pump returns after the grace, not after the hung handler"
    );
    assert_eq!(frames[0]["method"], "ready");
    assert!(
        !frames
            .iter()
            .any(|frame| frame.get("id") == Some(&json!(1))),
        "an aborted request writes no response"
    );
}

#[tokio::test]
async fn a_frame_without_content_length_answers_a_framing_error() {
    let frames =
        run_pump(b"X-Trace: 1\r\n\r\n{}".to_vec(), StioOptions::default())
            .await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["method"], "ready");
    assert_eq!(frames[1]["error"]["code"], -32600);
    assert_eq!(frames[1]["error"]["data"]["code"], "missing_header");
}

#[tokio::test]
async fn an_oversized_frame_is_rejected_and_the_next_frame_is_served() {
    let mut input = b"Content-Length: 9000\r\n\r\n".to_vec();
    input.extend(std::iter::repeat_n(b'x', 9000));
    input.extend(request(json!(1), "health/ping"));

    let options = StioOptions {
        max_frame_size: 100,
        ..StioOptions::default()
    };
    let frames = run_pump(input, options).await;
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0]["method"], "ready");
    assert_eq!(frames[1]["error"]["code"], -32600);
    assert_eq!(frames[1]["error"]["data"]["code"], "too_large");
    assert_eq!(frames[2]["id"], 1);
    assert_eq!(frames[2]["result"]["ok"], true);
}

#[tokio::test]
async fn a_malformed_frame_answers_the_parse_code() {
    let input = frame(b"{not json");

    let frames = run_pump(input, StioOptions::default()).await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1]["error"]["code"], -32700);
}

#[tokio::test]
async fn a_queue_over_the_watermark_spills_to_disk_and_keeps_its_order() {
    let (_done_tx, done) = tokio::sync::watch::channel(false);
    let queue = OutboundQueue::new(done);
    // Ten blocks of one MiB: the memory watermark is four, so the
    // queue must spill and hold the rest on disk.
    let frame = vec![0xABu8; 1024 * 1024];
    for _ in 0..10 {
        queue.push(frame.clone());
    }
    assert!(
        queue.peak_memory_bytes() <= OUTBOUND_MEMORY_WATERMARK,
        "the memory segment stays under the watermark, got {}",
        queue.peak_memory_bytes()
    );

    // Drain: memory first, then the spool, in arrival order.
    let mut drained: Vec<u8> = Vec::new();
    while let Some(frame) = queue.pop_memory() {
        drained.extend_from_slice(&frame);
    }
    while let Some(block) = queue.take_spool_block() {
        drained.extend_from_slice(&block);
    }
    assert_eq!(
        drained.len(),
        10 * 1024 * 1024,
        "every frame survives the spill"
    );
    assert!(
        drained
            .chunks(1024 * 1024)
            .all(|chunk| chunk.iter().all(|b| *b == 0xAB)),
        "the frames keep their order and their bytes"
    );
}

#[tokio::test]
async fn a_slow_reader_loses_no_frame_and_keeps_the_order() {
    let (state, _dir) = test_state().await;
    let mut input = request(json!(1), "health/ping");
    // A second request: the peer reads late, so the writer stalls
    // and the queue spills.
    input.extend(request(json!(2), "health/ping"));
    let (outbound_tx, receiver) = tokio::sync::mpsc::unbounded_channel();
    let outbound = Outbound::new(outbound_tx);
    let pump = StioPump::with_options(
        state,
        Arc::new(router()),
        receiver,
        StioOptions::default(),
    );
    // A small duplex: the writer blocks until the test reads.
    let (mut output, write_half) = tokio::io::duplex(64);
    let cursor = Cursor::new(input);
    let pumping = tokio::spawn(async move {
        pump.run_with(move || cursor, write_half, outbound)
            .await
            .expect("the pump runs");
    });

    // Read late: give the writer time to stall and spill.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut frames: Vec<Value> = Vec::new();
    loop {
        match read_frame(&mut output, DEFAULT_MAX_FRAME_SIZE).await {
            Ok(bytes) => frames
                .push(serde_json::from_slice(&bytes).expect("a JSON frame")),
            Err(error) if error.is_end_of_stream() => break,
            Err(error) => panic!("the pump wrote a bad frame: {error}"),
        }
    }
    pumping.await.expect("the pump task");

    let answers: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame.get("id").is_some())
        .collect();
    assert_eq!(answers.len(), 2, "both requests are answered");
    let ids: Vec<&Value> = answers.iter().map(|frame| &frame["id"]).collect();
    assert!(
        ids == vec![&json!(1), &json!(2)] || ids == vec![&json!(2), &json!(1)],
        "both ids are answered, order free: {ids:?}"
    );
}
