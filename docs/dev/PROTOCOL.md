<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Huang Zhaobin -->

# MRP/2 — the mirai Remote Protocol, version 2

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
| [10](#10-versioning-and-compatibility) | Versioning and compatibility |
| [11](#11-reference-figures) | Reference figures |
| [12](#12-conformance-checklist) | Conformance checklist |
| [A](#appendix-a-worked-exchange) | Worked exchange, byte for byte |
| [B](#appendix-b-findings) | Findings and under-specified points |

---

## 1. Status and scope

| Item | Value | Constant |
|---|---|---|
| Version | 2, carried as `u16` | `PROTO_VERSION` (`mirai-proto/src/types.rs`) |
| ALPN | `mirai/2` | `ALPN` (`mirai-proto/src/transport.rs`) |
| Default port | UDP 9678 | `DEFAULT_PORT` (same file) |
| URL scheme | `mirai://host[:port]` | `URL_SCHEME` (same file) |
| Server default bind | `0.0.0.0:9678` | `DEFAULT_LISTEN` (`mirai-server/src/config.rs`) |

MRP/2 carries Go position analysis between an analysis GUI and a server owning KataGo
processes. Three properties define its shape:

* **Stateless requests (INV-4).** Every request carries the whole position — initial stones,
  moves, rules, komi. Servers keep no position state; nothing needs resynchronising after a
  reconnect.
* **Streamed results.** One request yields a stream of progressively refined reports until a
  terminal message or a cancellation.
* **Quantised values.** No float ever crosses the wire ([§7](#7-value-types-and-quantisation)).

### URL syntax

```
mirai-url = [ "mirai://" ] host [ ":" port ] [ "/" ]
host      = reg-name | IPv4address | "[" IPv6address "]"
port      = 1*DIGIT                                       ; default 9678
```

Parsing (`transport.rs` — `parse_url`), which a client MUST reproduce:

1. Trim whitespace; strip an optional `mirai://` prefix; strip trailing `/`.
2. Empty host is an error.
3. A leading `[` starts an IPv6 literal ending at the first `]`; an optional `:port` follows.
4. Otherwise split host and port at the **last** `:`; a non-numeric port is an error.
5. Absent port means 9678.

No path, query or userinfo. Credentials travel in `Hello.token`, never in the URL.

---

## 2. Transport

### 2.1 QUIC and TLS

| Requirement | Level | Value |
|---|---|---|
| Transport | MUST | QUIC v1 (RFC 9000) over UDP |
| TLS | MUST | 1.3 only; older versions neither offered nor accepted |
| ALPN | MUST | exactly `mirai/2`; anything else MUST fail the connection |
| Client certificates | MUST NOT | neither side requests nor sends them |
| 0-RTT | MUST NOT be relied on | not enabled by either reference endpoint |

QUIC transport parameters (`transport.rs` — `transport_config`), shared by both endpoints:

| Parameter | Reference value | Status |
|---|---|---|
| `keep_alive_interval` | 5 s | **Advisory.** Any interval, or none. ≤ ⅓ of the peer's idle timeout is RECOMMENDED. |
| `max_idle_timeout` | 30 s | **Advisory value, normative kind.** QUIC takes the minimum of the two advertised values; an implementation MUST tolerate any peer value. |
| `max_concurrent_uni_streams` | 256 | **Normative floor.** The server opens one unidirectional stream per subscription, so a client MUST advertise at least its intended concurrent subscription count. |
| `stream_receive_window` | 16 KiB | **Recommended for clients.** A server writing to a subscription stream blocks once it is one window ahead of what the client has read, and only then can it replace a queued report with a newer one. quinn's 1.25 MB default lets hundreds of stale reports queue on a slow link, all delivered late and in order. |
| bidirectional streams | 1 per connection | A server MUST permit at least one; a client MUST NOT open a second. |

Other flow-control windows, migration and datagrams are implementation choices outside MRP/2.

### 2.2 Certificates: trust on first use

MRP/2 does not use PKI. Servers are LAN boxes; requiring a CA-issued certificate for
`mirai://basement.local` would mean public DNS plus ACME, or a private CA the user installs.
The client pins the key directly — the SSH host-key model.

| Rule | Level |
|---|---|
| The pin is the **lowercase hex SHA-256 of the leaf certificate's DER**, 64 characters, no separators. Only the leaf is hashed; intermediates are ignored. | MUST |
| Self-signed leaves are expected and normal. | — |
| First connection with no stored pin: record the observed fingerprint; SHOULD show it to the user before persisting. | MUST / SHOULD |
| Later connections: compare, and abort the TLS handshake on mismatch. A mismatch is a **hard failure** — no fallback check, no one-time override, no retry of that connection. | MUST |
| Do NOT validate against a trust store, do NOT validate hostname/SAN, do NOT reject on `notBefore`/`notAfter`. The fingerprint is the entire check. | MUST |
| Still verify the TLS 1.3 `CertificateVerify` signature against the leaf's public key. Pinning replaces chain validation, not proof of key possession. | MUST |
| Normalise a user-supplied pin — trim, lowercase, strip `:` — so a fingerprint pasted from `openssl x509 -fingerprint -sha256` works. | SHOULD |

Reference: `transport.rs` — `TofuVerifier` compares fingerprints and delegates signature
checking to rustls; `fingerprint_of` / `sha256.rs` — `fingerprint` produce the pin.

**SNI.** rustls requires a syntactically valid server name, so a client whose URL host is an IP
literal sends the SNI name `localhost` (`transport.rs` — `connect`). Servers MUST NOT route or
authorise on SNI, and MUST NOT reject a handshake because SNI disagrees with the certificate.

**Provisioning.** A server MAY generate a self-signed certificate on first start
(`transport.rs` — `load_or_generate_cert`: rcgen pair for the configured hostnames, key written
`0600`, reused on later starts so the pin stays stable). Rotating the certificate invalidates
every client pin and is a user-visible event.

---

## 3. Stream topology

```
                        client                                    server
                          |                                          |
   QUIC connect, ALPN "mirai/2", TLS 1.3, leaf pinned                 |
                          |=========================================>|
   client-opened BIDIRECTIONAL control stream (exactly one)           |
                          |----- ClientMsg frames ------------------->|
                          |<---- ServerMsg frames --------------------|
                          |          (lives for the whole session)    |
   server-opened UNIDIRECTIONAL stream, one per subscription:         |
      sub 1               |<-- [01 00 00 00] SubMsg frames ... Done --|
      sub 2               |<-- [02 00 00 00] SubMsg frames ... Done --|
      sub 7               |<-- [07 00 00 00] SubMsg frames ... RESET -|  (cancelled)
```

| # | Rule | Level |
|---|---|---|
| 1 | After the handshake the client opens exactly one bidirectional stream and sends `ClientMsg::Hello` as its first frame. | MUST |
| 2 | The control stream carries `ClientMsg` client→server and `ServerMsg` server→client for the whole connection. | MUST |
| 3 | The client opens no further bidirectional streams. | MUST NOT |
| 4 | For each accepted `Open` the server opens a unidirectional stream whose **first four bytes are the subscription id as a little-endian `u32`, outside any frame**; framed `SubMsg` values follow. | MUST |
| 5 | The client identifies a unidirectional stream solely by that preamble, and tolerates it arriving before, after or interleaved with `Opened`. | MUST |
| 6 | `SubMsg` never appears on the control stream; `ServerMsg` never appears on a subscription stream. | MUST NOT |
| 7 | A subscription stream ends with either a `Done`/`Failed` frame plus a clean FIN, or a QUIC `RESET_STREAM`. | MUST |

Why a stream per subscription: cancellation must *discard* in-flight reports. On one ordered
byte stream they would still have to be received and parsed before later data. With one QUIC
stream per subscription, `STOP_SENDING` and `RESET_STREAM` drop them inside the transport.

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

Constants (`mirai-proto/src/frame.rs`): `MAX_FRAME` = 8 MiB = 8388608 · `COMPRESS_THRESHOLD` =
4096 · `FLAG_ZSTD` = `0x01` · `FLAG_SUB_ZSTD` = `0x02` · control-stream zstd level 1 ·
subscription window 2^16 (`SUB_WINDOW_LOG`) · `SUB_STREAM_LEVEL`, the reference sender's level.

| # | Rule | Level |
|---|---|---|
| 1 | Never emit `len > MAX_FRAME`. | MUST NOT |
| 2 | Read the 5-byte header first and reject `len > MAX_FRAME` **before** reading or allocating the body. | MUST |
| 3 | **Decompression-bomb guard:** bound each frame's decompressed size and abort as soon as the plaintext would exceed `MAX_FRAME`. Never trust a content-size field inside the zstd frame. (`frame.rs` — `bounded` on the control stream, `SubStreamDecoder::inflate` on subscription streams.) | MUST |
| 4 | Reject a `flags` value the stream does not allow. | MUST |
| 5 | A payload decodes to exactly one message; reject trailing bytes after it. | MUST |
| 6 | Control stream: compress iff the postcard payload is **strictly greater than** `COMPRESS_THRESHOLD`. Interop does not depend on it: receivers MUST accept either form at any size, so a minimal implementation MAY always send `flags = 0x00`. | SHOULD / MUST |
| 7 | Control stream: the reference encoder streams at level 1 with **no content-size field and no checksum**; receivers MUST NOT require either. | MUST NOT |
| 8 | A stream ending exactly at a frame boundary is a graceful end, not an error. Ending inside a frame is an error. | MUST |
| 9 | Frames carry no message-type tag: the type follows from stream and direction. | — |

### 4.1 Subscription streams: one zstd stream

A subscription stream carries a single zstd stream that starts in its first `0x02` frame and
is never ended. The sender compresses each message into it and flushes (`ZSTD_e_flush`) at the
end of the frame, so every frame decodes as soon as it arrives, and every report is compressed
against the reports before it. Consecutive reports of one search differ in a few numbers;
measured on a real search, a stream carries about a quarter of what the same reports cost
framed one by one ([§11](#11-reference-figures)).

| # | Rule | Level |
|---|---|---|
| 1 | The receiver decodes every frame of the stream, in order, including one whose report it then drops: each `0x02` frame continues the zstd stream. | MUST |
| 2 | The sender flushes at the end of every frame. A receiver MUST NOT need the next frame to finish decoding this one. | MUST |
| 3 | The zstd window is at most 2^16 bytes. It bounds each stream's memory on the client, which MUST refuse a stream that declares a larger one. | MUST |
| 4 | A `0x00` frame stays outside the zstd stream, which it neither advances nor resets. Any frame MAY be sent that way. | MAY |
| 5 | Level, checksum and content-size flag are the sender's choice; the reference sender uses `SUB_STREAM_LEVEL` with neither checksum nor content size. | — |

A stream's zstd state never crosses into another stream, so cancelling one — which throws its
unread bytes away ([§8.4](#84-cancellation-inv-3)) — cannot desynchronise any other.

### Worked frame

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

Each row below was verified against postcard 1.1.3's own sources and README rather than
assumed; the last column names what was read (paths relative to the crate root
`~/.cargo/registry/src/*/postcard-1.1.3/`).

| Construct | Encoding | Confirmed in |
|---|---|---|
| `bool` | one byte, `0x00` / `0x01` | `src/ser/serializer.rs` — `serialize_bool` |
| `u8` | one raw byte, **no varint** | `serialize_u8` |
| `i8` | one raw byte, two's complement, **no varint, no zigzag** | `serialize_i8` |
| `u16`, `u32`, `u64` | unsigned LEB128 varint | `serialize_u16/u32/u64`, `src/varint.rs` |
| `i16` | **zigzag, then varint**: `zz = ((v << 1) ^ (v >> 15)) as u16`, then varint(`zz`) | `serialize_i16` + `zig_zag_i16`; decode `src/de/deserializer.rs` — `deserialize_i16` |
| `Option<T>` | `0x00` = none; `0x01` + value = some | `serialize_none` / `serialize_some`; decode rejects any other tag (`DeserializeBadOption`) |
| enum variant | varint of the **zero-based declaration-order index**, then the variant's fields (nothing extra for a unit variant) | `serialize_unit_variant`, `serialize_newtype_variant`, `serialize_tuple_variant`, `serialize_struct_variant`; decode `variant_seed` |
| `Vec<T>`, sequences | varint element **count**, then elements back to back | `serialize_seq`; decode `deserialize_seq` |
| `String`, `&str` | varint **byte** length, then UTF-8 bytes | `serialize_str`; decode rejects invalid UTF-8 |
| tuple, tuple struct, struct | fields in declaration order, concatenated; no length, no framing | `serialize_tuple`, `serialize_tuple_struct`, `serialize_struct` |
| newtype struct (`Point(u16)`) | transparent — the inner value's encoding | `serialize_newtype_struct` |
| unit, unit struct | zero bytes | `serialize_unit`, `serialize_unit_struct` |

Corroborated by postcard's `README.md` ("Variable Length Data"): *all signed and unsigned
integers larger than eight bits are encoded using a varint, including slice lengths and enum
discriminants.*

Deliberately unused by MRP/2, therefore unspecified here: `f32`/`f64` (MRP/2 quantises
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
* Maximum lengths: `u16` → 3 bytes, `u32` → 5, `u64` → 10 (`src/varint.rs` — `varint_max`).
* Encoders MUST emit the minimal byte count.
* Decoders MUST reject a varint running past its type's maximum length, and MUST reject a final
  byte whose value exceeds the bits the type has left — e.g. a five-byte `u32` ending in
  `> 0x0F` (`deserializer.rs` — `try_take_varint_u16/u32/u64`, `varint.rs` — `max_of_last_byte`;
  postcard's own `varint_boundary_canon` test pins `[FF FF FF FF 1F]` as invalid for `u32`).
* Decoders MAY accept a non-minimal encoding that still fits the budget — postcard's does, e.g.
  `80 00` reads as `u16` 0. Encoders MUST NOT depend on that.

**Length varints.** Sequence and string lengths use postcard's `usize` varint, whose decoder
width follows the decoding machine's pointer size. MRP/2 removes the portability question: no
frame may exceed `MAX_FRAME`, so no valid length exceeds 8388608, which is at most four varint
bytes and decodes identically everywhere. Implementations MUST NOT emit a longer length.

### 5.3 Type-to-encoding map

| MRP/2 value | Rust type | Wire encoding |
|---|---|---|
| protocol version | `u16` | varint (always 2 in v2) |
| subscription id | `u32` | varint — but **4 raw LE bytes** as the stream preamble ([§3](#3-stream-topology)) |
| session id, ping nonce | `u64` | varint |
| text | `String` | varint byte length + UTF-8 |
| board point | `Point(u16)` | varint; `index = y*w + x`, `65535` = pass |
| board size | `Size { w: u8, h: u8 }` | two raw bytes, each `2..=19` |
| colour | `Color` | varint variant: `0` Black, `1` White |
| ruleset | `RuleSet` | varint variant `0..=8` ([§7.5](#75-ruleset)) |
| requested extras | `Want` | **one raw byte** (custom `Serialize` calling `serialize_u8`) |
| komi | `i16` (`komi_x2`) | zigzag + varint |
| priority | `i8` | one raw byte |
| quantised probability / policy | `u16` | varint |
| quantised signed scalar | `i16` | zigzag + varint |
| ownership cell | `i8` | one raw byte |
| visit counts | `u32` | varint |
| optional field | `Option<T>` | `0x00` / `0x01` + value |
| list | `Vec<T>` | varint count + elements |
| pair | `(A, B)` | `A` then `B`, nothing else |

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
| **0** | `Hello` | `proto` | `u16` | Version the client speaks. MUST be 2. |
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
| **0** | `Welcome` | `proto` | `u16` | Version the server speaks. MUST be 2. |
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
| **0** | `BadVersion` | `bad-version` | `Hello.proto != 2`. |
| **1** | `Unauthorized` | `unauthorized` | `Hello.token` matches no configured token. |
| **2** | `NoSuchEngine` | `no-such-engine` | `Open.engine` names an unknown engine, or is `None` and no engine is configured. |
| **3** | `TooManySubs` | `too-many-subs` | The token's `max_subs` is already reached. |
| **4** | `BadRequest` | `bad-request` | First control message was not `Hello`; a second `Hello`; `Open.sub` duplicates a live id. |
| **5** | `EngineFailed` | `engine-failed` | Reserved. Not emitted by the reference server — engine failures arrive as `SubMsg::Failed` ([B.3](#appendix-b-findings)). |
| **6** | `Internal` | `internal` | Reserved. Not emitted by the reference server. |

A client MUST decode and handle all seven, including the two the reference server never sends.

---

## 7. Value types and quantisation

Declared in `mirai-proto/src/types.rs`; `Point`, `Size`, `Color` and `RuleSet` in
`mirai-core/src/point.rs` and `mirai-core/src/rules.rs`.

### 7.1 INV-1: point and array ordering

```
index = y * width + x            y = 0 is the TOP row, x = 0 the LEFT column

 19x19 board indices                    policy array
 x:   0    1    2  ...  18              [   0 .. 360 ]  board, same order
 y=0 [0]  [1]  [2]  ... [18]   TOP      [ 361 ]         pass
 y=1 [19] [20] [21] ... [37]
 y=18[342] ...          [360]  BOTTOM
```

| Rule | Level |
|---|---|
| A `Point` is a `u16` equal to `y*w + x`, with `y = 0` the **top** row. | MUST |
| `Point` `65535` (`Point::PASS`) means pass; no other out-of-board value may be sent. | MUST |
| `Report.ownership`, when present, has exactly `w*h` entries in that order. | MUST |
| `Report.policy`, when present, has exactly `w*h + 1` entries: the board, then **one pass slot last**. | MUST |
| Both board dimensions are in `2..=19` (`MIN_DIM`/`MAX_DIM`, KataGo's stock `MAX_LEN`); receivers reject anything else. | MUST |

This is KataGo's own ordering, which is the reason for choosing it: `ownership` and `policy`
index identically to the board array, so overlays need no remapping. Reversing it would
silently mirror every overlay. Pinned by `mirai-engine/src/decode.rs` —
`ownership_keeps_katago_row_major_top_left_order` and
`policy_has_a_pass_slot_and_marks_illegal_moves`.

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

| Field group | Codec, scale | Range | Resolution | Max round-trip error |
|---|---|---|---|---|
| winrate, prior | `q16` | `[0, 1]` | 1.526e-5 | 7.63e-6 |
| `lcb` | `qs`, 16384 | ±1.99994 | 6.1e-5 | 3.05e-5 |
| `utility`, `utility_lcb` | `qs`, 8192 | ±3.99988 | 1.22e-4 | 6.1e-5 |
| `score_lead`, `score_selfplay`, `raw_lead` | `qs`, 32 | ±1023.97 points | 0.03125 pt | 0.015625 pt |
| `score_stdev` | `qu`, 32 | `[0, 2047.97]` points | 0.03125 pt | 0.015625 pt |
| ownership cell | `q_own` | `[-1, 1]` | 0.00787 | 0.00394 |
| policy cell | `q_policy` | `[0, 1]` ∪ illegal | 1.526e-5 | 7.63e-6 |
| `raw_var_time_left` | `qu`, 4 | `[0, 16383.75]` | 0.25 | 0.125 |

Guaranteed tolerances, asserted by `mirai-proto/tests/wire_size.rs` —
`dequantisation_error_stays_inside_the_documented_tolerances`:
winrate ≤ 1e-4 (measured 7.644e-6) · score lead ≤ 0.02 points (measured 0.0125) ·
ownership ≤ 0.005 (measured 0.00394).

`utility_lcb` is the one field whose source routinely leaves that range. KataGo's lower
confidence bound subtracts `lcbStdevs * stdev / sqrt(ess)` from the utility — 3.5 at one
visit, and `2 * (winLoss + staticScore + dynamicScore) * lcbStdevs` = 14 for a child with no
visits at all — so a policy-tail candidate arrives clipped at −3.99988 (18 % of candidates on
a 42-position sweep at 5 000 visits, every one of them at one visit). This is not a defect to
fix by rescaling: below a handful of visits the bound carries no information. Clipping only
loosens it — a clipped value is still a lower bound — so a receiver MAY read one as
"unsearched", but MUST NOT read it as a magnitude.

### 7.2.1 INV-2: Black perspective

**Every winrate, score and utility on an MRP/2 wire is from Black's perspective.** There is no
per-message perspective flag and v2 MUST NOT add one.

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
| 12 | `report_every_ms` | `Option<u16>` | **milliseconds** | Intermediate-report interval; absent = only the terminal message. Forwarded as `reportDuringSearchEvery` in seconds. |
| 13 | `priority` | `i8` (1 raw byte) | | Higher runs first. A server MUST clamp it into `-8..=8` (`session.rs` — `PRIORITY_RANGE`) so one client cannot starve others. |
| 14 | `avoid` | `Vec<AvoidSpec>` | | Move restrictions ([§7.8](#78-avoidspec)). |
| 15 | `overrides` | `Vec<(String, String)>` | | Raw per-query KataGo `overrideSettings` entries. Servers SHOULD treat them as untrusted and MAY ignore them. |

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
| 3 | `0x08` | `MOVES_OWNERSHIP` | Requests per-move ownership from the engine. No MRP/2 field carries it ([B.2](#appendix-b-findings)). |
| 4 | `0x10` | `ROOT_RAW` | Nominally requests `RootInfo.raw_*`. Not consulted by the reference producer ([B.1](#appendix-b-findings)). |
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
| 15 | `pv_visits` | `Vec<u32>` | — | Per-ply visits along `pv`. Empty unless `PV_VISITS` was requested; when non-empty it SHOULD match `pv` in length ([B.8](#appendix-b-findings)). |

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
| 4 | `ownership` | `Option<Vec<i8>>` | Exactly `w*h` entries when present; any other length MUST be rejected. |
| 5 | `policy` | `Option<Vec<u16>>` | Exactly `w*h + 1` entries when present, pass last; any other length MUST be rejected. |

Once decoded, every report is a complete snapshot, never a delta: a client MAY drop an older
one the moment a newer arrives. The frames underneath are not independent
([§4.1](#41-subscription-streams-one-zstd-stream)): the client still decodes every one.

---

## 8. Session state machine

### 8.1 Handshake

```
client                                                            server
  | QUIC Initial, ALPN "mirai/2", TLS 1.3; leaf fingerprint pinned    |
  |----------------------------------------------------------------->|
  | open bidirectional stream; frame ClientMsg::Hello{proto,token,..} |
  |----------------------------------------------------------------->|
  |                                    frame ServerMsg::Welcome{...}  |
  |<-----------------------------------------------------------------|
```

| # | Rule | Level |
|---|---|---|
| 1 | Complete the QUIC/TLS handshake with ALPN `mirai/2` and verify the leaf fingerprint before sending any frame. | MUST |
| 2 | Open exactly one bidirectional stream and send `Hello` as its first frame. | MUST |
| 3 | (Server) Treat any first control message other than `Hello` as fatal: `Error { None, BadRequest }`, then close the connection with application code 1. | MUST |
| 4 | (Server) Send nothing before receiving `Hello`, except an `Error`. | MUST NOT |
| 5 | (Client) Ignore, rather than fail on, any other message arriving before `Welcome`. | SHOULD |

**Version negotiation** is two-layered and both layers are REQUIRED: ALPN `mirai/2` fails the
handshake between incompatible major versions, and the `Hello`/`Welcome` `proto` check catches
a peer that offered `mirai/2` without implementing it. A server MUST reject `Hello.proto != 2`
with `BadVersion` and close; a client MUST treat `Welcome.proto != 2` as unusable and MUST NOT
send `Open`. There is no other feature negotiation — capabilities MUST NOT be inferred from the
`client`/`server` identification strings.

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
| The token is sent nowhere but inside `Hello` on an established TLS 1.3 connection. | MUST NOT (otherwise) |

### 8.3 Subscription lifecycle

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Requested: ClientMsg::Open
    Requested --> Rejected: ServerMsg::Error Some(sub)
    Requested --> Active: ServerMsg::Opened
    Active --> Active: SubMsg::Report
    Active --> Done: SubMsg::Done
    Active --> FailedFin: SubMsg::Failed
    Active --> Cancelled: ClientMsg::Cancel
    Active --> FailedLost: connection lost
    Rejected --> [*]
    Done --> [*]
    FailedFin --> [*]
    Cancelled --> [*]
    FailedLost --> [*]

    Rejected: REJECTED
    Done: DONE — stream FIN
    FailedFin: FAILED — stream FIN
    Cancelled: CANCELLED — stream RESET
    FailedLost: FAILED — never replayed
```

All terminal states are final: the id is retired and the stream closed.

| Rule | Level |
|---|---|
| `sub` is client-chosen and unique among the connection's live subscriptions. The reference client counts up from 1 and never reuses an id within a connection. | MUST |
| (Server) Reject a duplicate live id with `Error { Some(sub), BadRequest }`, and reap finished subscriptions before applying that and the `max_subs` check. | MUST |
| `Opened` and the subscription stream are independent; neither side may require one before the other. | MUST NOT |
| (Server) End a subscription with exactly one `Done` or `Failed`, then **finish** (not reset) the stream. | MUST |
| (Client) Treat a subscription stream that reaches clean EOF without `Done`/`Failed` as failed — never as success, never waiting indefinitely. | MUST |

### 8.4 Cancellation (INV-3)

Cancellation must *discard* in-flight reports, so it is propagated on both streams.

| Actor | Required actions, in order |
|---|---|
| **Client** | 1. `STOP_SENDING` on that subscription's stream with application code 1 — this drops reports already buffered in the receive window instead of decoding and delivering them. 2. Send `ClientMsg::Cancel { sub }` on the control stream. 3. Retire the id and ignore anything further for it. |
| **Server** | Stop the search and **reset** the subscription stream with application code 1 (`session.rs` — `pump`, `CODE_CANCELLED`). Resetting is REQUIRED: finishing would deliver exactly the stale reports the client just refused. Cancelling an unknown id is not an error and MUST be ignored silently. |

A client MAY send only one of the two and a server MUST cope, but doing both is RECOMMENDED:
`STOP_SENDING` acts in the transport immediately, while `Cancel` waits on the peer's control
loop. Connection loss cancels everything implicitly.

### 8.5 Other control requests

| Request | Response | Notes |
|---|---|---|
| `Ping(n)` | `Pong(n)` | `n` echoed unchanged. Unsolicited `Pong` MUST NOT be sent. QUIC keep-alive already covers transport liveness. |
| `ListEngines` | `Engines(..)` | Current engine list; MAY differ from `Welcome.engines`. |
| second `Hello` | `Error { None, BadRequest, "already greeted" }` | Non-fatal; the connection continues. |

### 8.6 Connection loss and shutdown

| Rule | Level |
|---|---|
| On connection loss both endpoints treat every live subscription on it as failed. | MUST |
| Subscriptions are **never** replayed, resumed or reissued after a reconnect; a new connection starts empty. Reissuing is the application's decision, based on what the user is looking at now. | MUST NOT |
| (Server) Stop the underlying searches when the connection goes away. Every exit path of the reference server's `pump` drops the engine subscription, which terminates the KataGo query. | MUST |
| (Client) Reconnecting automatically is optional; capped exponential backoff is RECOMMENDED (reference: 0.5 s, 1 s, 2 s, 4 s, then 8 s forever — `remote.rs` — `backoff`). | MAY / SHOULD |
| (Client) Never treat a fingerprint mismatch, `BadVersion` or `Unauthorized` as transient. Background retries are allowed, but the session MUST NOT be reported as healthy (`remote.rs` — `RemoteStatus::Failed`). | MUST |

Rationale: analysis requests are cheap to reissue and expensive to run. Replaying a queue of
stale positions after a five-second outage burns search threads on positions nobody is
watching.

| Shutdown path | Action |
|---|---|
| Client finished | close the QUIC connection, application code **0**, reason `bye`. |
| Server, normal end or control-stream EOF | close, application code **0**, reason `bye`. |
| Server, rejecting a handshake | write the `Error` frame, **flush and finish the control stream and wait for the peer's acknowledgement**, then close with application code **1** and the `ErrCode::as_str()` name as the reason. Flushing first is REQUIRED — closing immediately would discard the explanation. |

---

## 9. Errors and limits

### 9.1 Scope and fatality

| `ErrCode` | `sub` | Trigger | Fatal to |
|---|---|---|---|
| `BadRequest` | `None` | first control message was not `Hello` | **connection** (server closes, code 1) |
| `BadVersion` | `None` | `Hello.proto != 2` | **connection** (code 1) |
| `Unauthorized` | `None` | token not recognised | **connection** (code 1) |
| `BadRequest` | `None` | second `Hello` | nothing |
| `BadRequest` | `Some(sub)` | `Open.sub` already live | that subscription |
| `TooManySubs` | `Some(sub)` | token's `max_subs` reached | that subscription |
| `NoSuchEngine` | `Some(sub)` | unknown engine, or `None` with none configured | that subscription |
| `SubMsg::Failed` | sub stream | the search itself failed | that subscription |

Client rules: fail exactly the named subscription on `Error { Some(sub) }` and keep the
connection · on `Error { None }` do not tear down subscriptions unilaterally; surface it and
wait to see whether the server closes · ignore an `Error` naming an unknown or already-terminal
subscription · treat `Error` as advisory *in addition to*, never *instead of*, the transport
signal — a server MAY close without sending one.

### 9.2 QUIC application codes

| Where | Code | Meaning |
|---|---|---|
| connection close, either side | 0 (`bye`) | orderly shutdown |
| connection close, server | 1 | handshake rejected — see the `Error` frame sent just before |
| `RESET_STREAM` on a subscription stream (server) | 1 | subscription cancelled |
| `STOP_SENDING` on a subscription stream (client) | 1 | subscription cancelled |

No other code carries meaning beyond "the peer is gone".

### 9.3 Limits

| Limit | Value | Enforcement |
|---|---|---|
| Frame payload | ≤ `MAX_FRAME` (8 MiB) | sender MUST NOT exceed; receiver MUST reject before allocating |
| Decompressed payload | ≤ `MAX_FRAME` | receiver MUST abort decompression past the limit |
| Concurrent subscriptions | per-token `max_subs`, default 4 | server MUST refuse further `Open` with `TooManySubs` |
| Server-opened uni streams | client's `max_concurrent_uni_streams` | QUIC flow control; client MUST advertise ≥ its intended subscription count |
| `AnalyzeReq.priority` | clamped to `-8..=8` | server MUST clamp, not reject |
| Board size | `2..=19` per dimension | receiver MUST reject anything else |
| Control streams | exactly 1 bidirectional | client MUST NOT open more |

`max_subs` is counted **per connection** in the reference server, not summed per token across
connections ([B.5](#appendix-b-findings)); a client MUST NOT rely on
either interpretation.

---

## 10. Versioning and compatibility

### 10.1 What a v2 implementation MUST reject

Postcard is positional: one misread byte desynchronises the rest of the frame, so leniency here
produces silently wrong analysis rather than a clean failure.

| # | Condition |
|---|---|
| 1 | ALPN other than `mirai/2`; TLS below 1.3. |
| 2 | A leaf certificate whose SHA-256 differs from the stored pin. |
| 3 | `Hello.proto != 2` (server) or `Welcome.proto != 2` (client). |
| 4 | A first control message that is not `Hello`. |
| 5 | Frame `len > MAX_FRAME`, before reading the body; a zstd payload whose plaintext would exceed `MAX_FRAME`; a frame truncated by end of stream. |
| 6 | An enum discriminant out of range: `ClientMsg` 0–4, `ServerMsg` 0–4, `SubMsg` 0–2, `ErrCode` 0–6, `Color` 0–1, `RuleSet` 0–8. |
| 7 | An `Option` tag or `bool` byte other than `0x00`/`0x01`. |
| 8 | A varint longer than its type's maximum, or whose final byte overflows the type. |
| 9 | A `String` that is not valid UTF-8. |
| 10 | Trailing bytes after a fully decoded message within one frame. |
| 11 | `Size` outside `2..=19`; `ownership` not `w*h` long; `policy` not `w*h + 1` long. |
| 12 | A `flags` value the stream does not allow ([§4](#4-framing)); a subscription stream whose zstd window exceeds 2^16. |

A v2 implementation MUST NOT reject: unknown `Want` bits (ignore them), unknown `overrides`
keys, an empty `Welcome.engines`, or a `pv_visits` shorter than `pv`.

### 10.2 Rules for a future v3

With no field names, counts or type tags on the wire, **nothing may be added to an existing
struct or enum in a way a v2 peer could encounter.**

| Change | Compatible within v2? |
|---|---|
| New value in an existing string field | **Yes** |
| New `Want` bit | **Yes** — receivers truncate unknown bits, and the bit only ever requests optional data |
| New key in `AnalyzeReq.overrides` | **Yes** — an opaque string map |
| New field appended to any struct | **No** — the decoder stops early, then rejects trailing bytes or mis-parses the next field |
| New variant appended to any enum, including `ErrCode` | **No** — v2 rejects the unknown discriminant. `EngineFailed` and `Internal` already exist for engine-side and unclassified failures |
| Any change to a scale constant, `POLICY_ILLEGAL`, or a quantisation formula | **No** — silently wrong numbers, the worst failure mode |
| Field reordering | **No** |
| New framing flag bit | **No** — receivers reject unknown flag bits ([B.7](#appendix-b-findings)), so a new bit needs a new version. Reserved bits stay zero for the life of v2 |

A v3 MUST: use ALPN `mirai/3` so incompatible peers fail at the TLS handshake (a dual-stack
server SHOULD offer both and behave per the negotiated one) · set `PROTO_VERSION = 3` so a
wrong-ALPN peer is still caught · keep the 5-byte frame header byte-identical so a version
mismatch can still be reported with a readable `Error` · append new enum variants only at the
end and never renumber or repurpose a discriminant, so v3 tooling can still read v2 captures ·
preserve INV-1, INV-2 and INV-4, which are meaning, not encoding.

### 10.3 Changes from MRP/1

MRP/2 replaced MRP/1 outright; the reference endpoints speak only v2, so a v1 peer fails at
the TLS handshake.

| # | Change |
|---|---|
| 1 | ALPN `mirai/2`, `PROTO_VERSION = 2`. |
| 2 | `AnalyzeReq.max_candidates` ([§7.3](#73-analyzereq)): a client asks for only the candidates it shows. |
| 3 | A subscription stream is one zstd stream, flags `0x02` ([§4.1](#41-subscription-streams-one-zstd-stream)); a report is compressed against those before it. |
| 4 | Trailing bytes after a message are rejected. MRP/1 already required it, but its reference implementation ignored them: `postcard::from_bytes` does not check. |

---

## 11. Reference figures

Measured by `mirai-proto/tests/wire_size.rs`.

| Message | Contents | postcard payload | Compressed | **Framed** |
|---|---|---|---|---|
| `SubMsg::Report` | 19×19, 50 candidates, 15-move PVs with `pv_visits`, 361-entry ownership, no policy; first frame of a subscription stream | 4128 B | yes → 2605 B | **2610 B** |
| `ClientMsg::Open` | 19×19, 200 moves, `want = OWNERSHIP\|PV_VISITS`, `max_visits`, `report_every_ms` | 496 B | no | **501 B** |
| `SubMsg::Report` | 2 candidates, 2-move PVs, no ownership or policy ([Appendix A](#appendix-a-worked-exchange)) | 77 B | no | **82 B** |
| `ClientMsg::Ping` | one `u64` | 2 B | no | **7 B** |

What to expect from those numbers:

* A live 19×19 analysis at `report_every_ms = 100` costs roughly **26 KB/s** per subscription.
  The equivalent KataGo JSON is put at ~45 KB per report by the `mirai-proto` crate docs
  (~17× larger); that figure is a project claim, not measured by this specification.
* Ownership dominates a report: `w*h` raw bytes before compression. `POLICY` adds `w*h + 1`
  varints of 1–3 bytes each.
* A report crosses the 4096-byte compression threshold at roughly 50 candidates with full PVs
  plus ownership; smaller reports go out uncompressed, so an implementation that never
  compresses interoperates and is only larger on the biggest frames.
* `MAX_FRAME` is ~2000× a full report. It is a safety ceiling, not a working budget.

Verified end to end against KataGo 1.16.4, locally and through `mirai-server`; a client SIGINT
makes the server drop the subscription in under 1 ms and KataGo falls to 0 % CPU.

---

## 12. Conformance checklist

### MUST

1. Negotiate ALPN `mirai/2` over QUIC with TLS 1.3 only.
2. Pin the server leaf certificate by lowercase-hex SHA-256 of its DER; refuse on mismatch; do
   no chain, hostname or expiry validation; still verify the handshake signature.
3. Use one client-opened bidirectional control stream, with `Hello` as its first frame.
4. Prefix each server-opened unidirectional stream with the 4-byte little-endian subscription
   id, outside the framing.
5. Frame every message as `[len: u32 LE][flags: u8][payload]`.
6. Reject `len > MAX_FRAME` before allocating or reading the body.
7. Bound decompressed payloads to `MAX_FRAME` and abort decompression that would exceed it.
   Refuse a subscription stream whose zstd window exceeds 2^16.
8. Encode payloads as postcard v1 in exactly the field order of §6 and §7.
9. Encode `u16`/`u32`/`u64`, all lengths and all discriminants as unsigned LEB128 varints;
   `u8`/`i8` as one raw byte; `i16` as zigzag-then-varint; `bool` as one `0x00`/`0x01` byte.
10. Encode `Option` as `0x00`, or `0x01` + value, and reject any other tag.
11. Reject unknown discriminants, malformed varints, invalid UTF-8, trailing bytes, and a
    `flags` value the stream does not allow.
12. Use `index = y*w + x` with `y = 0` at the top, and `65535` for pass.
13. Send `ownership` with `w*h` entries and `policy` with `w*h + 1`, pass last, `65535` illegal.
14. Treat every winrate, score and utility as Black-perspective, converting only at display
    time.
15. Apply the quantisation formulas and scale constants of §7.2 exactly.
16. Carry komi as `komi_x2`, and the whole position in every request.
17. Check `proto == 2` in `Hello`/`Welcome` and fail the session on mismatch.
18. (Server) Compare tokens in constant time against every configured token.
19. (Server) Clamp `priority` into `-8..=8`; enforce `max_subs` with `TooManySubs`; reject a
    duplicate live `sub` with `BadRequest`.
20. (Server) Send exactly one `Done` or `Failed` per subscription, then finish the stream.
21. (Server) On `Cancel`, reset the subscription stream rather than finishing it.
22. (Client) On cancel, send `STOP_SENDING` and `Cancel`, then retire the id.
23. Fail every live subscription on connection loss, and never replay one across a reconnect.
24. Treat a subscription stream that reaches EOF without a terminal message as failed.
25. (Server) Flush the `Error` frame before closing a rejected connection.
26. Decode and handle all seven `ErrCode` values.
27. Decode every frame of a subscription stream, in order.

### SHOULD

28. Control stream: compress payloads above 4096 bytes with zstd and set `flags = 0x01`.
    Subscription streams: send every frame as `0x02`.
29. Advertise `max_concurrent_uni_streams` well above the intended subscription count.
30. Send QUIC keep-alives at roughly ⅓ of the negotiated idle timeout.
31. Normalise a user-supplied fingerprint (trim, lowercase, strip `:`) before comparing, and
    show a first-seen fingerprint to the user before persisting it.
32. Use ≥ 128 bits of entropy per token.
33. Reconnect with capped exponential backoff, distinguishing "reconnecting" from "permanently
    rejected" in anything the user sees.
34. Sort `Report.moves` ascending by `order` before sending.
35. Set `report_every_ms` only when a live view is wanted, so the server does not serialise
    reports nobody reads.

---

## Appendix A: worked exchange

Every byte below was produced by an independent encoder written from this specification; the
`Open` and `Report` sizes reproduce the reference implementation's measured frame sizes
exactly. `|` marks the header/payload boundary for readability only.

**0 — connect.** UDP to `box.local:9678`, QUIC + ALPN `mirai/2` + TLS 1.3. Hash the leaf DER
with SHA-256, render 64 lowercase hex characters, compare with the stored pin. No pin stored ⇒
record and ask the user; mismatch ⇒ abort before any frame is sent.

**1 — `Hello { proto: 1, token: "t0k", client: "demo/1" }`**

```
0d 00 00 00 | 00 | 00 01 03 74 30 6b 06 64 65 6d 6f 2f 31
len=13       flags  ^  ^  ^  "t0k"  ^  "demo/1"
                    |  |  len 3     len 6
                    |  proto = 1
                    variant 0 = Hello
```

**2 — `Welcome`** with `server: "mirai-server/0.1.0"`, `session: 1`, one engine
`{ "default", "1.16.4", "b18c384nbt", 4 threads, 19×19, no human model }`

```
35 00 00 00 | 00 | 00 01 12 6d 69 72 61 69 2d 73 65 72 76 65 72 2f 30 2e 31 2e 30
                   01 01 07 64 65 66 61 75 6c 74 06 31 2e 31 36 2e 34
                   0a 62 31 38 63 33 38 34 6e 62 74 04 13 13 00

00 variant 0 = Welcome · 01 proto · 12 "mirai-server/0.1.0" (len 18) · 01 session = 1
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
(Black), two candidates with 2-move PVs, no ownership yet, no policy.

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

---

## Appendix B: findings

Raised while writing this specification. B.1, B.2, B.4 and B.7 were fixed in the reference
implementation as a result and are recorded here only so the reasoning survives; the rest are
open. None blocks an interoperable implementation, and none corrupts a session or a result.

| # | Finding | Status | What an implementer should do |
|---|---|---|---|
| B.1 | `Want::ROOT_RAW` was documented as gating `RootInfo`'s `raw_*` fields, but nothing consulted it — KataGo has no switch for those values | **fixed** (documented as advisory) | Treat the raw fields as "present when the engine supplied them". Set the bit for forward compatibility; never rely on it to suppress them. |
| B.2 | `Want::MOVES_OWNERSHIP` was forwarded as KataGo's `includeMovesOwnership`, but `MoveInfo` has no field for the result, so it was decoded and discarded | **fixed** (flag reserved; no longer requested) | Leave unset. The bit is reserved so a later version can add the field without reusing it. |
| B.3 | `EngineFailed` and `Internal` are never sent by the reference server; engine failures arrive as `SubMsg::Failed(String)`, losing the machine-readable code | open | Decode both anyway — the discriminants are part of the protocol and another server may use them. |
| B.4 | `RAW_VAR_TIME_SCALE` was defined in `mirai-engine`, so `mirai-proto` alone was not enough to decode `raw_var_time_left` | **fixed** (moved beside the other scales in `types.rs`) | — |
| B.5 | `max_subs` is enforced per connection, not per token, so one token on two connections gets twice its quota | open | A server MAY enforce it globally; a client must rely on neither. |
| B.6 | The reference server accepts one bidirectional stream and never looks for another, so a second stalls rather than erroring. [INFERENCE] from the control flow; untested | open | Never open a second bidirectional stream. A stricter server would close the connection. |
| B.7 | Unknown framing flag bits were ignored, so `flags = 0x02` parsed as uncompressed postcard | **fixed** (unknown bits are now a hard error) | Keep reserved bits zero; expect a peer to reject anything else ([§10.2](#102-rules-for-a-future-v3)). |
| B.8 | Nothing validates a non-empty `MoveInfo.pv_visits` against the length of `pv` | open | Index defensively; do not assume the two are the same length. |

Everything else read for this specification — framing, quantisation, the handshake, the
subscription state machine, cancellation, and both endpoint implementations — behaves as
specified above.
