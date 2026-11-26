//! Content-Length framing for the headless core's stdio transport.
//!
//! One frame is an HTTP-style header block followed by exactly the
//! declared number of payload bytes:
//!
//! ```text
//! Content-Length: 27\r\n
//! \r\n
//! {"jsonrpc":"2.0","id":1,…}
//! ```
//!
//! The header name is matched case-insensitively and other header
//! lines are tolerated. The payload is read to its declared length —
//! never to a newline — so a JSON string that itself contains a
//! newline, two frames coalesced into one read, and a frame split
//! across several writes all decode correctly.
//!
//! # Why a local codec
//!
//! Task 0 evaluated `rpc-io 0.4` and found it complete, but its
//! endpoint is single-owner (`&mut` for every call) and its ids are
//! its own. The core already owns an id-carrying frame and needs
//! only the byte layer, so the codec lives here, behind the same
//! seam (`read_frame` / `write_frame`), and can be swapped for the
//! crate later without touching the pump.
//!
//! # Bounds
//!
//! [`DEFAULT_MAX_FRAME_SIZE`] is 64 MiB. A header that declares a
//! larger body is rejected from the header alone — the payload is
//! never allocated — and the header block itself is capped so a
//! peer that never sends `\r\n\r\n` cannot grow it without bound.
use std::io::{self, BufRead, ErrorKind, Read, Write};

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest frame body the core accepts by default.
///
/// 64 MiB, omp's negotiated logical ceiling (`rpc-frame.ts:9`
/// `MAX_RPC_REASSEMBLED_BYTES`) — Content-Length framing has no
/// physical frame limit to chunk around, so the logical ceiling is
/// the only knob a peer can hit, and omp's is the ceiling an
/// industry client expects to be able to send.
pub const DEFAULT_MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// The largest header block a frame may carry. Nothing the protocol
/// needs comes close: one `Content-Length` line is a few dozen bytes.
const MAX_HEADER_SIZE: usize = 8 * 1024;

/// The header line's fixed prefix, minus the number.
const HEADER_PREFIX: &[u8] = b"Content-Length: ";

/// The longest decimal `usize` is 20 digits (64-bit).
const MAX_DECIMAL_DIGITS: usize = 20;

/// A framing failure, classified so the caller can decide whether
/// the stream is still aligned.
///
/// [`FrameError::kind`] is the stable spelling (`missing_header`,
/// `too_large`, …) a log line or a test can match without depending
/// on the variant's payload.
#[derive(Debug, Error)]
pub enum FrameError {
    /// The underlying reader or writer failed.
    #[error("frame i/o failed: {0}")]
    Io(#[from] io::Error),
    /// The header block carried no `Content-Length`.
    ///
    /// There is no boundary to guess, so the stream cannot be
    /// resynchronised by this codec.
    #[error("frame header has no Content-Length")]
    MissingHeader,
    /// `Content-Length` was not a non-negative decimal integer.
    #[error("invalid Content-Length: {value:?}")]
    InvalidContentLength {
        /// The offending value, as text.
        value: String,
    },
    /// The declared length is above the caller's ceiling.
    ///
    /// Returned before the body is read, so the payload is never
    /// allocated; the bytes remain on the stream for the caller to
    /// drain or discard with the connection.
    #[error("frame of {length} bytes exceeds the {max}-byte maximum")]
    TooLarge {
        /// The length the peer declared.
        length: usize,
        /// The caller's ceiling.
        max: usize,
    },
    /// The stream ended before the declared payload was complete.
    #[error("stream ended after {actual} of {expected} declared frame bytes")]
    Truncated {
        /// The length the peer declared.
        expected: usize,
        /// How many payload bytes arrived before EOF.
        actual: usize,
    },
    /// The header block was malformed (duplicate length, bad line,
    /// oversized block, or a line not terminated by CRLF).
    #[error("malformed frame header: {reason}")]
    BadHeader {
        /// What was wrong, for the log line.
        reason: String,
    },
}

impl FrameError {
    /// The stable machine kind of this failure.
    ///
    /// These spellings are part of the transport's contract; a test
    /// or a log filter may match them without matching the variant.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::MissingHeader => "missing_header",
            Self::InvalidContentLength { .. } => "invalid_content_length",
            Self::TooLarge { .. } => "too_large",
            Self::Truncated { .. } => "truncated",
            Self::BadHeader { .. } => "bad_header",
        }
    }

    /// Whether this failure is the peer closing the stream at a
    /// frame boundary.
    ///
    /// The pump treats this as a normal end, not as a protocol
    /// error.
    #[must_use]
    pub fn is_end_of_stream(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == ErrorKind::UnexpectedEof)
    }
}

/// Read one Content-Length frame from an async reader.
///
/// `max` is the largest accepted payload; a header that declares a
/// bigger body fails with [`FrameError::TooLarge`] before any
/// allocation. The reader is consumed exactly to the frame boundary,
/// so a second call sees the next coalesced frame.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Vec<u8>, FrameError> {
    let header = read_header_async(reader).await?;
    let length = parse_content_length(&header)?;
    if length > max {
        return Err(FrameError::TooLarge { length, max });
    }
    read_payload_async(reader, length).await
}

/// Read one Content-Length frame from a blocking reader.
///
/// This is the variant the stdio thread uses: the process stdin is
/// deliberately read on a plain OS thread rather than through
/// `tokio::io::stdin()` (see [`super::stdio`]), and the `BufRead`
/// seam keeps the payload bytes buffered between calls.
pub fn read_frame_blocking<R: BufRead>(
    reader: &mut R,
    max: usize,
) -> Result<Vec<u8>, FrameError> {
    let header = read_header_blocking(reader)?;
    let length = parse_content_length(&header)?;
    if length > max {
        return Err(FrameError::TooLarge { length, max });
    }
    read_payload_blocking(reader, length)
}

/// Write one Content-Length frame and flush it.
///
/// The header and payload are separate writes on a writer the caller
/// already serialises; the declared length is the payload's **byte**
/// length, so a non-ASCII JSON document needs no escaping to be
/// framed correctly.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> io::Result<()> {
    let mut header = [0u8; HEADER_PREFIX.len() + MAX_DECIMAL_DIGITS + 4];
    let mut cursor = HEADER_PREFIX.len();
    header[..cursor].copy_from_slice(HEADER_PREFIX);
    cursor += decimal_digits(payload.len(), &mut header[cursor..]);
    header[cursor..cursor + 4].copy_from_slice(b"\r\n\r\n");
    let cursor = cursor + 4;
    writer.write_all(&header[..cursor]).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

/// Render one Content-Length frame into bytes.
///
/// The synchronous twin of [`write_frame`]: the pump encodes a
/// payload into the frame bytes **before** it hands them to the
/// writer, so the outbound queue holds complete frames and the
/// writer only copies bytes.
///
/// # Errors
///
/// None today: the header fits its fixed buffer by construction (the
/// declared length's digit count is bounded).
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut header = [0u8; HEADER_PREFIX.len() + MAX_DECIMAL_DIGITS + 4];
    let mut cursor = HEADER_PREFIX.len();
    header[..cursor].copy_from_slice(HEADER_PREFIX);
    cursor += decimal_digits(payload.len(), &mut header[cursor..]);
    header[cursor..cursor + 4].copy_from_slice(b"\r\n\r\n");
    let cursor = cursor + 4;
    let mut frame = Vec::with_capacity(cursor + payload.len());
    frame.extend_from_slice(&header[..cursor]);
    frame.extend_from_slice(payload);
    frame
}

/// Write one Content-Length frame to a blocking writer and flush it.
///
/// The synchronous counterpart of [`write_frame`]: the stdio test
/// harness (and any plain-stdio peer) writes the same bytes the async
/// pump does. The declared length is the payload's **byte** length.
///
/// # Errors
///
/// Propagates the writer's [`io::Error`].
pub fn write_frame_blocking<W: Write>(
    writer: &mut W,
    payload: &[u8],
) -> io::Result<()> {
    let mut header = [0u8; HEADER_PREFIX.len() + MAX_DECIMAL_DIGITS + 4];
    let mut cursor = HEADER_PREFIX.len();
    header[..cursor].copy_from_slice(HEADER_PREFIX);
    cursor += decimal_digits(payload.len(), &mut header[cursor..]);
    header[cursor..cursor + 4].copy_from_slice(b"\r\n\r\n");
    let cursor = cursor + 4;
    writer.write_all(&header[..cursor])?;
    writer.write_all(payload)?;
    writer.flush()
}

/// Render `value` as decimal ASCII into `out`, returning the digit
/// count.
fn decimal_digits(value: usize, out: &mut [u8]) -> usize {
    let mut reversed = [0u8; MAX_DECIMAL_DIGITS];
    let mut digits = 0;
    let mut remaining = value;
    loop {
        reversed[digits] = b'0' + (remaining % 10) as u8;
        digits += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for (index, byte) in reversed[..digits].iter().rev().enumerate() {
        out[index] = *byte;
    }
    digits
}

fn end_of_stream(context: &str) -> FrameError {
    FrameError::Io(io::Error::new(
        ErrorKind::UnexpectedEof,
        context.to_string(),
    ))
}

fn header_too_large() -> FrameError {
    FrameError::BadHeader {
        reason: format!("frame header exceeds {MAX_HEADER_SIZE} bytes"),
    }
}

/// Parse the header block's `Content-Length` member.
fn parse_content_length(header: &[u8]) -> Result<usize, FrameError> {
    let mut length: Option<usize> = None;
    for line in header.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            return Err(FrameError::BadHeader {
                reason: format!(
                    "header line has no `:`: {:?}",
                    String::from_utf8_lossy(line)
                ),
            });
        };
        let (name, value) = line.split_at(colon);
        let name = trim_ascii(name);
        if name.is_empty() {
            return Err(FrameError::BadHeader {
                reason: "header line has an empty name".to_string(),
            });
        }
        if !name.eq_ignore_ascii_case(b"content-length") {
            continue;
        }
        if length.is_some() {
            return Err(FrameError::BadHeader {
                reason: "duplicate Content-Length header".to_string(),
            });
        }
        let value = trim_ascii(&value[1..]);
        if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
            return Err(FrameError::InvalidContentLength {
                value: String::from_utf8_lossy(value).into_owned(),
            });
        }
        let text = std::str::from_utf8(value).expect("ASCII digits are UTF-8");
        length = Some(text.parse().map_err(|_| {
            FrameError::InvalidContentLength {
                value: text.to_string(),
            }
        })?);
    }
    length.ok_or(FrameError::MissingHeader)
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = bytes {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

/// Read the header block, one CRLF line at a time, from a buffered
/// reader. The reader's buffer keeps any payload bytes that arrived
/// with the header, so a coalesced frame is not lost.
fn read_header_blocking<R: BufRead>(
    reader: &mut R,
) -> Result<Vec<u8>, FrameError> {
    let mut header = Vec::with_capacity(64);
    let mut line = Vec::new();
    loop {
        let budget = MAX_HEADER_SIZE - header.len();
        if budget == 0 {
            return Err(header_too_large());
        }
        line.clear();
        let read = reader
            .by_ref()
            .take(budget as u64)
            .read_until(b'\n', &mut line)
            .map_err(FrameError::Io)?;
        if read == 0 {
            return Err(end_of_stream("stream ended inside a frame header"));
        }
        if !line.ends_with(b"\r\n") {
            return Err(FrameError::BadHeader {
                reason: "header lines must end with CRLF".to_string(),
            });
        }
        header.extend_from_slice(&line);
        if line == b"\r\n" {
            return Ok(header);
        }
    }
}

/// Read the header block from an unbuffered async reader.
///
/// Bytes are consumed one at a time on purpose: the function must not
/// read past `\r\n\r\n` or it would eat the first payload bytes of a
/// coalesced frame and lose them.
async fn read_header_async<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, FrameError> {
    let mut header = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await {
            Ok(0) => {
                return Err(end_of_stream(
                    "stream ended inside a frame header",
                ));
            }
            Ok(_) => header.push(byte[0]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameError::Io(error)),
        }
        if header.ends_with(b"\r\n\r\n") {
            return Ok(header);
        }
        reject_malformed_line(&header, byte[0])?;
        if header.len() >= MAX_HEADER_SIZE {
            return Err(header_too_large());
        }
    }
}

/// Reject a header block that cannot be a sequence of CRLF lines.
fn reject_malformed_line(header: &[u8], last: u8) -> Result<(), FrameError> {
    let previous = header
        .len()
        .checked_sub(2)
        .and_then(|index| header.get(index))
        .copied();
    let reason = if previous == Some(b'\r') && last != b'\n' {
        Some("a `\\r` in a frame header must be followed by `\\n`")
    } else if last == b'\n' && previous != Some(b'\r') {
        Some("frame header lines must end with CRLF, not a bare LF")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(FrameError::BadHeader {
            reason: reason.to_string(),
        }),
        None => Ok(()),
    }
}

async fn read_payload_async<R: AsyncRead + Unpin>(
    reader: &mut R,
    length: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut payload = vec![0u8; length];
    let mut filled = 0;
    while filled < length {
        match reader.read(&mut payload[filled..]).await {
            Ok(0) => {
                return Err(FrameError::Truncated {
                    expected: length,
                    actual: filled,
                });
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameError::Io(error)),
        }
    }
    Ok(payload)
}

fn read_payload_blocking<R: BufRead>(
    reader: &mut R,
    length: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut payload = vec![0u8; length];
    let mut filled = 0;
    while filled < length {
        match reader.read(&mut payload[filled..]) {
            Ok(0) => {
                return Err(FrameError::Truncated {
                    expected: length,
                    actual: filled,
                });
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameError::Io(error)),
        }
    }
    Ok(payload)
}
