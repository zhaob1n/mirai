// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Length-prefixed postcard framing, optionally zstd-compressed.
//!
//! ```text
//! [ len: u32 LE ][ flags: u8 ][ payload: len bytes ]
//! flags & 0x01 : payload is a zstd frame whose plaintext is the postcard message
//! ```
//!
//! Generic over `AsyncRead + AsyncWrite`, so the same codec serves the QUIC transport and
//! any future TCP fallback. Both directions reuse a per-connection [`FrameBuf`], so
//! steady-state framing does not allocate.

use std::io::{self, Write};

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Hard ceiling on one frame's payload, in either direction.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Postcard payloads above this are zstd-compressed.
pub const COMPRESS_THRESHOLD: usize = 4096;

const FLAG_ZSTD: u8 = 0x01;
/// Every flag bit this version understands. Anything else is a hard error.
const FLAG_KNOWN: u8 = FLAG_ZSTD;
const ZSTD_LEVEL: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME} byte limit")]
    TooLarge(u64),
    #[error("malformed frame: {0}")]
    Codec(String),
    #[error("peer closed the stream")]
    Eof,
}

/// Scratch buffers reused across every frame on one connection.
#[derive(Default)]
pub struct FrameBuf {
    plain: Vec<u8>,
    wire: Vec<u8>,
}

impl FrameBuf {
    pub fn new() -> FrameBuf {
        FrameBuf::default()
    }

    /// The most recently [`encode`]d frame, header included.
    pub fn frame(&self) -> &[u8] {
        &self.wire
    }
}

/// A `Write` sink that refuses to grow a `Vec` past `MAX_FRAME` — decompression-bomb guard.
struct Bounded<'a>(&'a mut Vec<u8>);

impl Write for Bounded<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.0.len() + buf.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decompressed frame exceeds the frame limit",
            ));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encodes `msg` into `buf.wire` as a complete frame and returns it.
pub fn encode<'b, T: Serialize + ?Sized>(
    buf: &'b mut FrameBuf,
    msg: &T,
) -> Result<&'b [u8], FrameError> {
    buf.plain.clear();
    postcard::to_io(msg, &mut buf.plain).map_err(|e| FrameError::Codec(e.to_string()))?;

    buf.wire.clear();
    buf.wire.extend_from_slice(&[0; 5]);
    let flags = if buf.plain.len() > COMPRESS_THRESHOLD {
        zstd::stream::copy_encode(&buf.plain[..], Bounded(&mut buf.wire), ZSTD_LEVEL)?;
        FLAG_ZSTD
    } else {
        buf.wire.extend_from_slice(&buf.plain);
        0
    };

    let len = buf.wire.len() - 5;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len as u64));
    }
    buf.wire[..4].copy_from_slice(&(len as u32).to_le_bytes());
    buf.wire[4] = flags;
    Ok(&buf.wire)
}

/// Decodes one complete frame (header included) into `T`.
pub fn decode<T: DeserializeOwned>(buf: &mut FrameBuf, frame: &[u8]) -> Result<T, FrameError> {
    if frame.len() < 5 {
        return Err(FrameError::Eof);
    }
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    let flags = frame[4];
    if len > MAX_FRAME || frame.len() < 5 + len {
        return Err(FrameError::TooLarge(len as u64));
    }
    decode_payload(buf, flags, &frame[5..5 + len])
}

fn decode_payload<T: DeserializeOwned>(
    buf: &mut FrameBuf,
    flags: u8,
    payload: &[u8],
) -> Result<T, FrameError> {
    // Postcard is positional and the header has no other extension point, so an unknown
    // flag means the sender is speaking a dialect we would silently misparse. Reject it.
    if flags & !FLAG_KNOWN != 0 {
        return Err(FrameError::Codec(format!(
            "unknown frame flags {flags:#04x}"
        )));
    }
    let bytes: &[u8] = if flags & FLAG_ZSTD != 0 {
        buf.plain.clear();
        zstd::stream::copy_decode(payload, Bounded(&mut buf.plain))?;
        &buf.plain
    } else {
        payload
    };
    postcard::from_bytes(bytes).map_err(|e| FrameError::Codec(e.to_string()))
}

/// Writes one framed message.
pub async fn write_msg<W, T>(w: &mut W, buf: &mut FrameBuf, msg: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    encode(buf, msg)?;
    w.write_all(&buf.wire).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one framed message. Returns [`FrameError::Eof`] on a clean stream end.
pub async fn read_msg<R, T>(r: &mut R, buf: &mut FrameBuf) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0u8; 5];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Eof),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let flags = header[4];
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len as u64));
    }

    // Read into `wire`, then decompress (if needed) into `plain`.
    buf.wire.clear();
    buf.wire.resize(len, 0);
    r.read_exact(&mut buf.wire).await?;
    let payload = std::mem::take(&mut buf.wire);
    let out = decode_payload(buf, flags, &payload);
    buf.wire = payload;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_small_and_large_messages() {
        let small = vec![1u32, 2, 3];
        let large: Vec<u32> = (0..20_000).collect();

        let mut buf = FrameBuf::new();
        let mut sink: Vec<u8> = Vec::new();
        write_msg(&mut sink, &mut buf, &small).await.unwrap();
        let uncompressed_flag = sink[4];
        write_msg(&mut sink, &mut buf, &large).await.unwrap();

        // The small payload is stored raw, the large one is compressed.
        assert_eq!(uncompressed_flag, 0);

        let mut rd = &sink[..];
        let mut rbuf = FrameBuf::new();
        let a: Vec<u32> = read_msg(&mut rd, &mut rbuf).await.unwrap();
        let b: Vec<u32> = read_msg(&mut rd, &mut rbuf).await.unwrap();
        assert_eq!(a, small);
        assert_eq!(b, large);
        // And the second frame really was compressed.
        let hdr_off = 5 + postcard::to_stdvec(&small).unwrap().len();
        assert_eq!(sink[hdr_off + 4] & FLAG_ZSTD, FLAG_ZSTD);
    }

    #[tokio::test]
    async fn clean_eof_is_reported_as_eof() {
        let empty: &[u8] = &[];
        let mut rd = empty;
        let mut buf = FrameBuf::new();
        let r: Result<u32, _> = read_msg(&mut rd, &mut buf).await;
        assert!(matches!(r, Err(FrameError::Eof)));
    }

    #[tokio::test]
    async fn oversized_length_is_rejected_without_reading_the_body() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        frame.push(0);
        let mut rd = &frame[..];
        let mut buf = FrameBuf::new();
        let r: Result<u32, _> = read_msg(&mut rd, &mut buf).await;
        assert!(matches!(r, Err(FrameError::TooLarge(_))));
    }

    #[tokio::test]
    async fn unknown_flag_bits_are_rejected_not_ignored() {
        // A reserved bit set by a future dialect must fail loudly. Postcard is positional,
        // so parsing the payload anyway would produce plausible-looking wrong values.
        let payload = postcard::to_stdvec(&7u32).unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.push(0x02);
        frame.extend_from_slice(&payload);

        let mut buf = FrameBuf::new();
        let r: Result<u32, _> = read_msg(&mut &frame[..], &mut buf).await;
        assert!(matches!(r, Err(FrameError::Codec(_))), "got {r:?}");

        // The same bytes with no flags still decode, so the payload itself was fine.
        frame[4] = 0x00;
        let ok: u32 = read_msg(&mut &frame[..], &mut buf).await.unwrap();
        assert_eq!(ok, 7);
    }

    #[test]
    fn buffers_are_reused_across_frames() {
        let mut buf = FrameBuf::new();
        let big: Vec<u32> = (0..20_000).collect();
        encode(&mut buf, &big).unwrap();
        let cap = buf.wire.capacity();
        for _ in 0..8 {
            encode(&mut buf, &big).unwrap();
        }
        assert_eq!(buf.wire.capacity(), cap, "steady-state framing reallocated");
    }
}
