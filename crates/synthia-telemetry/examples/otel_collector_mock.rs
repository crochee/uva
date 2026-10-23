//! Minimal OTel HTTP collector for the V1 smoke test.
//!
//! Listens on `127.0.0.1:14318` and accepts POSTs to any path
//! (the OTel HTTP exporter ignores the path for our purposes:
//! a smoke test, no upstream collector that cares). Each batch
//! increments a counter, and after a single batch the listener
//! prints its summary and exits.
//!
//! The proof line is:
//!
//! ```text
//! OTLP-MOCK: 1 metrics batches delivered (12 345 bytes)
//! ```
//!
//! Usage:
//!
//! ```bash
//! cargo run --example otel_collector_mock
//! # then in another shell:
//! SYNTHIA_OTLP_ENDPOINT=http://127.0.0.1:4318 \
//!   cargo run --example otel_metrics_smoke -p synthia-telemetry
//! ```

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

const BIND: &str = "127.0.0.1:14318";

fn main() {
    let listener = TcpListener::bind(BIND).expect("bind 127.0.0.1:14318");
    let batches = Arc::new(AtomicU32::new(0));
    let bytes = Arc::new(AtomicU64::new(0));

    println!("OTLP-MOCK: listening on http://{BIND}/v1/metrics");
    let _ = listener.set_nonblocking(false);
    if let Ok((mut stream, _addr)) = listener.accept() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = vec![0u8; 16 * 1024];
        let mut total = 0;
        // Read until the headers terminate; `header_end` is
        // the offset of `\r\n\r\n` in the first `total` bytes.
        let header_end = loop {
            let n = stream.read(&mut buf[total..]).unwrap_or(0);
            if n == 0 {
                break total;
            }
            total += n;
            if let Some(pos) =
                buf[..total].windows(4).position(|w| w == b"\r\n\r\n")
            {
                break pos;
            }
            if total == buf.len() {
                eprintln!("OTLP-MOCK: malformed request");
                return;
            }
        };
        let header = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
        let content_length: usize = header
            .lines()
            .find_map(|l| {
                l.strip_prefix("Content-Length:")
                    .or_else(|| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        let already_have = total - header_end - 4;
        let remaining = content_length.saturating_sub(already_have);
        if remaining > 0 {
            let mut body = vec![0u8; remaining];
            let _ = stream.read_exact(&mut body);
        }
        batches.fetch_add(1, Ordering::SeqCst);
        bytes.fetch_add(content_length as u64, Ordering::SeqCst);

        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\n\
              Content-Length: 0\r\n\
              Content-Type: application/x-protobuf\r\n\
              Connection: close\r\n\r\n",
        );
        let _ = stream.flush();
    }
    let b = batches.load(Ordering::SeqCst);
    let n = bytes.load(Ordering::SeqCst);
    println!("OTLP-MOCK: {b} metrics batches delivered ({n} bytes)");
    // Give the upstream `Cargo run ... -p synthia-telemetry`
    // example a chance to print its summary before we exit.
    thread::sleep(Duration::from_millis(200));
}
