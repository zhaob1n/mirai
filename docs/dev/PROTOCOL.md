<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Huang Zhaobin -->

# MRP — the mirai Remote Protocol, version 0.1.0

Normative specification. Sufficient to implement an interoperable client or server without
reading the Rust. Reference implementations: `crates/mirai-server/src/session.rs` (server) and
`crates/mirai-engine/src/remote.rs` (client); wire types in `crates/mirai-proto`.

Related: [architecture](ARCHITECTURE.md) · [testing](TESTING.md) ·
[history and rationale](../archive/RETROSPECTIVE.md) · [user guide](../user/GUIDE.md) ·
[contributor entry point](../../AGENTS.md).

RFC 2119 keywords (MUST, MUST NOT, SHOULD, MAY, …) are used with their RFC 2119 meanings.
"Client" is the endpoint that initiates the QUIC connection; "server" accepts it. Roles are
fixed for the life of a connection.

| § | Topic |
|---|---|
| [1](#1-status-and-scope) | Status and scope |
| [2](#2-transport) | Transport: QUIC, TLS, certificate pinning |
| [3](#3-stream-topology) | Stream topology |
| [4](#4-framing) | Framing |
| [5](#5-payload-encoding-postcard-v1) | Payload encoding (postcard v1) |
| [6](#6-message-catalogue) | Message catalogue |
| [7](#7-value-types-and-quantisation) | Value types and quantisation |
| [8](#8-session-state-machine) | Session state machine |
| [9](#9-errors-and-limits) | Errors and limits |
| [10](#10-version) | Version |
| [11](#11-reference-figures) | Reference figures |
| [A](#appendix-a-worked-exchange) | Worked exchange, byte for byte |

---

## 1. Status and scope

MRP carries Go position analysis between a client and a server running KataGo. Each
request contains the whole position (INV-4); results stream until completion or
cancellation. Wire values are quantised, not floating-point ([§7](#7-value-types-and-quantisation)).

### 1.1 URL syntax

```
mirai-url = [ "mirai://" ] host [ ":" port ] [ "/" ]
host      = reg-name | IPv4address | "[" IPv6address [ "%" zone ] "]"
port      = 1*DIGIT                                       ; default 9678, at most 65535
```

Parsing (`endpoint.rs` — `parse_url`), which a client MUST reproduce:

1. Trim whitespace; strip an optional `mirai://` prefix; strip one trailing `/`. Any
   other `/` is an error: there is no path.
2. Empty host is an error, including a missing host before `:` and an empty `[]`.
3. A leading `[` starts an IPv6 literal ending at the first `]`. The text inside must be
   an IPv6 address, optionally followed by `%` and a non-empty zone. An optional `:port`
   may follow; anything else after `]` is an error.
4. Otherwise split host and port at the **last** `:`. If the host side still contains
   `:`, reject the URL: IPv6 literals must be bracketed. A host containing `[`, `]` or
   `%` is an error.
5. A port is digits only, no sign, within `u16`; absent means 9678.

A link-local literal needs its zone, written as the interface name or index the resolver
takes (`[fe80::1%eth0]`, not RFC 6874's `%25`). The zone stays part of the host.

No path, query or userinfo. Credentials travel in `Hello.token`, never in the URL.

---

## 2. Transport

### 2.1 QUIC and TLS

| Requirement | Level | Value |
|---|---|---|
| Transport | MUST | QUIC v1 (RFC 9000) over UDP |
| TLS | MUST | 1.3 only; older versions neither offered nor accepted |
| ALPN | MUST | exactly `mirai`; anything else MUST fail the connection |
| Client certificates | MUST NOT | neither side requests nor sends them |
| 0-RTT | MUST NOT be relied on | not enabled by either reference endpoint |

QUIC transport parameters (`transport.rs` — `transport_config`), shared by both endpoints:

| Parameter | Reference value | Status |
|---|---|---|
| `keep_alive_interval` | 5 s | **Advisory.** Any interval, or none. ≤ ⅓ of the peer's idle timeout is RECOMMENDED. |
| `max_idle_timeout` | 30 s | **Advisory value, normative kind.** QUIC takes the minimum of the two advertised values; an implementation MUST tolerate any peer value. |
| `max_concurrent_uni_streams` | 256 | **Normative floor.** A client MUST advertise at least its intended concurrent subscription count and SHOULD leave spare capacity. |
| `stream_receive_window` | 16 KiB | **Recommended for clients.** A server writing to a subscription stream blocks once it is one window ahead of what the client has read, and only then can it replace a queued report with a newer one. quinn's 1.25 MB default lets hundreds of stale reports queue on a slow link, all delivered late and in order. |
| bidirectional streams | 1 per connection | A server MUST permit the control stream; a client MUST NOT open a second ([§3](#3-stream-topology)). |

Other flow-control windows, migration and datagrams are implementation choices outside MRP.

The reference client tries every address a hostname resolves to, starting attempts 250 ms
apart until one handshake succeeds. A silent address must not prevent reaching a server on
another address (for example, IPv4-only listening behind a dual-stack name). Each attempt
checks its own certificate; for a pinned connect, only a handshake with the accepted pin can
win. A mismatching address is never used or retried. If no address succeeds, the client
reports a fingerprint mismatch in preference to a handshake rejection, and a rejection in
preference to a timeout.

### 2.2 Certificates: trust on first use

MRP pins certificates instead of using PKI, so a LAN server needs no public DNS or CA.

| Rule | Level |
|---|---|
| The pin is the **lowercase hex SHA-256 of the leaf certificate's DER**, 64 characters, no separators. Only the leaf is hashed; intermediates are ignored. | MUST |
| Self-signed leaves are expected and normal. | — |
| First contact with no stored pin: perform a TLS handshake, record the leaf fingerprint, and close with application code 0 **without opening a stream or sending a frame**. Show that fingerprint to the user and persist it only after they accept it. | MUST |
| A client MUST NOT send `Hello` or a token before the user has accepted the leaf fingerprint. The token-bearing connection is a later, pinned handshake. | MUST NOT |
| Later connections: compare, and abort the TLS handshake on mismatch. A mismatch is a **hard failure** — no fallback check, no one-time override, no retry of that connection. | MUST |
| Do NOT validate against a trust store, do NOT validate hostname/SAN, do NOT reject on `notBefore`/`notAfter`. The fingerprint is the entire check. | MUST |
| Still verify the TLS 1.3 `CertificateVerify` signature against the leaf's public key. Pinning replaces chain validation, not proof of key possession. | MUST |
| Normalise a user-supplied pin — trim, lowercase, strip `:`, internal whitespace and an optional `SHA256 Fingerprint=` prefix — so a fingerprint pasted from `openssl x509 -fingerprint -sha256` works. After normalisation the client MUST reject anything other than 64 hex digits *before* the handshake; a malformed pin is not a fingerprint mismatch. | SHOULD / MUST |

**SNI.** A client connecting to an IP literal, with or without a zone, sends `localhost` as
SNI. Servers MUST NOT route or authorise on SNI, or reject a handshake because SNI
disagrees with the certificate.

**Provisioning.** A server MAY generate a self-signed certificate on first start and
reuse it. Rotation invalidates every client pin and requires user approval.

---

## 3. Stream topology

| # | Rule | Level |
|---|---|---|
| 1 | After the handshake the client opens exactly one bidirectional stream and sends `ClientMsg::Hello` as its first frame. | MUST |
| 2 | The control stream carries `ClientMsg` client→server and `ServerMsg` server→client for the whole connection. | MUST |
| 3 | The client opens no further bidirectional streams. | MUST NOT |
| 4 | For each accepted `Open` the server opens a unidirectional stream whose **first four bytes are the subscription id as a little-endian `u32`, outside any frame**; framed `SubMsg` values follow. | MUST |
| 5 | The client identifies a unidirectional stream solely by that preamble, and tolerates it arriving before, after or interleaved with `Opened`. | MUST |
| 6 | `SubMsg` never appears on the control stream; `ServerMsg` never appears on a subscription stream. | MUST NOT |
| 7 | A subscription stream ends with either a `Done`/`Failed` frame plus a clean FIN, or a QUIC `RESET_STREAM`. | MUST |

One stream per subscription lets `STOP_SENDING` and `RESET_STREAM` discard unread reports
without delaying another subscription on the same ordered byte stream.

---

## 4. Framing

Every message on every stream is one frame:

```
 0        1        2        3        4        5                 5+len
 +--------+--------+--------+--------+--------+-------------------+
 |          len : u32 little-endian |  flags |  payload (len B)   |
 +--------+--------+--------+--------+--------+-------------------+
```

| Field | Type | Meaning |
|---|---|---|
| `len` | `u32` little-endian | payload length, **excluding** the 5-byte header |
| `flags` | `u8` | how the payload is encoded, below. Bits 2–7 reserved, MUST be 0 |
| `payload` | `len` bytes | [§5](#5-payload-encoding-postcard-v1) |

| `flags` | Allowed on | Payload |
|---|---|---|
| `0x00` | every stream | the postcard encoding |
| `0x01` | the control stream | a standalone zstd frame (RFC 8878) whose plaintext is the postcard encoding |
| `0x02` | subscription streams | the next chunk of the stream's zstd stream ([§4.1](#41-subscription-streams-one-zstd-stream)), which decompresses to the postcard encoding |

Constants: `MAX_FRAME` = 8 MiB (8388608 bytes); `COMPRESS_THRESHOLD` = 4096;
`FLAG_ZSTD` = `0x01`; `FLAG_SUB_ZSTD` = `0x02`. The reference sender uses
zstd level 1 and window 2^19 on control streams; subscription windows are 2^16.

| # | Rule | Level |
|---|---|---|
| 1 | Never emit `len > MAX_FRAME` or a postcard plaintext larger than `MAX_FRAME`, even if it compresses below the wire limit. | MUST NOT |
| 2 | Read the 5-byte header first and reject `len > MAX_FRAME` **before** reading or allocating the body. | MUST |
| 3 | Bound each frame's decompressed size to `MAX_FRAME`; abort inflation as soon as it would exceed the bound. Do not trust the zstd content-size field. A receiver MAY use a smaller plaintext limit where appropriate ([§9.3](#93-limits)). A control-frame sender MUST declare a zstd window no larger than 2^19; the receiver MUST refuse a larger one, which would be allocated from its header before the plaintext bound applies. | MUST |
| 4 | Reject a `flags` value the stream does not allow. | MUST |
| 5 | A payload decodes to exactly one message; reject trailing bytes after it. | MUST |
| 6 | Control stream: compress iff the postcard payload is **strictly greater than** `COMPRESS_THRESHOLD`. Within the frame and window limits above, receivers MUST accept either form at any payload size, so a minimal implementation MAY always send `flags = 0x00`. | SHOULD / MUST |
| 7 | Control stream: the reference encoder streams at level 1 with **no content-size field and no checksum**; receivers MUST NOT require either. | MUST NOT |
| 8 | A stream ending exactly at a frame boundary is a graceful end, not an error. Ending inside a frame is an error. | MUST |
| 9 | Frames carry no message-type tag: the type follows from stream and direction. | — |

### 4.1 Subscription streams: one zstd stream

A subscription stream carries a single zstd stream that starts in its first `0x02` frame and
is never ended. The sender compresses each message into it and flushes (`ZSTD_e_flush`) at the
end of the frame, so every frame decodes as soon as it arrives, and every report is compressed
against the reports before it.

| # | Rule | Level |
|---|---|---|
| 1 | The receiver decodes every frame of the stream, in order, including one whose report it then drops: each `0x02` frame continues the zstd stream. | MUST |
| 2 | The sender flushes at the end of every frame. A receiver MUST NOT need the next frame to finish decoding this one. | MUST |
| 3 | The zstd window is at most 2^16 bytes. It bounds each stream's memory on the client, which MUST refuse a stream that declares a larger one. | MUST |
| 4 | A `0x00` frame stays outside the zstd stream, which it neither advances nor resets. Any frame MAY be sent that way; send subscription frames as `0x02` where possible. | MAY / SHOULD |
| 5 | Level, checksum and content-size flag are the sender's choice; the reference sender uses zstd level 6 (`SUB_STREAM_LEVEL`) with neither checksum nor content size. | — |

A stream's zstd state never crosses into another stream, so cancelling one — which throws its
unread bytes away ([§8.4](#84-cancellation-inv-3)) — cannot desynchronise any other.

### 4.2 Worked frame

`ClientMsg::Ping(7)` — variant 4, one `u64`:

```
payload : 04 07                     04 = variant 4 (Ping), 07 = u64 varint 7
frame   : 02 00 00 00 00 04 07      len = 2 (LE u32), flags = 0x00, payload
```

---

## 5. Payload encoding (postcard v1)

The payload is [postcard](https://postcard.jamesmunns.com) v1: non-self-describing and
schema-driven. **No field names, no field counts, no type tags, no padding.** The decoder
succeeds only because it knows the schema from this document, so field order is load-bearing:
every struct MUST be encoded in exactly the order given in [§6](#6-message-catalogue) and
[§7](#7-value-types-and-quantisation).

### 5.1 Encoding rules

| Construct | Encoding |
|---|---|
| `bool` | one byte, `0x00` / `0x01` |
| `u8` | one raw byte, **no varint** |
| `i8` | one raw byte, two's complement, **no varint, no zigzag** |
| `u16`, `u32`, `u64` | unsigned LEB128 varint |
| `i16` | **zigzag, then varint**: `zz = ((v << 1) ^ (v >> 15)) as u16`, then varint(`zz`) |
| `Option<T>` | `0x00` = none; `0x01` + value = some; reject any other tag |
| enum variant | varint of the **zero-based declaration-order index**, then its fields (nothing extra for a unit variant); reject unknown discriminants |
| `Vec<T>`, sequences | varint element **count**, then elements back to back |
| `String`, `&str` | varint **byte** length, then UTF-8 bytes; reject invalid UTF-8 |
| tuple, tuple struct, struct | fields in declaration order, concatenated; no length, no framing |
| newtype struct (`Point(u16)`) | transparent — the inner value's encoding |
| unit, unit struct | zero bytes |

Deliberately unused by MRP, therefore unspecified here: `f32`/`f64` (MRP quantises
instead), `char`, maps, 32/64/128-bit signed integers, `u128`, COBS framing, CRC flavours.

### 5.2 Varints

```
encode(n):                              decode():
  loop:                                   result = 0; shift = 0
    byte = n & 0x7F                       loop:
    n >>= 7                                 b = next_byte()
    if n != 0: emit(byte | 0x80)            result |= (b & 0x7F) << shift
    else:      emit(byte); stop             if (b & 0x80) == 0: stop
                                            shift += 7
```

* Seven bits per byte, **least-significant group first**; high bit set on all but the last byte.
* Maximum lengths: `u16` → 3 bytes, `u32` → 5, `u64` → 10.
* Encoders MUST emit the minimal byte count. Decoders MUST reject a varint exceeding
  its type's maximum length or remaining bits (e.g. a five-byte `u32` ending in
  `> 0x0F`).
* Decoders MAY accept a non-minimal encoding that still fits the budget — postcard's does, e.g.
  `80 00` reads as `u16` 0. Encoders MUST NOT depend on that.

**Length varints.** Sequence and string lengths use postcard's `usize` varint, whose decoder
width follows the decoding machine's pointer size. MRP removes the portability question: no
frame may exceed `MAX_FRAME`, so no valid length exceeds 8388608, which is at most four varint
bytes and decodes identically everywhere. Implementations MUST NOT emit a longer length.

### 5.3 Type-to-encoding map

The primitives above compose the wire types in [§6](#6-message-catalogue) and
[§7](#7-value-types-and-quantisation). `ProtoVersion` is three `u16` varints
(`major`, `minor`, `patch`); `Color` is an enum discriminant, `0` Black or `1`
White. `Point` is a transparent `u16` varint; `Size` is two raw `u8` bytes
(`w`, then `h`). A subscription id is a `u32` varint inside a message;
its stream preamble is specified in [§3](#3-stream-topology).

---

## 6. Message catalogue

| Stream | Direction | Message type |
|---|---|---|
| control (bidirectional) | client → server | `ClientMsg` |
| control (bidirectional) | server → client | `ServerMsg` |
| subscription (unidirectional) | server → client | `SubMsg` |

All three are postcard enums: one varint discriminant, then the variant's fields in the order
below. Declared in `mirai-proto/src/msg.rs`.

### 6.1 `ClientMsg`

| # | Variant | Field | Wire type | Semantics |
|---|---|---|---|---|
| **0** | `Hello` | `proto` | `ProtoVersion` | Version the client speaks. MUST be `0.1.0`. |
| | | `token` | `String` | Shared-secret credential. |
| | | `client` | `String` | Free-form identification (`mirai/<version>`). Informational; MUST NOT affect authorisation. |
| **1** | `Open` | `sub` | `u32` | Client-chosen id, unique among this connection's live subscriptions. |
| | | `engine` | `Option<String>` | `None` = the server's default (first configured) engine; `Some(name)` = that engine. |
| | | `req` | `AnalyzeReq` | The whole position and search parameters ([§7.3](#73-analyzereq)). |
| **2** | `Cancel` | `sub` | `u32` | Stop that subscription. Idempotent; unknown ids are ignored. |
| **3** | `ListEngines` | — | — | Ask for a fresh engine list. |
| **4** | `Ping` | *(unnamed)* | `u64` | Liveness probe; echoed verbatim. |

### 6.2 `ServerMsg`

| # | Variant | Field | Wire type | Semantics |
|---|---|---|---|---|
| **0** | `Welcome` | `proto` | `ProtoVersion` | Version the server speaks. MUST be `0.1.0`. |
| | | `server` | `String` | Server identification (`mirai-server/<version>`). |
| | | `session` | `u64` | Server-assigned connection id for log correlation. Opaque. |
| | | `engines` | `Vec<EngineDesc>` | Every engine offered, in configuration order; element 0 is the default. MAY be empty. |
| **1** | `Engines` | *(unnamed)* | `Vec<EngineDesc>` | Answer to `ListEngines`; MAY differ from `Welcome.engines`. |
| **2** | `Opened` | `sub` | `u32` | Subscription accepted. |
| **3** | `Pong` | *(unnamed)* | `u64` | Unmodified echo of `Ping`. |
| **4** | `Error` | `sub` | `Option<u32>` | `Some(id)` scopes the error to a subscription; `None` to the connection. |
| | | `code` | `ErrCode` | Machine-readable reason ([§6.4](#64-errcode)). |
| | | `msg` | `String` | Human detail. Clients MUST NOT parse it. |

### 6.3 `SubMsg`

| # | Variant | Payload | Semantics |
|---|---|---|---|
| **0** | `Report` | `Report` | Intermediate result. Zero or more. Not terminal. |
| **1** | `Done` | `Report` | Final result. **Terminal**; the stream is finished immediately after. |
| **2** | `Failed` | `String` | Search failed, human detail. **Terminal**; the stream is finished immediately after. |

At most one terminal message per stream, always the last frame.

### 6.4 `ErrCode`

| # | Variant | `as_str()` | Sent when |
|---|---|---|---|
| **0** | `BadVersion` | `bad-version` | `Hello.proto` is not `0.1.0`. The `msg` names both versions. |
| **1** | `Unauthorized` | `unauthorized` | `Hello.token` matches no configured token. |
| **2** | `NoSuchEngine` | `no-such-engine` | `Open.engine` names an unknown engine, or is `None` and no engine is configured. |
| **3** | `TooManySubs` | `too-many-subs` | The token's `max_subs` is already reached. |
| **4** | `BadRequest` | `bad-request` | First control message was not `Hello`, or exceeds the `Hello` bounds ([§9.3](#93-limits)); a second `Hello`; `Open.sub` duplicates a live id; `Open.req` is off the board or over a §9.3 request limit. |
| **5** | `EngineFailed` | `engine-failed` | Reserved. Not emitted by the reference server — engine failures arrive as `SubMsg::Failed`. |
| **6** | `Internal` | `internal` | Reserved. Not emitted by the reference server. |

A client MUST decode and handle all seven, including the two the reference server never sends.

---

## 7. Value types and quantisation

Declared in `mirai-proto/src/types.rs`; `Point`, `Size`, `Color` and `RuleSet` in
`mirai-core/src/point.rs` and `mirai-core/src/rules.rs`.

### 7.1 INV-1: point and array ordering

`index = y * width + x`; `y = 0` is the **top** row, `x = 0` the left column.
On a 19×19 board, indices 0–18 are the top row, 342–360 the bottom row,
and policy index 361 is pass.

| Rule | Level |
|---|---|
| A `Point` is a `u16` equal to `y*w + x`, with `y = 0` the **top** row. | MUST |
| `Point` `65535` (`Point::PASS`) means pass; no other out-of-board value may be sent. | MUST |
| `Report.ownership`, when present, has exactly `w*h` entries in that order. | MUST |
| `Report.policy`, when present, has exactly `w*h + 1` entries: the board, then **one pass slot last**. | MUST |
| Both board dimensions are in `2..=19` (`MIN_DIM`/`MAX_DIM`, KataGo's stock `MAX_LEN`); receivers reject anything else. | MUST |
| (Client) A report with invalid dimensions, points, or array lengths fails its subscription; discard the report and cancel as in [§8.4](#84-cancellation-inv-3). | MUST |

This is KataGo's ordering: `ownership` and `policy` index identically to the
board array. Reversing it silently mirrors overlays.

### 7.2 Quantisation

Scale constants are normative (`types.rs`): `LCB_SCALE` 16384.0 · `UTILITY_SCALE` 8192.0 ·
`SCORE_SCALE` 32.0 · `STDEV_SCALE` 32.0 · `POLICY_ILLEGAL` 65535 · `RAW_VAR_TIME_SCALE` 4.0.

`round(x)` is round-half-away-from-zero; `clamp(x, lo, hi)` is `min(max(x, lo), hi)`.

| Codec | Encode | Decode | Behaviour at the edges |
|---|---|---|---|
| `q16` / `dq16` — probabilities | `u16 = trunc(clamp(v, 0, 1) * 65535 + 0.5)` | `v = u / 65535` | input clamped to `[0,1]` before scaling |
| `qs` / `dqs` — signed scalars | `i16 = clamp(round(v * scale), -32767, 32767)`; **NaN → 0** | `v = i / scale` | saturates at ±32767; `-32768` never produced |
| `qu` / `dqu` — non-negative scalars | `u16 = clamp(round(v * scale), 0, 65535)`; **NaN → 0** | `v = u / scale` | negatives clamp to 0 |
| `q_own` / `dq_own` — ownership | `i8 = clamp(round(v * 127), -127, 127)` | `v = i / 127` | saturates at ±127; `-128` never produced |
| `q_policy` / `dq_policy` — policy | `v < 0` → `65535`; else `u16 = clamp(round(v * 65534), 0, 65534)` | `65535` → *illegal, no value*; else `v = u / 65534` | KataGo reports `-1` for illegal moves |

| Field group | Codec, scale | Range | Resolution | Max round-trip error | Guaranteed budget |
|---|---|---|---|---|---|
| winrate, prior | `q16` | `[0, 1]` | 1.526e-5 | ≈7.66e-6 | ≤ 1e-4 |
| `lcb` | `qs`, 16384 | ±1.99994 | 6.1e-5 | 3.05e-5 | — |
| `utility`, `utility_lcb` | `qs`, 8192 | ±3.99988 | 1.22e-4 | 6.1e-5 | — |
| `score_lead`, `score_selfplay`, `raw_lead` | `qs`, 32 | ±1023.97 points | 0.03125 pt | 0.015625 pt | ≤ 0.02 pt |
| `score_stdev` | `qu`, 32 | `[0, 2047.97]` points | 0.03125 pt | 0.015625 pt | — |
| ownership cell | `q_own` | `[-1, 1]` | 0.00787 | 0.00394 | ≤ 0.005 |
| policy cell | `q_policy` | `[0, 1]` ∪ illegal | 1.526e-5 | ≈7.66e-6 | — |
| `raw_var_time_left` | `qu`, 4 | `[0, 16383.75]` | 0.25 | 0.125 | — |

These error bounds apply to finite inputs within the codec's range; saturation is not
a round-trip error guarantee.

`utility_lcb` saturates at ±3.99988 for barely searched candidates. A receiver MAY read a
clipped value as "unsearched", but MUST NOT read it as a magnitude. Do not rescale.

### 7.2.1 INV-2: Black perspective

**Every winrate, score and utility on an MRP wire is from Black's perspective.** There is no
per-message perspective flag, and one MUST NOT be added.

| Rule | Level |
|---|---|
| Servers report Black-perspective values. The reference server guarantees it by launching KataGo with `reportAnalysisWinratesAs = BLACK`. | MUST |
| Clients MUST NOT assume side-to-move values. | MUST NOT |
| Conversion is display-time and client-side: `winrate_for(to_play) = to_play == Black ? w : 1 - w`; `score_lead_for(to_play) = lead * (Black ? +1 : -1)`; utility and `utilityLcb` use the score-lead rule, not `1 − u` (`types.rs` — `MoveInfo::winrate_for` / `score_lead_for` / `utility_for` / `utility_lcb_for`). | — |
| `RootInfo.current_player` is the side to move, and the argument to those conversions. | — |

Ownership shares the convention: `+1` fully Black, `-1` fully White (`Color::sign`).

### 7.3 `AnalyzeReq`

Fields in wire order. INV-4: the request is the entire position; servers retain nothing
between requests.

| # | Field | Wire type | Units / range | Semantics |
|---|---|---|---|---|
| 1 | `size` | `Size` (2 raw bytes) | each `2..=19` | Board dimensions. |
| 2 | `rules` | `RuleSet` varint | `0..=8` | Ruleset ([§7.5](#75-ruleset)). |
| 3 | `komi_x2` | `i16` zigzag varint | half-points | Komi × 2 ([§7.4](#74-komi)). |
| 4 | `initial_stones` | `Vec<(Color, Point)>` | | Pre-placed stones (handicap, set-up). Order not significant. |
| 5 | `moves` | `Vec<(Color, Point)>` | | Move list in game order, oldest first; `65535` = pass. |
| 6 | `initial_player` | `Option<Color>` | | Who moves first when `moves` is empty. Absent ⇒ Black if `initial_stones` is empty, else White (`AnalyzeReq::to_play`). |
| 7 | `max_visits` | `Option<u32>` | visits | Search cap; absent = engine default. |
| 8 | `max_time_ms` | `Option<u32>` | **milliseconds** | Wall-clock cap. Forwarded to KataGo as `overrideSettings.maxTime` in seconds. |
| 9 | `pv_len` | `Option<u8>` | moves | Maximum PV length (`analysisPVLen`). |
| 10 | `max_candidates` | `Option<u8>` | candidates | Keep only the first `n` candidates by KataGo's `order`; absent = all. The server applies it before quantising, so cut moves are neither decoded nor sent. |
| 11 | `want` | `Want` (1 raw byte) | bit flags | Optional extras ([§7.7](#77-want)). |
| 12 | `report_every_ms` | `Option<u16>` | **milliseconds** | Intermediate-report interval; absent = only the terminal message. Forwarded as `reportDuringSearchEvery` in seconds. Clients SHOULD request periodic reports only for live views. |
| 13 | `priority` | `i8` (1 raw byte) | | Higher runs first. A server MUST clamp it into `-8..=8` (`session.rs` — `PRIORITY_RANGE`) so one client cannot starve others. |
| 14 | `avoid` | `Vec<AvoidSpec>` | | Move restrictions ([§7.8](#78-avoidspec)). |
| 15 | `overrides` | `Vec<(String, String)>` | | Raw per-query KataGo `overrideSettings` entries. Servers SHOULD treat them as untrusted, MAY ignore them, and MUST NOT reject a request solely for an unknown key. See [§9.3](#93-limits) for the reference server's allowlist. |

### 7.4 Komi

INV-5: komi travels as `komi_x2: i16` — komi × 2 — because KataGo accepts only integer and
half-integer komi. `komi = komi_x2 / 2.0`; `komi_x2 = round(komi * 2)`. Examples: 7.5 → 15,
6.5 → 13, 0 → 0, −0.5 → −1. As an `i16` it is zigzag-then-varint: 15 → `0x1E`, −1 → `0x01`.

### 7.5 `RuleSet`

Varint discriminant in declaration order; the third column is the string a server forwards to
KataGo (`RuleSet::katago_name`). Receivers MUST reject a discriminant above 8.

| # | Variant | KataGo `rules` | | # | Variant | KataGo `rules` |
|---|---|---|---|---|---|---|
| 0 | `TrompTaylor` | `tromp-taylor` | | 5 | `StoneScoring` | `stone-scoring` |
| 1 | `Chinese` | `chinese` | | 6 | `Aga` | `aga` |
| 2 | `ChineseOgs` | `chinese-ogs` | | 7 | `AgaButton` | `aga-button` |
| 3 | `Japanese` | `japanese` | | 8 | `NewZealand` | `new-zealand` |
| 4 | `Korean` | `korean` | | | | |

### 7.6 `EngineDesc`

| # | Field | Wire type | Semantics |
|---|---|---|---|
| 1 | `name` | `String` | What `Open.engine` matches. Unique within a server. |
| 2 | `katago_version` | `String` | e.g. `1.16.4`; MAY be empty. |
| 3 | `model` | `String` | Neural-net identification; MAY be empty. |
| 4 | `analysis_threads` | `u16` | KataGo's `numAnalysisThreads` — concurrent positions this engine searches. |
| 5 | `max_board` | `Size` (2 raw bytes) | Largest board accepted. |
| 6 | `has_human_model` | `bool` (1 byte) | Whether a human-like policy model is loaded. |

### 7.7 `Want`

**One raw byte**, not a varint and not an enum.

| Bit | Value | Name | Effect |
|---|---|---|---|
| 0 | `0x01` | `OWNERSHIP` | `Report.ownership` populated (`w*h` entries). |
| 1 | `0x02` | `POLICY` | `Report.policy` populated (`w*h + 1` entries). |
| 2 | `0x04` | `PV_VISITS` | `MoveInfo.pv_visits` populated; otherwise empty. |
| 3 | `0x08` | `MOVES_OWNERSHIP` | Reserved. No MRP field carries per-move ownership; producers MUST leave it unset. |
| 4 | `0x10` | `ROOT_RAW` | Advisory request for `RootInfo.raw_*`: fields are present whenever the engine supplies them, whether or not the bit is set. Set it for forward compatibility; do not rely on it to suppress them. |
| 5–7 | `0xE0` | — | Reserved, MUST be 0. Receivers MUST ignore unknown bits rather than fail (`Want::from_bits_truncate`). |

### 7.8 `AvoidSpec`

| # | Field | Wire type | Semantics |
|---|---|---|---|
| 1 | `player` | `Color` varint | Side the restriction applies to. |
| 2 | `moves` | `Vec<Point>` | Restricted points. |
| 3 | `until_depth` | `u16` varint | Plies for which it holds. |
| 4 | `allow` | `bool` (1 byte) | `false` ⇒ an `avoidMoves` entry; `true` ⇒ an `allowMoves` entry (only these moves considered). |

A spec with an empty `moves` list MUST be dropped by the server rather than forwarded; KataGo
rejects the whole query otherwise (`mirai-engine/src/query.rs` — `build_query`).

### 7.9 `MoveInfo`

One candidate move; all values Black-perspective.

| # | Field | Wire type | Codec | Semantics |
|---|---|---|---|---|
| 1 | `mv` | `u16` varint | — | The candidate move; `65535` = pass. |
| 2 | `visits` | `u32` varint | — | Subtree visits. |
| 3 | `edge_visits` | `u32` varint | — | Visits through this edge. |
| 4 | `winrate` | `u16` varint | `q16` | Black's win probability after this move. |
| 5 | `prior` | `u16` varint | `q16` | Raw policy prior. |
| 6 | `lcb` | `i16` zz varint | `qs`, `LCB_SCALE` | Lower confidence bound on the win rate. |
| 7 | `utility` | `i16` zz varint | `qs`, `UTILITY_SCALE` | KataGo utility. |
| 8 | `utility_lcb` | `i16` zz varint | `qs`, `UTILITY_SCALE` | Lower confidence bound on utility. |
| 9 | `score_lead` | `i16` zz varint | `qs`, `SCORE_SCALE` | Expected lead in points, Black-positive. |
| 10 | `score_selfplay` | `i16` zz varint | `qs`, `SCORE_SCALE` | Self-play score estimate, points. |
| 11 | `score_stdev` | `u16` varint | `qu`, `STDEV_SCALE` | Score standard deviation, points. |
| 12 | `order` | `u8` raw byte | — | KataGo ranking; 0 is the engine's first choice. |
| 13 | `play_value` | `u32` varint | — | KataGo's `playSelectionValue`, rounded; drives play-mode sampling. |
| 14 | `pv` | `Vec<Point>` | — | Principal variation from this move, in order. |
| 15 | `pv_visits` | `Vec<u32>` | — | Per-ply visits along `pv`. Empty unless `PV_VISITS` was requested; when non-empty it SHOULD match `pv` in length. A client MUST NOT assume equal lengths. |

### 7.10 `RootInfo`

| # | Field | Wire type | Codec | Semantics |
|---|---|---|---|---|
| 1 | `visits` | `u32` varint | — | Total root visits. |
| 2 | `winrate` | `u16` varint | `q16` | Black's win probability at the root. |
| 3 | `score_lead` | `i16` zz varint | `qs`, `SCORE_SCALE` | Lead in points, Black-positive. |
| 4 | `score_selfplay` | `i16` zz varint | `qs`, `SCORE_SCALE` | Self-play score estimate. |
| 5 | `score_stdev` | `u16` varint | `qu`, `STDEV_SCALE` | Score standard deviation. |
| 6 | `utility` | `i16` zz varint | `qs`, `UTILITY_SCALE` | Root utility. |
| 7 | `current_player` | `Color` varint | — | Side to move at the analysed position. |
| 8 | `raw_winrate` | `Option<u16>` | `q16` | Single-evaluation (searchless) win rate. |
| 9 | `raw_lead` | `Option<i16>` | `qs`, `SCORE_SCALE` | Single-evaluation lead. |
| 10 | `raw_var_time_left` | `Option<u16>` | `qu`, `RAW_VAR_TIME_SCALE` | KataGo's `rawVarTimeLeft`; unitless. |

### 7.11 `Report`

Intermediate and final reports have the same shape; only the wrapping `SubMsg` variant differs.

| # | Field | Wire type | Semantics |
|---|---|---|---|
| 1 | `turn` | `u16` varint | Turn of the analysed position: 0 = before the first move, else the request's move count. |
| 2 | `root` | `RootInfo` | Root statistics. |
| 3 | `moves` | `Vec<MoveInfo>` | Candidates **sorted ascending by `order`**; `moves[0]` is the engine's choice. Servers MUST sort; clients MAY rely on it. |
| 4 | `ownership` | `Option<Vec<i8>>` | Map or stream delta; see [§7.1](#71-inv-1-point-and-array-ordering) for length and [below](#711-report) for restoration. |
| 5 | `policy` | `Option<Vec<u16>>` | See [§7.1](#71-inv-1-point-and-array-ordering) for length and pass slot. |

**Ownership on a subscription stream.** Between two reports of one search most points move by
a step or two. Sent as those changes, a live report measured a quarter smaller
([§11](#11-reference-figures)). So both ends of a subscription stream keep the last map the
stream carried:

| # | Rule | Level |
|---|---|---|
| 1 | A report whose `ownership` has the same length as the stream's last map carries `own[i] − last[i]` (wrapping `i8` arithmetic) instead of `own[i]`; any other map goes whole. The receiver restores `last[i] + wire[i]`, wrapping. | MUST |
| 2 | Either way the map, not the difference, becomes the stream's last map. A report without `ownership` leaves it unchanged. | MUST |
| 3 | Every other field is sent as is. | — |

The last map is scoped to the stream. After restoration, each report is a complete
snapshot, so a client MAY drop older reports without delivering them. Frame
decoding still follows [§4.1](#41-subscription-streams-one-zstd-stream).

---

## 8. Session state machine

### 8.1 Handshake

The client pins the leaf certificate ([§2.2](#22-certificates-trust-on-first-use)),
then opens the control stream and sends `Hello` ([§3](#3-stream-topology)).

| Rule | Level |
|---|---|
| (Server) A first control message other than `Hello` is fatal: `Error { None, BadRequest }`, then close with application code 1. | MUST |
| (Server) Send nothing before receiving `Hello`, except an `Error`. | MUST NOT |
| (Client) Ignore, rather than fail on, any other message arriving before `Welcome`. | SHOULD |

**Version check.** ALPN is the fixed identifier `mirai` and carries no version, so a mismatch
completes the TLS handshake and is reported as `BadVersion` rather than failing opaquely in
TLS. Peers MUST require an exact match with the version they speak (`0.1.0`,
[§10](#10-version)). A server MUST reject any other `Hello.proto` with `BadVersion`, naming
both versions, and close. A client MUST treat any other `Welcome.proto` as unusable and MUST
NOT send `Open`. There is no other feature negotiation — capabilities MUST NOT be inferred from
the `client`/`server` identification strings.

**Pre-authentication bound.** The reference server takes a session slot before the
handshake (32, `MAX_SESSIONS` in `mirai-server`). A peer that finishes the handshake
and then stays silent would otherwise hold that slot for the life of the connection:
QUIC keep-alives reset the idle timer. The server releases the slot if `Hello` has
not arrived within 10 s of the slot being taken (`Budgets::preauth` in `session.rs`).
The clock covers the handshake, opening the control stream and the first frame. A
first frame that is refused is answered within 2 s more ([§8.6](#86-connection-loss-and-shutdown)),
so an unauthenticated peer holds a slot for at most 12 s. A client SHOULD send `Hello`
immediately after the handshake; a server MAY enforce its own bound.

Once the handshake has completed, expiry closes with application code 1 and reason
`pre-authentication deadline`, and no `Error` frame — there may be no control stream
to write one on. That is not an authentication failure. If the handshake itself has
not finished, there are no 1-RTT keys, so an application close cannot be sent.
Dropping the attempt is a transport `CONNECTION_CLOSE` with `APPLICATION_ERROR` and
an empty reason. A client that has not sent `Hello` SHOULD treat either close as the
connection ending and MAY retry.

### 8.2 Authentication

The credential is the opaque UTF-8 `Hello.token`, checked against a server-side list
(`session.rs` — `Host::authenticate`).

| Rule | Level |
|---|---|
| Compare in **constant time** with respect to both content and match position: compare against **every** configured token, never returning early on the first match or first differing byte. | MUST |
| Compare raw bytes: exact, case-sensitive, no trimming, no prefix match. | MUST |
| A server with no configured tokens rejects every client. | MUST |
| On failure reply `Error { None, Unauthorized }`, then close with application code 1; do not distinguish "unknown" from "wrong" in `msg`. | MUST |
| Tokens carry ≥ 128 bits of entropy. `mirai-server --generate-token` emits 64 lowercase hex characters. | SHOULD |

### 8.3 Subscription lifecycle

A subscription id is retired when its request is rejected, it is cancelled, or
its stream ends. Stream termination follows [§3](#3-stream-topology);
`Done` and `Failed` are terminal messages ([§6.3](#63-submsg)).

| Rule | Level |
|---|---|
| `sub` is client-chosen and unique among the connection's live subscriptions. The reference client counts up from 1 and never reuses an id within a connection. | MUST |
| (Server) Reject a duplicate live id with `Error { Some(sub), BadRequest }`, and reap finished subscriptions before applying that and the `max_subs` check. | MUST |
| (Client) Treat a subscription stream that reaches clean EOF without `Done`/`Failed` as failed — never as success, never waiting indefinitely. | MUST |

If a subscription stream ends or stalls before its complete four-byte preamble,
the client cannot know its id. The reference client closes the connection and fails
all live subscriptions, with a 10 s preamble deadline. A preamble-free
`RESET_STREAM` with code 1 (`CODE_CANCELLED`) is an intentional cancellation
instead and does not fail unrelated searches. A stream whose id is known but
ends without a terminal message fails only that subscription.

### 8.4 Cancellation (INV-3)

Cancellation must *discard* in-flight reports, so it is propagated on both streams.

| Actor | Required actions, in order |
|---|---|
| **Client** | 1. If the subscription stream has arrived, `STOP_SENDING` on it with application code 1 to discard buffered reports. 2. Send `ClientMsg::Cancel { sub }` on the control stream. 3. Retire the id and ignore later reports for it. |
| **Server** | On `Cancel` or `STOP_SENDING`, stop the search and **reset** any open subscription stream with application code 1 (`CODE_CANCELLED`). Resetting is REQUIRED: finishing the stream would deliver the stale reports the client just refused. Stopping MUST NOT wait for the client to read, since flow control may block writes. Unknown ids MUST be ignored silently. |

A client MAY send only one of the two and a server MUST cope, but both are RECOMMENDED:
`STOP_SENDING` discards buffered reports immediately; `Cancel`
stops the search even if there is no stream yet or the stream is blocked on flow
control. Connection loss cancels everything implicitly.

### 8.5 Other control requests

| Request | Response | Notes |
|---|---|---|
| `Ping(n)` | `Pong(n)` | `n` echoed unchanged. Unsolicited `Pong` MUST NOT be sent. QUIC keep-alive already covers transport liveness. |
| `ListEngines` | `Engines(..)` | Current engine list; MAY differ from `Welcome.engines`. |

### 8.6 Connection loss and shutdown

| Rule | Level |
|---|---|
| On connection loss both endpoints treat every live subscription on it as failed. | MUST |
| Subscriptions are **never** replayed, resumed or reissued after a reconnect; a new connection starts empty. Reissuing is the application's decision, based on what the user is looking at now. | MUST NOT |
| (Server) Stop the underlying searches when the connection goes away. Every exit path of the reference server's `pump` drops the engine subscription, which terminates the KataGo query. | MUST |
| (Client) Reconnecting automatically is optional; capped exponential backoff is RECOMMENDED (reference: 0.5 s, 1 s, 2 s, 4 s, then 8 s forever — `remote.rs` — `backoff`). | MAY / SHOULD |
| (Client) Never treat a fingerprint mismatch, `BadVersion` or `Unauthorized` as transient. Background retries are allowed, but the session MUST NOT be reported as healthy (`remote.rs` — `RemoteStatus::Failed`). | MUST |

Both peers close normally with application code 0 and reason `bye` ([§9.2](#92-quic-application-codes)).
On a fatal handshake rejection the server MUST attempt to write and finish the
`Error` frame before closing with code 1 and the `ErrCode::as_str()` reason.
It SHOULD wait for the frame's acknowledgement so the peer can read the
explanation. The reference server bounds the write and acknowledgement wait
to 2 s; an unresponsive peer cannot hold a session slot indefinitely and may
receive only the connection close, not the `Error` frame.

The reference server queues up to 32 control replies per connection. While the queue
is full it stops reading the control stream until the queue drains, so a burst of
pipelined requests is answered in full. A peer that does not read its replies
is disconnected, with application code 0, once a control write has stalled for
10 s; its subscriptions then cancel as on any connection loss.

---

## 9. Errors and limits

### 9.1 Scope and fatality

| Error | Scope and effect |
|---|---|
| `BadRequest` on the first control message, `BadVersion`, `Unauthorized` | Connection closes with application code 1 after an `Error { None, .. }`. |
| `BadRequest` on a second `Hello` | Non-fatal `Error { None, .. }`; the connection continues. |
| `BadRequest`, `TooManySubs`, `NoSuchEngine` on `Open` | `Error { Some(sub), .. }` rejects that subscription; the connection continues. |
| `SubMsg::Failed` | Ends its subscription stream with FIN ([§6.3](#63-submsg)). |

Triggers are in [§6.4](#64-errcode); request bounds are in [§9.3](#93-limits).

Client rules: fail exactly the named subscription on `Error { Some(sub) }` and keep the
connection · on `Error { None }` do not tear down subscriptions unilaterally; surface it and
wait to see whether the server closes · ignore an `Error` naming an unknown or already-terminal
subscription · treat `Error` as advisory *in addition to*, never *instead of*, the transport
signal — a server MAY close without sending one.

### 9.2 QUIC application codes

| Where | Code | Meaning |
|---|---|---|
| connection close, either side | 0 (`bye`) | orderly shutdown |
| connection close, server | 1 | fatal handshake rejection ([§8.1](#81-handshake), [§8.2](#82-authentication)) or post-handshake pre-authentication deadline ([§8.1](#81-handshake)) |
| `RESET_STREAM` on a subscription stream (server) | 1 | subscription cancelled |
| `STOP_SENDING` on a subscription stream (client) | 1 | subscription cancelled |

No other code carries meaning beyond "the peer is gone".

### 9.3 Limits

| Limit | Value | Enforcement |
|---|---|---|
| Outstanding subscriptions | token's `max_subs`, default 64; shared by all connections using that token on the reference server. Counts requests queued in the engine as well as running ones: the reference server forwards every accepted `Open` at once and KataGo schedules them — `numAnalysisThreads` at a time, then highest `priority` first, earlier first on a tie, without pre-empting a started search. The limit guards against a runaway client; it is not the concurrency control | server MUST refuse further `Open` with `TooManySubs`. A cancelled search holds its permit until its pump drops the engine subscription. At the limit, only a connection that just cancelled one of its own searches waits up to 1 s (`CANCEL_GRACE`) for that search to stop before refusing; other connections are refused immediately. Clients MUST NOT assume either a per-connection or global token quota |
| `AnalyzeReq.moves` + `initial_stones` | ≤ 4096 together on the reference server (`MAX_REQUEST_STONES`) | refused with `Error { Some(sub), BadRequest }`; limits zstd-inflated engine input |
| `AnalyzeReq.report_every_ms` | ≥ 20 ms when present on the reference server | smaller intervals raised to 20 ms before forwarding to KataGo |
| `AnalyzeReq.max_time_ms` | ≤ 6 h on the reference server | longer times reduced to 6 h before forwarding; `Some(0)` is forwarded but is not a time cap for the visit limit below |
| `AnalyzeReq.max_visits` | ≤ 10,000,000 without a nonzero time cap on the reference server | an absent or larger visit limit is capped; a time-limited search may use `u32::MAX` and ends on time instead |
| `AnalyzeReq.avoid` | ≤ 64 specs and ≤ 1024 points across them on the reference server (`MAX_AVOID_SPECS`, `MAX_AVOID_POINTS`) | refused with `Error { Some(sub), BadRequest }` |
| `AnalyzeReq.overrides` | only `wideRootNoise` and `humanSLProfile`, values ≤ 64 bytes, on the reference server (`FORWARDED_OVERRIDES`, `MAX_OVERRIDE_VALUE`) | other entries dropped, not rejected ([§7.3](#73-analyzereq)) |
| First control frame | payload and decompressed plaintext each ≤ 1024 bytes on the reference server (`MAX_HELLO_FRAME`) | header checked before reading the body; inflation stops at the bound; `Error { None, BadRequest }`, then close with code 1 |
| `Hello.token`, `Hello.client` | ≤ 256 bytes each on the reference server (`MAX_HELLO_FIELD`) | `Error { None, BadRequest }`, then close with code 1, before checking the token. A server MUST NOT log untrusted `client` unbounded or unescaped |

Other bounds: framing [§4](#4-framing), stream counts [§2.1](#21-quic-and-tls)
and [§3](#3-stream-topology), board dimensions [§7.1](#71-inv-1-point-and-array-ordering),
priority [§7.3](#73-analyzereq), and time to `Hello` [§8.1](#81-handshake).

---

## 10. Version

The protocol version is `0.1.0` (`PROTO_VERSION` in `mirai-proto/src/types.rs`), carried as
`ProtoVersion { major: u16, minor: u16, patch: u16 }` in `Hello` and `Welcome`. Peers MUST
match it exactly; a mismatch is `BadVersion` ([§8.1](#81-handshake)), and the error names
both versions.

During development, peers are built from the same tree and the version does not
change for every protocol edit. It starts moving when external users need compatibility.

---

## 11. Reference figures

Two sources. `mirai-proto/tests/wire_size.rs` pins single messages on synthetic data and
fails `cargo test` if they grow; `mirai-engine/examples/wire_bench.rs` measures a real
search ([TESTING.md §4](TESTING.md#4-verifying-against-a-real-engine)).

| Message | Contents | postcard payload | Compressed | **Framed** |
|---|---|---|---|---|
| `SubMsg::Report` | 19×19, 50 candidates, 15-move PVs with `pv_visits`, 361-entry ownership, no policy; first frame of a subscription stream | 4128 B | yes → 2587 B | **2592 B** |
| `ClientMsg::Open` | 19×19, 200 moves, `want = OWNERSHIP\|PV_VISITS`, `max_visits`, `report_every_ms` | 496 B | no | **501 B** |
| `SubMsg::Report` | 2 candidates, 2-move PVs, no ownership or policy ([Appendix A](#appendix-a-worked-exchange)) | 77 B | no | **82 B** |
| `ClientMsg::Ping` | one `u64` | 2 B | no | **7 B** |

A real search: `katago analysis` on an 18-move 19×19 opening, 20 s at
`reportDuringSearchEvery = 0.1`, 196 reports of 63 candidates on average. KataGo's JSON for
one report averaged **33.6 KB**, and 240 µs to parse and quantise. Mean framed bytes per
report, each row adding one change to the one above:

| Wire | Policy off | Policy on |
|---|---|---|
| every candidate, with `pv_visits`, each report framed alone | 2318 B | 2656 B |
| without `pv_visits`, which no client reads | 2185 B | 2594 B |
| `max_candidates = 10`, what the board shows by default | 738 B | 1147 B |
| one zstd stream per subscription ([§4.1](#41-subscription-streams-one-zstd-stream)) | 198 B | 197 B |
| ownership as changes ([§7.11](#711-report)) — current wire | **148 B** | **148 B** |

At `report_every_ms = 100`, the current wire costs about **1.5 KB/s** per
subscription, against 23 KB/s when every candidate was framed alone and 336 KB/s for
KataGo JSON. The first frame has no history to compress against (654 B in this run);
a new stream per position pays that cost each time. An unchanged policy costs one
back-reference. Encoding takes about 14 µs at zstd level 6, decoding about
3 µs. `MAX_FRAME` is a safety ceiling, not a working budget.

## Appendix A: worked exchange

Every byte below was produced by an independent encoder written from this specification; the
`Open` and `Report` sizes reproduce the reference implementation's measured frame sizes
exactly. `|` marks the header/payload boundary for readability only.

**0 — connect.** UDP to `box.local:9678`, QUIC + ALPN `mirai` + TLS 1.3. Hash the leaf DER
with SHA-256, render 64 lowercase hex characters, compare with the stored pin. No pin stored ⇒
record and ask the user; mismatch ⇒ abort before any frame is sent.

**1 — `Hello { proto: 0.1.0, token: "t0k", client: "demo/1" }`**

```
0f 00 00 00 | 00 | 00 00 01 00 03 74 30 6b 06 64 65 6d 6f 2f 31
len=15       flags  ^  ^  ^  ^  ^  "t0k"  ^  "demo/1"
                    |  |  |  |  len 3     len 6
                    |  major minor patch = 0.1.0
                    variant 0 = Hello
```

**2 — `Welcome`** with `server: "mirai-server/0.1.0"`, `session: 1`, one engine
`{ "default", "1.16.4", "b18c384nbt", 4 threads, 19×19, no human model }`

```
37 00 00 00 | 00 | 00 00 01 00 12 6d 69 72 61 69 2d 73 65 72 76 65 72 2f 30 2e 31 2e 30
                   01 01 07 64 65 66 61 75 6c 74 06 31 2e 31 36 2e 34
                   0a 62 31 38 63 33 38 34 6e 62 74 04 13 13 00

00 variant 0 = Welcome · 00 01 00 proto 0.1.0 · 12 "mirai-server/0.1.0" (len 18) · 01 session = 1
01 engines: 1 element · 07 "default" · 06 "1.16.4" · 0a "b18c384nbt"
04 analysis_threads · 13 13 max_board 19x19 (raw u8) · 00 has_human_model = false
```

**3 — `Open { sub: 1, engine: None, req }`**, empty 19×19 Chinese board, komi 7.5,
`max_visits: Some(1000)`, `want: OWNERSHIP`, `report_every_ms: Some(250)`

```
17 00 00 00 | 00 | 01 01 00 13 13 01 1e 00 00 00 01 e8 07 00 00 00 01 01 fa 01 00 00 00

01 variant 1 = Open · 01 sub = 1 · 00 engine = None (server default)
13 13 size 19x19 · 01 rules = Chinese · 1e komi_x2: zigzag 30 -> 15 -> komi 7.5
00 initial_stones: 0 · 00 moves: 0 · 00 initial_player = None (=> Black)
01 e8 07 max_visits = Some(1000) · 00 max_time_ms = None · 00 pv_len = None
00 max_candidates = None
01 want = OWNERSHIP (raw byte) · 01 fa 01 report_every_ms = Some(250)
00 priority = 0 (raw byte) · 00 avoid: 0 · 00 overrides: 0
```

**4 — `Opened { sub: 1 }`**, then the subscription stream's raw preamble:

```
02 00 00 00 | 00 | 02 01          02 = variant 2 (Opened), 01 = sub 1
01 00 00 00                       stream preamble: sub id, LE u32, no frame
```

**5 — one intermediate `Report`**: 1000 root visits, root winrate 0.4837 and lead −1.5 points
(Black), two candidates with 2-move PVs, no ownership yet, no policy. It is shown as an
uncompressed `0x00` frame, which a subscription stream accepts
([§4.1](#41-subscription-streams-one-zstd-stream)); the reference server sends the same bytes
compressed into the stream's zstd stream, flags `0x02`.

```
4d 00 00 00 | 00 | 00 00 e8 07 d3 f7 01 5f 53 c0 03 d7 07 00 00 00 00 02
                   48 d8 04 d7 04 d3 f7 01 b3 66 a2 79 b4 06 90 05 5f 53 c0 03 00 d8 04
                      02 48 a0 02 00
                   a0 02 90 03 8f 03 9f f5 01 b3 66 88 78 b4 06 90 05 6b 5f c0 03 01 90 03
                      02 a0 02 48 00
                   00 00
```

| Bytes | Field | Value |
|---|---|---|
| `00` | `SubMsg` variant | 0 = `Report` |
| `00` | `turn` | 0 |
| `e8 07` | `root.visits` | 1000 |
| `d3 f7 01` | `root.winrate` | 31699 → 31699/65535 = 0.4837 (Black) |
| `5f` | `root.score_lead` | zigzag 95 → −48 → −48/32 = −1.5 points |
| `53` | `root.score_selfplay` | zigzag 83 → −42 → −1.3125 |
| `c0 03` | `root.score_stdev` | 448 → 448/32 = 14.0 |
| `d7 07` | `root.utility` | zigzag 983 → −492 → −492/8192 = −0.0601 |
| `00` | `root.current_player` | 0 = Black |
| `00 00 00` | `raw_winrate`, `raw_lead`, `raw_var_time_left` | all `None` |
| `02` | `moves` count | 2 |
| `48 d8 04 d7 04` | candidate 0: `mv`, `visits`, `edge_visits` | Point(72) = (x 15, y 3), 600, 599 |
| `d3 f7 01 b3 66` | `winrate`, `prior` | 0.4837, 13107 → 0.2000 |
| `a2 79 b4 06 90 05` | `lcb`, `utility`, `utility_lcb` | 7761/16384 = 0.4737, 410/8192 = 0.0500, 328/8192 = 0.0400 |
| `5f 53 c0 03` | `score_lead`, `score_selfplay`, `score_stdev` | −1.5, −1.3125, 14.0 |
| `00 d8 04` | `order`, `play_value` | 0 (raw byte), 600 |
| `02 48 a0 02` | `pv` | 2 elements: Point(72), Point(288) |
| `00` | `pv_visits` | 0 elements — `PV_VISITS` was not requested |
| `a0 02 90 03 8f 03 …` | candidate 1 | Point(288), 400 visits, 399 edge, winrate 0.4790, lead zigzag 107 → −54 → −1.6875, `order` 1 |
| `00 00` | `ownership`, `policy` | both `None` |

The client converts at display time: `current_player` is Black, so it shows 0.4837 and −1.5.
With White to move the same bytes display as 0.5163 and +1.5.

**6 — `Done`**, same body, variant 1 — `4d 00 00 00 | 00 | 01 00 e8 07 …` — followed by a clean
FIN on the unidirectional stream. The client retires subscription 1.

**7 — teardown.** The client closes the connection with application code 0, reason `bye`. Had
it lost interest at step 5 instead, it would have sent `STOP_SENDING(1)` on the subscription
stream plus `Cancel { sub: 1 }` — `02 00 00 00 00 02 01` — and the server would have reset the
stream with code 1 rather than finishing it. Note that those bytes are identical to
`Opened { sub: 1 }`: frames carry no type tag, and direction is what tells them apart.

