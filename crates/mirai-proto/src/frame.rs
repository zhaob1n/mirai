// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Length-prefixed postcard framing, optionally zstd-compressed.
//!
//! ```text
//! [ len: u32 LE ][ flags: u8 ][ payload: len bytes ]
//! flags 0x01 : payload is a zstd frame whose plaintext is the postcard message
//!              (control stream)
//! flags 0x02 : payload is the next flushed chunk of the zstd stream that runs through the
//!              whole subscription stream (subscription streams)
//! ```
//!
//! The control stream frames each message on its own ([`encode`], [`read_msg`]). A
//! subscription stream is one zstd stream from its first frame to its last
//! ([`SubStreamEncoder`], [`SubStreamDecoder`]), so each report is compressed against the
//! reports before it: consecutive reports of one search differ in a few numbers.
//!
//! Generic over `AsyncRead + AsyncWrite`, so the same codec serves the QUIC transport and
//! any future TCP fallback. Buffers are reused across frames, so steady-state framing does
//! not allocate.

use std::io;

use mirai_core::BoundedWriter;
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zstd::stream::raw::{CParameter, DParameter, Decoder, Encoder, InBuffer, Operation, OutBuffer};

/// Hard ceiling on one frame's payload, in either direction.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Postcard payloads above this are zstd-compressed.
pub const COMPRESS_THRESHOLD: usize = 4096;

const FLAG_ZSTD: u8 = 0x01;
/// Subscription streams only: the payload continues the stream's zstd stream.
const FLAG_SUB_ZSTD: u8 = 0x02;
/// Every flag bit the control stream understands. Anything else is a hard error.
const FLAG_KNOWN: u8 = FLAG_ZSTD;
const ZSTD_LEVEL: i32 = 1;

/// Compression level of a subscription stream. The sender's choice alone: a decoder
/// accepts any level.
pub const SUB_STREAM_LEVEL: i32 = 3;

/// A subscription stream's zstd window, as a power of two: 64 KiB, several times the
/// largest report, so the previous report is always in reach. It is also the decoder's
/// memory bound: a stream that declares a larger window is refused.
const SUB_WINDOW_LOG: u32 = 16;

/// Match-finder table sizes, as powers of two. Left to the level they would be sized for
/// megabyte inputs; 2^14 entries keep each table at 64 KiB per stream, ample for reports.
const SUB_TABLE_LOG: u32 = 14;

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

/// The decompression-bomb guard: nothing may inflate past [`MAX_FRAME`].
fn bounded(out: &mut Vec<u8>) -> BoundedWriter<'_> {
    BoundedWriter::new(out, MAX_FRAME, "frame")
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
        zstd::stream::copy_encode(&buf.plain[..], bounded(&mut buf.wire), ZSTD_LEVEL)?;
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
    let (flags, payload) = split_frame(frame)?;
    decode_payload(buf, flags, payload)
}

/// One complete frame's flags and payload.
fn split_frame(frame: &[u8]) -> Result<(u8, &[u8]), FrameError> {
    if frame.len() < 5 {
        return Err(FrameError::Eof);
    }
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if len > MAX_FRAME || frame.len() < 5 + len {
        return Err(FrameError::TooLarge(len as u64));
    }
    Ok((frame[4], &frame[5..5 + len]))
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
        zstd::stream::copy_decode(payload, bounded(&mut buf.plain))?;
        &buf.plain
    } else {
        payload
    };
    postcard_exact(bytes)
}

/// Decodes exactly one postcard message. Postcard is positional: bytes left over mean the
/// sender encoded another schema, and what was decoded before them is not what it meant.
fn postcard_exact<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, FrameError> {
    let (msg, rest) =
        postcard::take_from_bytes(bytes).map_err(|e| FrameError::Codec(e.to_string()))?;
    if !rest.is_empty() {
        return Err(FrameError::Codec(format!(
            "{} trailing bytes after the message",
            rest.len()
        )));
    }
    Ok(msg)
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
    let flags = read_frame(r, &mut buf.wire).await?;
    let payload = std::mem::take(&mut buf.wire);
    let out = decode_payload(buf, flags, &payload);
    buf.wire = payload;
    out
}

/// Reads one frame's payload into `wire` and returns its flags. The length is checked
/// before anything is allocated; a clean end of stream is [`FrameError::Eof`].
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, wire: &mut Vec<u8>) -> Result<u8, FrameError> {
    let mut header = [0u8; 5];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Eof),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len as u64));
    }
    wire.clear();
    wire.resize(len, 0);
    r.read_exact(wire).await?;
    Ok(header[4])
}

/// Makes room for at least `n` more bytes without shrinking what is already there.
fn spare(v: &mut Vec<u8>, n: usize) {
    if v.capacity() - v.len() < n {
        v.reserve(n);
    }
}

/// Writes one subscription stream.
///
/// The stream carries a single zstd stream, flushed at the end of every frame: each frame
/// decodes as soon as it arrives, and each is compressed against everything the stream
/// sent before it. One encoder per stream, never shared; frames go out in the order they
/// were encoded. After an error the stream is unusable.
pub struct SubStreamEncoder {
    zstd: Encoder<'static>,
    plain: Vec<u8>,
    wire: Vec<u8>,
}

impl SubStreamEncoder {
    pub fn new(level: i32) -> Result<SubStreamEncoder, FrameError> {
        let mut zstd = Encoder::new(level)?;
        for p in [
            CParameter::WindowLog(SUB_WINDOW_LOG),
            CParameter::HashLog(SUB_TABLE_LOG),
            CParameter::ChainLog(SUB_TABLE_LOG),
            CParameter::ChecksumFlag(false),
            CParameter::ContentSizeFlag(false),
        ] {
            zstd.set_parameter(p)?;
        }
        Ok(SubStreamEncoder {
            zstd,
            plain: Vec::new(),
            wire: Vec::new(),
        })
    }

    /// Encodes `msg` as the stream's next frame and returns the frame, header included.
    pub fn encode<T: Serialize + ?Sized>(&mut self, msg: &T) -> Result<&[u8], FrameError> {
        self.plain.clear();
        postcard::to_io(msg, &mut self.plain).map_err(|e| FrameError::Codec(e.to_string()))?;
        // Refused before zstd sees it: a message fed to the stream but never sent would
        // leave the receiver's copy of the stream behind.
        if self.plain.len() > MAX_FRAME {
            return Err(FrameError::TooLarge(self.plain.len() as u64));
        }

        self.wire.clear();
        self.wire.extend_from_slice(&[0; 5]);
        spare(
            &mut self.wire,
            zstd::zstd_safe::compress_bound(self.plain.len()) + 64,
        );
        let mut input = InBuffer::around(&self.plain);
        while input.pos() < self.plain.len() {
            spare(&mut self.wire, 4096);
            let pos = self.wire.len();
            let mut out = OutBuffer::around_pos(&mut self.wire, pos);
            self.zstd.run(&mut input, &mut out)?;
        }
        // End the block so the receiver can decode this frame without the next one.
        loop {
            spare(&mut self.wire, 4096);
            let pos = self.wire.len();
            let mut out = OutBuffer::around_pos(&mut self.wire, pos);
            if self.zstd.flush(&mut out)? == 0 {
                break;
            }
        }

        let len = self.wire.len() - 5;
        if len > MAX_FRAME {
            return Err(FrameError::TooLarge(len as u64));
        }
        self.wire[..4].copy_from_slice(&(len as u32).to_le_bytes());
        self.wire[4] = FLAG_SUB_ZSTD;
        Ok(&self.wire)
    }

    /// Writes `msg` as the stream's next frame.
    pub async fn write<W, T>(&mut self, w: &mut W, msg: &T) -> Result<(), FrameError>
    where
        W: AsyncWrite + Unpin,
        T: Serialize + ?Sized,
    {
        self.encode(msg)?;
        w.write_all(&self.wire).await?;
        w.flush().await?;
        Ok(())
    }
}

/// Reads one subscription stream: the other end of a [`SubStreamEncoder`].
///
/// Every frame has to be decoded, in order, even one whose report is then thrown away:
/// each continues the stream's zstd stream.
pub struct SubStreamDecoder {
    zstd: Decoder<'static>,
    wire: Vec<u8>,
    plain: Vec<u8>,
}

impl SubStreamDecoder {
    pub fn new() -> Result<SubStreamDecoder, FrameError> {
        let mut zstd = Decoder::new()?;
        zstd.set_parameter(DParameter::WindowLogMax(SUB_WINDOW_LOG))?;
        Ok(SubStreamDecoder {
            zstd,
            wire: Vec::new(),
            plain: Vec::new(),
        })
    }

    /// Decodes the stream's next frame (header included).
    pub fn decode<T: DeserializeOwned>(&mut self, frame: &[u8]) -> Result<T, FrameError> {
        let (flags, payload) = split_frame(frame)?;
        self.decode_payload(flags, payload)
    }

    /// Reads and decodes the stream's next frame. Returns [`FrameError::Eof`] on a clean
    /// stream end.
    pub async fn read<R, T>(&mut self, r: &mut R) -> Result<T, FrameError>
    where
        R: AsyncRead + Unpin,
        T: DeserializeOwned,
    {
        let flags = read_frame(r, &mut self.wire).await?;
        let payload = std::mem::take(&mut self.wire);
        let out = self.decode_payload(flags, &payload);
        self.wire = payload;
        out
    }

    fn decode_payload<T: DeserializeOwned>(
        &mut self,
        flags: u8,
        payload: &[u8],
    ) -> Result<T, FrameError> {
        match flags {
            // Not compressed, and outside the zstd stream: the stream carries on unchanged.
            0 => postcard_exact(payload),
            FLAG_SUB_ZSTD => {
                self.inflate(payload)?;
                postcard_exact(&self.plain)
            }
            _ => Err(FrameError::Codec(format!(
                "unknown frame flags {flags:#04x}"
            ))),
        }
    }

    /// Decompresses one flushed chunk into `plain`. Nothing may inflate past
    /// [`MAX_FRAME`]: the decompression-bomb guard.
    fn inflate(&mut self, payload: &[u8]) -> Result<(), FrameError> {
        self.plain.clear();
        let mut input = InBuffer::around(payload);
        loop {
            spare(&mut self.plain, 16 * 1024);
            let pos = self.plain.len();
            let mut out = OutBuffer::around_pos(&mut self.plain, pos);
            self.zstd.run(&mut input, &mut out)?;
            if self.plain.len() > MAX_FRAME {
                return Err(FrameError::TooLarge(self.plain.len() as u64));
            }
            // A call that leaves room in the output has emitted all it can.
            if input.pos() == payload.len() && self.plain.len() < self.plain.capacity() {
                return Ok(());
            }
        }
    }
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

    /// Postcard is positional: bytes left over after a message mean the sender encoded a
    /// different schema, and whatever was decoded before them is not what it meant.
    #[test]
    fn trailing_bytes_after_a_message_are_rejected() {
        let mut payload = postcard::to_stdvec(&7u32).unwrap();
        payload.push(0);
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.push(0);
        frame.extend_from_slice(&payload);

        let r: Result<u32, _> = decode(&mut FrameBuf::new(), &frame);
        assert!(matches!(r, Err(FrameError::Codec(_))), "got {r:?}");
    }

    /// Every frame of a subscription stream must decode as it arrives: a sender that did
    /// not flush would hold the tail of each report in its compressor until the next one.
    #[test]
    fn sub_stream_frames_decode_one_at_a_time_in_order() {
        let mut enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).unwrap();
        let mut dec = SubStreamDecoder::new().unwrap();
        for n in [10u32, 3000, 7] {
            let msg: Vec<u32> = (0..n).map(|i| i * n).collect();
            let frame = enc.encode(&msg).unwrap().to_vec();
            let back: Vec<u32> = dec.decode(&frame).unwrap();
            assert_eq!(back, msg);
        }
    }

    /// The point of one zstd stream per subscription: a report that repeats what the stream
    /// already carried costs a back-reference, not its size again.
    #[test]
    fn a_repeated_message_costs_almost_nothing_the_second_time() {
        let msg: Vec<u32> = (0..2000u32).map(|i| i * i % 9973).collect();
        let mut enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).unwrap();
        let mut dec = SubStreamDecoder::new().unwrap();
        let first = enc.encode(&msg).unwrap().to_vec();
        let second = enc.encode(&msg).unwrap().to_vec();
        assert!(first.len() > 1000, "first frame {} bytes", first.len());
        assert!(second.len() <= 64, "second frame {} bytes", second.len());
        for frame in [first, second] {
            assert_eq!(dec.decode::<Vec<u32>>(&frame).unwrap(), msg);
        }
    }

    #[test]
    fn sub_stream_flags_are_checked() {
        let mut enc = SubStreamEncoder::new(SUB_STREAM_LEVEL).unwrap();
        let mut dec = SubStreamDecoder::new().unwrap();
        let chunk = enc.encode(&7u32).unwrap().to_vec();

        for flags in [FLAG_ZSTD, 0x04] {
            let mut bad = chunk.clone();
            bad[4] = flags;
            let r: Result<u32, _> = SubStreamDecoder::new().unwrap().decode(&bad);
            assert!(
                matches!(r, Err(FrameError::Codec(_))),
                "flags {flags:#04x}: {r:?}"
            );
        }
        let r: Result<u32, _> = decode(&mut FrameBuf::new(), &chunk);
        assert!(
            matches!(r, Err(FrameError::Codec(_))),
            "the control stream took {r:?}"
        );

        // An uncompressed frame is legal between chunks and leaves the zstd stream alone.
        assert_eq!(dec.decode::<u32>(&chunk).unwrap(), 7);
        let payload = postcard::to_stdvec(&8u32).unwrap();
        let mut raw = (payload.len() as u32).to_le_bytes().to_vec();
        raw.push(0);
        raw.extend_from_slice(&payload);
        assert_eq!(dec.decode::<u32>(&raw).unwrap(), 8);
        let next = enc.encode(&9u32).unwrap().to_vec();
        assert_eq!(dec.decode::<u32>(&next).unwrap(), 9);
    }

    /// A whole zstd frame of `plain` with the given window, sent as a subscription-stream
    /// chunk: what a sender outside the rules could put on the wire.
    fn foreign_sub_frame(plain: &[u8], window_log: u32) -> Vec<u8> {
        let mut z = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
        z.window_log(window_log).unwrap();
        io::Write::write_all(&mut z, plain).unwrap();
        let body = z.finish().unwrap();
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.push(FLAG_SUB_ZSTD);
        frame.extend_from_slice(&body);
        frame
    }

    #[test]
    fn a_sub_stream_chunk_cannot_inflate_past_max_frame() {
        let bomb = foreign_sub_frame(&vec![0u8; MAX_FRAME + 1], SUB_WINDOW_LOG);
        assert!(
            bomb.len() < 4096,
            "not much of a bomb: {} bytes",
            bomb.len()
        );
        let r: Result<u32, _> = SubStreamDecoder::new().unwrap().decode(&bomb);
        assert!(matches!(r, Err(FrameError::TooLarge(_))), "got {r:?}");
    }

    /// The window is the decoder's memory: a stream may not make a client hold more.
    #[test]
    fn a_sub_stream_window_over_the_limit_is_refused() {
        let payload = postcard::to_stdvec(&7u32).unwrap();
        let frame = foreign_sub_frame(&payload, SUB_WINDOW_LOG + 4);
        let r: Result<u32, _> = SubStreamDecoder::new().unwrap().decode(&frame);
        assert!(r.is_err(), "a 2^20 window was accepted: {r:?}");
        // The same bytes at the permitted window decode.
        let ok = foreign_sub_frame(&payload, SUB_WINDOW_LOG);
        assert_eq!(
            SubStreamDecoder::new().unwrap().decode::<u32>(&ok).unwrap(),
            7
        );
    }
}
