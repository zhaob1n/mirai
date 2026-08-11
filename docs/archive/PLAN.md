> **Archived.** This is the original build plan, written before any code existed, and it is
> kept only as a historical record. It is not maintained and it is not authoritative: where it
> disagrees with the tree, the tree is right. At least one acceptance criterion here is known
> to be wrong (the empty-board win rate is ~35.6% for the bundled network, not 45–55%). For
> current design, read [`../dev/ARCHITECTURE.md`](../dev/ARCHITECTURE.md); for the wire format,
> [`../dev/PROTOCOL.md`](../dev/PROTOCOL.md).

# mirai — KataGo GUI + remote KataGo protocol

## Context

Build `mirai` from scratch in `/home/ykpcx/probe/mirai` (currently empty): a Rust/GTK4/libadwaita desktop
application that (a) drives a **local** KataGo process directly, (b) drives a **remote** KataGo over a new
purpose-built binary protocol with a server and client implementation, and (c) supports both **analysis/review**
and **playing against KataGo**, at feature parity with the essentials of LizzieYzy Next
(`/home/ykpcx/probe/lizzieyzy-next`).

End state: `cargo run -p mirai` opens an Adwaita window that analyses a position with the bundled KataGo, plays a
full game against it, loads/saves SGF, and can be pointed at `mirai-server` on another machine and behave
identically.

Verified environment (this session): rustc 1.96.0-nightly, GTK 4.22.4, libadwaita 1.9.3, KataGo v1.16.4 OpenCL at
`/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/linux-x64/katago`, model
`/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/weights/default.bin.gz`, analysis config
`/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/configs/analysis.cfg`.

---

## Foundational decisions (apply everywhere)

These are settled; do not revisit them while implementing.

**D1 — One engine interface: the KataGo JSON analysis engine.** `katago analysis`, never `katago gtp`.
Verified in `/home/ykpcx/KataGo/cpp/command/analysis.cpp` and `docs/Analysis_Engine.md`:
queries are stateless (each carries `moves`/`initialStones`/`rules`/`komi`), fully asynchronous, support
`priority`, `terminate`/`terminate_all`, `reportDuringSearchEvery` streaming, and `numAnalysisThreads` concurrent
positions. This removes LizzieYzy's entire class of engine-state-desync bugs (`Leelaz.java` replay/undo/lease
machinery, 12 487 lines) and makes the remote protocol a pure request/stream forwarder.
Playing moves is done by us: run an analysis query, then select from `moveInfos[].playSelectionValue`
(verified emitted at `cpp/search/searchresults.cpp:2066`).

**D2 — Point encoding is KataGo's.** `Point(u16)` where `index = y * width + x`, **y = 0 is the TOP row**.
This is byte-identical to KataGo's `ownership`/`policy` array order (`docs/Analysis_Engine.md:260,280,282`), so
overlays need zero index remapping. `PASS = u16::MAX`. Do **not** copy LizzieYzy's column-major
`x * height + y` (`rules/Board.java getIndex`).

**D3 — All engine values are Black-perspective.** Launch KataGo with `reportAnalysisWinratesAs=BLACK`
(present in the bundled `analysis.cfg:30`). Stored node stats are therefore sign-stable; the UI converts to
side-to-move where it displays candidate moves (`wr_stm = if black_to_move { wr } else { 1.0 - wr }`, and
`lead_stm = if black_to_move { lead } else { -lead }`).

**D4 — Wire values are quantised integers, not floats/JSON.** See "MRP wire types". A live report is ~2.5 KB
instead of ~45 KB of KataGo JSON.

**D5 — Async boundary.** Engine drivers run on one shared multi-thread `tokio` runtime owned by the GUI process
(or by `mirai-server`). A subscription delivers into `tokio::sync::watch::Sender<SubEvent>`; the GUI consumes with
`glib::spawn_future_local` (verified `glib 0.22.8`, feature `futures`). `watch` collapses superseded partial
reports automatically — never build a coalescing queue.

**D6 — Rendering.** The board, winrate graph and move tree are custom `gtk::Widget` subclasses implementing
`WidgetImpl::snapshot(&self, snapshot: &gtk::Snapshot)` (verified in `gtk4 0.11.4`). Geometry uses
`gsk::PathBuilder` + `Snapshot::append_fill`/`append_stroke`; text uses `Snapshot::append_layout`; the ownership
heat map is one `gdk::MemoryTextureBuilder` texture of `w*h` RGBA8 pixels drawn scaled. No `gtk::DrawingArea`,
no cairo, anywhere.

**D7 — Crate versions.** Pin these exact latest releases (checked against crates.io this session) in
`[workspace.dependencies]`:
`gtk4 = { version = "0.11.4", features = ["v4_22"] }`, `libadwaita = { version = "0.9.2", features = ["v1_9"] }`,
`glib = { version = "0.22.8", features = ["futures"] }`, `glib-build-tools 0.22.8` (build-dep),
`tokio 1.53.1` (features `rt-multi-thread`, `macros`, `process`, `io-util`, `sync`, `time`),
`quinn 0.11.11` (default features — they already select `rustls-ring` + `runtime-tokio`), `rustls 0.23.43`,
`rcgen 0.14.8`, `subtle 2.6.1`, `serde 1.0.229` (feature `derive`), `serde_json 1.0.151`,
`postcard 1.1.3` (feature `use-std`), `bitflags 2.13.1` (feature `serde`), `zstd 0.13.3`, `base64 0.23.1`,
`dashmap 6.2.1`, `arrayvec 0.7.8`, `smallvec 1.15.2` (feature `union`), `bytes 1.12.1`,
`thiserror 2.0.20`, `anyhow 1.0.104`, `tracing 0.1.44` + `tracing-subscriber 0.3` (feature `env-filter`),
`clap 4.6.6` (feature `derive`), `toml 1.1.4`, `directories 6.0.0`, `encoding_rs 0.8.35`.
`graphene` and `gsk` are used through the `gtk4::graphene` / `gtk4::gsk` re-exports — no direct dependency.

**D8 — Scope line.** Implement exactly Steps 1–13 below. Explicitly **not** built (do not add them even if
they look easy while nearby): Fox/Tygem game fetching, screen-board OCR (`readboard_java`), the joseki/teacher
knowledge modules, KataGo auto-download/benchmark/tuning wizards, theme skinning, dual-engine (`bestMoves2`)
comparison, Zhizi cloud accounts.

---

## Approach

### Step 1 — Workspace skeleton

Create a Cargo workspace at the repo root with `resolver = "3"` and `[workspace.package] edition = "2024"`,
`rust-version = "1.92"`. Put every dependency from **D7** in `[workspace.dependencies]`; member crates use
`dep.workspace = true`. Members, in dependency order:

| crate | kind | depends on | contains |
|---|---|---|---|
| `mirai-core` | lib | serde, thiserror, smallvec | points, board, rules, scoring, game tree, SGF, time control |
| `mirai-proto` | lib | mirai-core, serde, postcard, zstd, quinn, rustls, tokio, bytes, thiserror | MRP types, frame codec, QUIC client+server transport |
| `mirai-engine` | lib | mirai-core, mirai-proto, serde_json, tokio, tracing | `Engine` trait, `LocalEngine`, `RemoteEngine` |
| `mirai-server` | bin `mirai-server` | mirai-engine, mirai-proto, clap, toml, rcgen, subtle, tracing-subscriber | headless remote engine host |
| `mirai` | bin `mirai` | mirai-engine, mirai-core, gtk4, libadwaita, glib, tokio, toml, directories | the GTK application |

`mirai-engine` depends on `mirai-proto` because `RemoteEngine` lives there; `mirai-server` depends on
`mirai-engine` for `LocalEngine`. No cycles.

After this step `cargo check --workspace` passes with empty `lib.rs`/`main.rs` files.

### Step 2 — `mirai-core`: geometry, board, rules

Independent of every other step; everything else depends on it.

`src/point.rs`
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Point(pub u16);              // y * width + x, y = 0 is TOP.  PASS == u16::MAX
impl Point { pub const PASS: Point = Point(u16::MAX); }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size { pub w: u8, pub h: u8 } // 2..=19 both dims; reject others in ::new

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color { Black, White }
impl Color { pub fn other(self) -> Color; pub fn katago(self) -> &'static str; } // "B" / "W"
```
Conversions on `Size` (all `#[inline]`, no allocation on the hot paths):
* `xy(p) -> (u8, u8)`, `point(x, y) -> Point`.
* `to_gtp(p) -> ArrayString<4>`: column letters `"ABCDEFGHJKLMNOPQRSTUVWXYZ"` (no `I`), row number
  `h - y` (so `Point(0)` on 19×19 is `A19`); `Point::PASS` → `"pass"`.
* `from_gtp(&str) -> Option<Point>`, case-insensitive, accepts `"pass"`.
* `to_sgf(p) -> [u8; 2]` / `from_sgf(&[u8]) -> Option<Point>`: `b'a' + x`, `b'a' + y` (top-left origin, so it is a
  direct mapping — no flip); empty value and `"tt"` on boards ≤ 19 mean pass.

`src/rules.rs` — mirror KataGo's own definitions so local legality never disagrees with the engine. Verified
against `/home/ykpcx/KataGo/cpp/game/rules.cpp:273-372`:
```rust
pub enum Ko { Simple, Positional, Situational }
pub enum Scoring { Area, Territory }
pub enum Tax { None, Seki, All }
pub enum Whb { Zero, N, NMinusOne }
pub struct Rules { pub ko: Ko, pub scoring: Scoring, pub tax: Tax,
                   pub multi_stone_suicide: bool, pub has_button: bool,
                   pub whb: Whb, pub friendly_pass_ok: bool }
pub enum RuleSet { TrompTaylor, Chinese, ChineseOgs, Japanese, Korean, StoneScoring, Aga, AgaButton, NewZealand }
```
`RuleSet::katago_name()` returns exactly: `"tromp-taylor"`, `"chinese"`, `"chinese-ogs"`, `"japanese"`,
`"korean"`, `"stone-scoring"`, `"aga"`, `"aga-button"`, `"new-zealand"`.
`RuleSet::rules()` and `RuleSet::default_komi()` return, in order (ko, scoring, tax, suicide, button, whb, pass, komi):
* TrompTaylor: Positional, Area, None, true, false, Zero, false, 7.5
* Chinese: Simple, Area, None, false, false, N, true, 7.5
* ChineseOgs: Positional, Area, None, false, false, N, true, 7.5
* Japanese / Korean: Simple, Territory, Seki, false, false, Zero, false, 6.5
* StoneScoring: Simple, Area, All, false, false, Zero, true, 7.5
* Aga: Situational, Area, None, false, false, NMinusOne, true, 7.5
* AgaButton: Situational, Area, None, false, true, NMinusOne, true, 7.0
* NewZealand: Situational, Area, None, true, false, Zero, true, 7.0

`src/board.rs` — `Board { size: Size, stones: Box<[Option<Color>]>, captures: [u16; 2], zobrist: u64, ko_ban: Option<Point> }`.
* Zobrist table: `OnceLock<[[u64; 361]; 2]>` filled from a fixed seed (`SplitMix64` seeded `0x9E37_79B9_7F4A_7C15`)
  so hashes are reproducible across runs and machines. Situational superko mixes in one extra constant when the
  side to move is White.
* `play(color, point, rules) -> Result<Captured, IllegalMove>` implemented with an explicit stack flood fill over a
  reusable `Vec<u16>` scratch buffer held in the `Board` (no per-move allocation): place stone, remove adjacent
  enemy chains with zero liberties, then if the played chain has zero liberties reject unless
  `rules.multi_stone_suicide` and the chain size > 1 (single-stone suicide is always illegal in KataGo's board).
* `IllegalMove { Occupied, Suicide, Ko, OffBoard }`.
* Superko is enforced by the *game tree* (Step 3), which owns the hash history; `Board` only enforces simple ko
  via `ko_ban`.

`src/score.rs` — `fn score(board, rules, komi, handicap_stones, dead: &DeadSet) -> ScoreResult`.
* Dead-stone default derivation: `DeadSet::from_ownership(board, ownership: &[f32], threshold = 0.4)` marks a
  stone dead when `ownership[p]` has the opposite sign to its colour **and** `ownership[p].abs() >= threshold`;
  the whole chain is marked if a majority of its stones qualify (avoids speckled results).
* Area scoring: alive stones + points reachable only by one colour after removing dead stones; White adds komi;
  `Whb::N` adds `handicap_stones` to White, `Whb::NMinusOne` adds `handicap_stones - 1` (0 if no handicap).
* Territory scoring: territory − own dead stones − opponent captures, White adds komi; `Tax::Seki` and `Tax::All`
  subtract one point per eye-space group as KataGo does — implement `Tax::None` and `Tax::Seki` only and return
  `ScoreResult::approximate = true` for `Tax::All`; the UI labels it "estimated".
* `ScoreResult { black: f32, white: f32, territory: Box<[Option<Color>]> }`.

`src/handicap.rs` — `fixed_handicap(size, n) -> SmallVec<[Point; 9]>` for square boards 7×7/9×9/13×13/19×19,
n = 2..=9, star points at `(3, 3)`-style offsets (offset 3 for ≥ 13, offset 2 for < 13), following the standard
corner→corner→corner→corner→sides→tengen order that LizzieYzy's `Board.fixedHandicapPoints` uses.

### Step 3 — `mirai-core`: game tree + SGF (depends on Step 2)

`src/tree.rs` — arena tree, index-based, no `Rc`:
```rust
pub struct NodeId(u32);
pub struct GameTree { nodes: Vec<Node>, root: NodeId, pub info: GameInfo }
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: SmallVec<[NodeId; 2]>,   // children[0] is the main line
    pub mv: Option<(Color, Point)>,        // None on root/setup nodes; Point::PASS for a pass
    pub setup: Setup,                      // AB / AW / AE
    pub marks: Marks,                      // LB / TR / SQ / CR / MA
    pub comment: String,
    pub move_number_override: Option<u16>, // MN
    pub analysis: Option<NodeAnalysis>,
    pub unknown_props: Vec<(Box<str>, Vec<Box<str>>)>, // preserved verbatim on save
}
pub struct NodeAnalysis {           // Black perspective (D3)
    pub visits: u32, pub winrate: f32, pub score_lead: f32, pub score_stdev: f32,
    pub candidates: Vec<Candidate>, // decoded MRP MoveInfo, already dequantised
    pub ownership: Option<Box<[i8]>>,
}
```
* `GameInfo { size, rules: RuleSet, komi: f32, handicap: u8, result: String, players: [PlayerInfo; 2], date, event, time_limit, overtime }`.
* A cached `Position { board: Board, to_play: Color, hash_history: Vec<u64>, move_number: u16 }` is recomputed by
  replaying from the root when the cursor moves; cache the last computed `(NodeId, Position)` so sequential
  navigation is O(1). Superko: a move is illegal if the resulting `zobrist` is already in `hash_history`
  (positional) or in the subset with the same side to move (situational).
* Editing ops: `play(node, color, point)` (reuses an existing child with the same move instead of branching),
  `add_variation`, `delete_branch`, `promote_to_main_line`, `set_comment`, `toggle_mark`, `set_setup_stone`.

`src/sgf.rs` — hand-written parser (no external SGF crate: `sgf-parse` 4.2.8 has 47 k downloads and does not
preserve unknown properties, which we require).
* Read: `SZ` (`19` or `19:13`), `KM` (with LizzieYzy's normalisation — a value ≥ 200 is divided by 100, for
  Chinese quarter-point files), `HA`, `RE`, `PL`, `GN`, `PB`, `PW`, `BR`, `WR`, `DT`, `TM`, `OT`, `CA`, `AP`,
  `B`, `W`, `AB`, `AW`, `AE`, `MN`, `C`, `LB`, `TR`, `SQ`, `CR`, `MA`. Every other property lands in
  `unknown_props` and is written back unchanged (this is what keeps LizzieYzy `LZ`/`LZOP` blobs intact).
* Multi-game collections: parse all `(;...)` siblings at top level, return `Vec<GameTree>`; the GUI opens the
  first and offers the rest in a "Games in file" list.
* Encoding: if `CA[...]` names a charset, decode with `encoding_rs`; otherwise try UTF-8 and fall back to
  GB18030 on invalid UTF-8 (LizzieYzy's `EncodingDetector` remaps Windows-1252 → GB18030 for the same reason).
* Write: always `CA[UTF-8]`, `AP[mirai:<CARGO_PKG_VERSION>]`, `FF[4]`, `GM[1]`.
* mirai's own cached analysis is written to property `MRAI` as
  `base64(zstd(postcard(Vec<(u32 /*node index*/, NodeAnalysis)>)))` on the **root** node, one property value, and
  is skipped on load if the leading version byte (`0x01`) does not match. Saving analysis is off by default and
  toggled by a "Save analysis in SGF" preference.

`src/clock.rs` — `TimeControl { main_s: u32, byo_periods: u8, byo_period_s: u32, increment_s: u32 }` and
`fn think_budget(tc, remaining_main_s: f32, byo_periods_left: u8) -> Option<f32>`:
* `main_s == 0 && byo_periods == 0` → `None` (unlimited; the caller uses a visit cap instead).
* `remaining_main_s > 0.0` → `Some((remaining_main_s / 20.0 + increment_s as f32 * 0.9).clamp(0.1, remaining_main_s * 0.5))`.
* otherwise → `Some(byo_period_s as f32 * 0.9)`.

### Step 4 — `mirai-engine`: the `Engine` trait and `LocalEngine` (depends on Steps 2–3, and on the MRP report
types from Step 5 — implement Step 5's `types.rs` first, it has no I/O)

`src/lib.rs`
```rust
pub trait Engine: Send + Sync + 'static {
    fn subscribe(&self, req: AnalyzeReq) -> Subscription;   // synchronous; no async-trait
    fn describe(&self) -> EngineDesc;
}
pub struct Subscription { rx: watch::Receiver<SubEvent>, _cancel: CancelGuard }
pub enum SubEvent { Pending, Report(Arc<Report>), Done(Arc<Report>), Failed(EngineError) }
```
`EngineDesc` itself lives in **`mirai-proto`** (`ServerMsg::Welcome` carries it, and `mirai-engine` depends on
`mirai-proto`, not the reverse); `mirai-engine` re-exports it:
```rust
pub struct EngineDesc { pub name: String, pub katago_version: String, pub model: String,
                        pub analysis_threads: u16, pub max_board: Size, pub has_human_model: bool }
```
Dropping a `Subscription` must terminate the underlying query — that is the whole point of `CancelGuard`.

`src/local.rs` — `LocalEngine::spawn(cfg: LocalEngineConfig) -> Result<LocalEngine>`:
* Command line, exactly:
  `<katago> analysis -model <model> -config <config> -quit-without-waiting -override-config "<overrides>"`
  where `<overrides>` is a comma-joined list of `key=value`
  (`-override-config` is available on the `analysis` subcommand via `cmd.addOverrideConfigArg()`,
  `cpp/command/analysis.cpp:77`). mirai always sets:
  `reportAnalysisWinratesAs=BLACK`, `logDir=<state_dir>/katago-logs`, `logToStderr=false`,
  `logAllRequests=false`, `logAllResponses=false`, plus, when the user overrode them in settings,
  `numAnalysisThreads`, `numSearchThreadsPerAnalysisThread`, `nnCacheSizePowerOfTwo`.
  Never pass `-analysis-threads` (KataGo errors if the config also has `numAnalysisThreads`,
  `cpp/command/analysis.cpp:101`).
* Startup handshake: send `{"id":"v0","action":"query_version"}` then `{"id":"m0","action":"query_models"}` and
  wait up to 180 s for both (OpenCL first-run tuning is slow). Populate `EngineDesc` from them. Stderr is drained
  into `tracing::debug!` continuously; the last 64 lines are kept in a ring buffer and attached to
  `EngineError::Startup` if the process dies.
* Three tokio tasks per engine: **writer** (`mpsc::Receiver<String>` → `stdin.write_all(line)` + `\n`, one
  `BufWriter` flush per drained batch), **reader** (`BufReader::lines()` → `serde_json::from_str::<RawResponse>`),
  **supervisor** (`child.wait()`; on exit, fail every live subscription with `EngineError::EngineExited`).
* Routing: `DashMap<u64, watch::Sender<SubEvent>>` keyed by a monotonic `AtomicU64` query id, serialised as its
  decimal string. `isDuringSearch == false` (or `noResults == true`) → `SubEvent::Done` + remove the entry.
  A response whose `id` is unknown is dropped with `tracing::trace!` (it is a terminated query's tail).
* `CancelGuard::drop` enqueues `{"id":"t<n>","action":"terminate","terminateId":"<sub id>"}` and marks the entry
  `terminating`; the entry is only removed when its `isDuringSearch:false` response arrives.
* Query construction from `AnalyzeReq` (`src/query.rs`) — exact JSON keys:
  `id`, `moves` (`[["B","Q4"],…]`), `initialStones`, `initialPlayer`, `rules` (shorthand string),
  `komi` (`komi_x2 as f32 / 2.0`), `boardXSize`, `boardYSize`, `maxVisits`, `analysisPVLen`,
  `includeOwnership`, `includePolicy`, `includePVVisits`, `includeMovesOwnership`,
  `reportDuringSearchEvery` (seconds, `report_every_ms as f64 / 1000.0`), `priority`,
  `avoidMoves`/`allowMoves`, `overrideSettings`.
  `max_time_ms` is emitted as `overrideSettings.maxTime` (a float in seconds) — verified overridable:
  `overrideSettings` runs through `ConfigParser::overrideKeys` + `loadParams`
  (`cpp/command/analysis.cpp:944-979`) and only `nodeTableShardsPowerOfTwo`, `useEvalCache`,
  `evalCacheMinVisits` are rejected (`cpp/search/searchparams.cpp:368`); `maxTime` is honoured by
  `Search::runWholeSearch` (`cpp/search/search.cpp:499,568`).
  `analyzeTurns` is **never** used — one query is always exactly one position, so each subscription has exactly
  one `turnNumber` and `watch` cannot drop a result that matters.
* Response decoding (`src/decode.rs`) maps KataGo JSON → the MRP `Report` (quantising as in Step 5) so that the
  local and remote paths produce bit-identical `Report`s and the GUI has one code path. Handle
  `{"error":…}` / `{"warning":…}`: an `error` with an `id` fails that subscription with
  `EngineError::Query(msg)`; a `warning` is logged and analysis continues; an `error` without an `id` fails the
  whole engine.

### Step 5 — `mirai-proto`: MRP/1 wire format (independent of Step 4; `types.rs` is a prerequisite for it)

**`src/types.rs` — quantisation is part of the type.** Every conversion helper lives here and is used by both the
local decoder and the server, so quantisation happens exactly once.

```rust
pub const PROTO_VERSION: u16 = 1;

pub struct AnalyzeReq {
    pub size: Size,
    pub rules: RuleSet,
    pub komi_x2: i16,                       // komi * 2; KataGo requires int-or-half-int
    pub initial_stones: Vec<(Color, Point)>,
    pub moves: Vec<(Color, Point)>,
    pub initial_player: Option<Color>,
    pub max_visits: Option<u32>,
    pub max_time_ms: Option<u32>,
    pub pv_len: Option<u8>,
    pub want: Want,                         // bitflags u8
    pub report_every_ms: Option<u16>,
    pub priority: i8,
    pub avoid: Vec<AvoidSpec>,              // {player, moves, until_depth, allow: bool}
    pub overrides: Vec<(String, String)>,   // e.g. ("wideRootNoise","0.0"), ("humanSLProfile","rank_3d")
}
bitflags! { pub struct Want: u8 {
    const OWNERSHIP = 1; const POLICY = 2; const PV_VISITS = 4;
    const MOVES_OWNERSHIP = 8; const ROOT_RAW = 16; } }

pub struct Report { pub turn: u16, pub root: RootInfo, pub moves: Vec<MoveInfo>,
                    pub ownership: Option<Vec<i8>>, pub policy: Option<Vec<u16>> }

pub struct MoveInfo {
    pub mv: Point, pub visits: u32, pub edge_visits: u32,
    pub winrate: u16,        // q16(wr)          wr  in [0,1]
    pub prior: u16,          // q16(prior)
    pub lcb: i16,            // qs(lcb,  16384)  clamped to [-1.9999, 1.9999]
    pub utility: i16,        // qs(u,     8192)  clamped to [-3.9999, 3.9999]
    pub utility_lcb: i16,    // qs(u,     8192)
    pub score_lead: i16,     // qs(lead,    32)  clamped to [-1023, 1023] points
    pub score_selfplay: i16, // qs(sp,      32)
    pub score_stdev: u16,    // qu(stdev,   32)  clamped to [0, 2047]
    pub order: u8,
    pub play_value: u32,     // KataGo `playSelectionValue`, rounded; drives play-mode sampling (Step 11)
    pub pv: Vec<Point>,
    pub pv_visits: Vec<u32>, // empty unless Want::PV_VISITS
}
pub struct RootInfo {
    pub visits: u32, pub winrate: u16, pub score_lead: i16, pub score_selfplay: i16,
    pub score_stdev: u16, pub utility: i16, pub current_player: Color,
    pub raw_winrate: Option<u16>, pub raw_lead: Option<i16>, pub raw_var_time_left: Option<u16>,
}
// helpers
#[inline] fn q16(v: f64) -> u16 { (v.clamp(0.0,1.0) * 65535.0 + 0.5) as u16 }
#[inline] fn dq16(v: u16) -> f32 { v as f32 / 65535.0 }
#[inline] fn qs(v: f64, scale: f64) -> i16 { (v * scale).round().clamp(-32767.0, 32767.0) as i16 }
```
`ownership[i] = (v * 127.0).round().clamp(-127.0, 127.0) as i8` — index order is already KataGo's row-major
top-left origin (**D2**), so it is a straight `map`.
`policy[i]`: legal → `min(65534, (p * 65534.0).round() as u16)`; illegal (`-1` from KataGo) → `65535`
(`POLICY_ILLEGAL`); the array has `w*h + 1` entries, the last being pass.

Because `postcard` LEB128-varints all integers, small visit counts and `Point`s < 128 cost one byte; a 50-candidate
report with PVs and ownership serialises to ≈ 2.5 KB.

**`src/msg.rs`**
```rust
pub enum ClientMsg {
    Hello { proto: u16, token: String, client: String },
    Open   { sub: u32, engine: Option<String>, req: AnalyzeReq },
    Cancel { sub: u32 },
    ListEngines,
    Ping(u64),
}
pub enum ServerMsg {
    Welcome { proto: u16, server: String, session: u64, engines: Vec<EngineDesc> },
    Engines(Vec<EngineDesc>),
    Opened { sub: u32 },
    Pong(u64),
    Error { sub: Option<u32>, code: ErrCode, msg: String },
}
pub enum SubMsg { Report(Report), Done(Report), Failed(String) } // carried on the per-sub stream
pub enum ErrCode { BadVersion, Unauthorized, NoSuchEngine, TooManySubs, BadRequest, EngineFailed, Internal }
```

**`src/frame.rs` — framing** over any `AsyncRead + AsyncWrite`:
```
[ len: u32 LE ][ flags: u8 ][ payload: len bytes ]
flags & 0x01 : payload is a zstd frame whose plaintext is the postcard message
```
`MAX_FRAME = 8 * 1024 * 1024`; a larger `len` is a protocol error and closes the connection. Compress with
`zstd` level 1 when the postcard bytes exceed `COMPRESS_THRESHOLD = 4096`; both encoder and decoder reuse a
per-connection `Vec<u8>` scratch buffer so steady-state framing allocates nothing.
Public API: `async fn write_msg<W, T: Serialize>(w: &mut W, buf: &mut FrameBuf, msg: &T)` and
`async fn read_msg<R, T: DeserializeOwned>(r: &mut R, buf: &mut FrameBuf) -> Result<T, FrameError>`.

**`src/transport.rs` — QUIC.** Stream topology, chosen so that a cancelled subscription's in-flight bytes are
*discarded* rather than delivered (the reason for QUIC over a single TCP socket):
* One client-opened **bidirectional control stream**, created immediately after the connection handshake, carrying
  `ClientMsg` in one direction and `ServerMsg` in the other, for the whole session.
* One **server-opened unidirectional stream per subscription**, whose first frame is `SubMsg` and which the server
  finishes after `Done`/`Failed`. The client maps stream → `sub` by having the server write a 4-byte LE `sub` id as
  the stream preamble before the first frame.
* On `Cancel`, the client calls `stop_sending` on that stream and the server calls `reset` — buffered stale
  reports never reach the GUI.
* ALPN `b"mirai/1"`. `TransportConfig`: `keep_alive_interval = 5 s`, `max_idle_timeout = 30 s`,
  `max_concurrent_uni_streams = 256`.
* TLS: server loads `cert.pem`/`key.pem`, generating a self-signed cert for its hostname with `rcgen` on first
  start and printing the SHA-256 fingerprint. The client verifies with a **TOFU** `rustls::client::danger::
  ServerCertVerifier` that accepts a cert whose SHA-256 matches the fingerprint stored in the profile, and on
  first connection stores whatever it is offered (the GUI shows the fingerprint in a confirmation dialog before
  storing). A mismatch is a hard connection failure with `ErrCode::Unauthorized` semantics surfaced as a toast.
* Auth: `Hello.token` compared against the configured tokens with `subtle::ConstantTimeEq`. Wrong token →
  `ServerMsg::Error { code: Unauthorized }` then close with QUIC application code `1`.

### Step 6 — `mirai-server` (depends on Steps 4–5)

Binary `mirai-server`, `clap` args: `--config <path>` (default `$XDG_CONFIG_HOME/mirai/server.toml`),
`--listen <addr>` (default `0.0.0.0:9678`), `--generate-token`, `--print-fingerprint`.

`server.toml`:
```toml
listen = "0.0.0.0:9678"
cert = "cert.pem"          # generated if missing
key  = "key.pem"

[[engine]]
name = "default"
katago = "/path/to/katago"
model  = "/path/to/model.bin.gz"
config = "/path/to/analysis.cfg"
analysis_threads = 4        # -> numAnalysisThreads override
search_threads   = 8        # -> numSearchThreadsPerAnalysisThread override

[[token]]
value = "…64 hex chars…"
name  = "laptop"
max_subs = 4
```
Behaviour:
* Start every configured engine eagerly at boot (loading a b28 net takes ~10 s; a lazy first query would look
  broken). If an engine fails to start, log the error and keep serving the others; `ListEngines` omits it.
* Per connection: read `Hello`, check `proto == PROTO_VERSION` (else `ErrCode::BadVersion`), check the token,
  reply `Welcome` with the live `EngineDesc` list.
* `Open`: resolve `engine` (`None` → the first configured engine), enforce the token's `max_subs`
  (`ErrCode::TooManySubs`), clamp `req.priority` into `-8..=8`, call `Engine::subscribe`, open the uni stream, and
  pump `SubEvent` → `SubMsg` until `Done`/`Failed`.
* Dropping the connection drops all its `Subscription`s, which terminates the KataGo queries.
* Multiple sessions share one `LocalEngine` — that is exactly what `numAnalysisThreads` is for; no per-client
  process spawning.

### Step 7 — `RemoteEngine` client (`mirai-engine/src/remote.rs`, depends on Steps 5–6)

`RemoteEngine::connect(url: &str, token: &str, engine: Option<String>, tofu: &mut TofuStore) -> Result<RemoteEngine>`
where `url` is `mirai://host:port`. It implements the same `Engine` trait, so the GUI is entirely agnostic:
* Background task owns the QUIC connection, the control stream and a `HashMap<u32, watch::Sender<SubEvent>>`.
* `subscribe` allocates a `sub` id from an `AtomicU32`, sends `Open`, returns the `Subscription`; the guard sends
  `Cancel` on drop.
* Reconnect: on connection loss, all live subscriptions get `SubEvent::Failed(EngineError::Disconnected)`; the
  task retries with exponential backoff (0.5 s, 1 s, 2 s, 4 s, capped 8 s) and the GUI shows a persistent
  "reconnecting" banner. Subscriptions are **not** silently replayed — the GUI re-requests analysis for the node
  the user is actually looking at once the banner clears.

### Step 8 — GUI shell (`mirai`, depends on Steps 3–4, 7)

* `main.rs`: install `tracing-subscriber`, build the tokio runtime, `adw::Application::builder().application_id("io.github.mirai.Mirai")`,
  `adw::init` via `libadwaita`'s application, and a `gio::resource` bundle compiled by `build.rs` with
  `glib_build_tools::compile_resources` for the UI XML, icons and CSS.
* Window: `adw::ApplicationWindow` → `adw::ToolbarView` (top `adw::HeaderBar`, bottom navigation bar) →
  `adw::OverlaySplitView` with the sidebar on the **end** side holding an `adw::ViewStack`
  (`Analysis`, `Moves`, `Comment`), content = the board area (`BoardView` in an `adw::Clamp`-free
  aspect-preserving container, plus the `WinrateGraph` under it in a `gtk::Paned`).
  `adw::Breakpoint` at `max-width: 900sp` collapses the split view.
* Header bar: open/save SGF (`gtk::FileDialog`), a `gtk::ToggleButton` for live analysis, an engine
  `gtk::DropDown` listing configured engines (local + remote profiles), a "New game" button, and the primary menu
  (`gtk::MenuButton` → `gio::Menu`) with Preferences / Keyboard shortcuts / About.
* Bottom navigation bar: `|<` `<` `>` `>|` plus branch up/down, a move-number `gtk::Scale`, and the current
  score/winrate readout.
* `adw::ToastOverlay` wraps the content; all engine errors surface as toasts.
* Actions (`gio::SimpleAction` on the window) and their accelerators — mirai's own bindings, GNOME-idiomatic:
  `win.first` <kbd>Home</kbd>, `win.last` <kbd>End</kbd>, `win.prev` <kbd>Left</kbd>, `win.next` <kbd>Right</kbd>,
  `win.prev10` <kbd>Page_Up</kbd>, `win.next10` <kbd>Page_Down</kbd>, `win.branch-prev` <kbd>Up</kbd>,
  `win.branch-next` <kbd>Down</kbd>, `win.toggle-analysis` <kbd>space</kbd>, `win.pass` <kbd>p</kbd>,
  `win.undo` <kbd>ctrl+z</kbd>, `win.delete-branch` <kbd>Delete</kbd>, `win.open` <kbd>ctrl+o</kbd>,
  `win.save` <kbd>ctrl+s</kbd>, `win.save-as` <kbd>ctrl+shift+s</kbd>, `win.copy-sgf` <kbd>ctrl+c</kbd>,
  `win.paste-sgf` <kbd>ctrl+v</kbd>, `win.toggle-ownership` <kbd>o</kbd>, `win.toggle-policy` <kbd>y</kbd>,
  `win.toggle-coords` <kbd>c</kbd>, `win.toggle-move-numbers` <kbd>n</kbd>, `win.analyse-game` <kbd>ctrl+a</kbd>,
  `win.new-game` <kbd>ctrl+n</kbd>, `win.score` <kbd>ctrl+e</kbd>. Register them in a
  `gtk::ShortcutsWindow` built from the resource bundle.
* `AppState` (a `glib::Object` subclass so properties can be bound): current `GameTree`, `NodeId` cursor,
  `Arc<dyn Engine>`, live `Subscription`, display toggles, `PlaySession`. Widgets subscribe to its
  `notify::` signals rather than reaching into each other.
* Autosave: every 30 s and on quit, write the current tree to `$XDG_DATA_HOME/mirai/autosave.sgf`; offer to
  restore it at next start if the app did not exit cleanly (a `clean-exit` flag file).

### Step 9 — `BoardView` widget (depends on Step 8)

`mirai/src/widgets/board.rs`, a `gtk::Widget` subclass. `measure` reports a square-ish natural size; `size_allocate`
computes `cell`, `origin` and `stone_r = cell * 0.48` once and caches them in a `Cell<Layout>`.

`snapshot()` draws in this order:
1. **Static layer** — wood background, grid lines, star points, coordinate labels. Built once with
   `gsk::PathBuilder` into a throwaway `gtk::Snapshot`, converted with `Snapshot::to_node()` (which consumes the
   snapshot) and cached as a `gsk::RenderNode` keyed by `(width, height, size, show_coords, style)`; every frame
   is one `snapshot.append_node(&cached)`. Invalidate the cache in `size_allocate`. Do not round-trip through a
   texture — the node tree is static and GSK caches its rendering.
2. **Ownership heat map** (toggle) — build a `w*h` RGBA8 buffer from `Report.ownership`
   (`alpha = |v|/127 * 0.45`, black or white by sign), upload with `gdk::MemoryTextureBuilder`
   (`set_bytes`, `set_format(MemoryFormat::R8g8b8a8Premultiplied)`, `set_width/height/stride`, `build()`), and
   `append_scaled_texture` with `gsk::ScalingFilter::Nearest` so each point is a crisp square.
3. **Policy heat map** (toggle, mutually exclusive with ownership) — same mechanism, alpha `= p^0.5 * 0.6`, hue
   from the app accent colour.
4. **Stones** — one `gsk::PathBuilder` circle per stone, `append_fill` with a radial gradient
   (`append_radial_gradient` for the highlight) plus a soft drop shadow drawn as a translated dark circle at 25 %
   alpha under each stone.
5. **Move numbers** (toggle) / **last-move marker** — a small circle in the contrasting colour on the last move.
6. **Marks** — `LB`/`TR`/`SQ`/`CR`/`MA` from the node.
7. **Candidate blobs** — for each `MoveInfo` up to `max_suggestions` (default 10, preference), a filled circle of
   radius `stone_r` at the move point:
   * **Fill colour** by relative visits `f = visits / best_visits`, interpolated through the Lizzie ramp
     `f=0 → #2848C8` (blue), `0.25 → #28A0A0`, `0.5 → #48C848`, `0.75 → #E8C838`, `1.0 → #E85038` (red),
     at 78 % alpha; the `order == 0` move gets a 2 px white outline.
   * **Labels**, three centred `pango` lines at `cell * 0.22`, `cell * 0.19`, `cell * 0.17`:
     line 1 = win rate for the side to move, one decimal, e.g. `56.3`; line 2 = score lead for the side to move,
     signed, one decimal, e.g. `+3.4`; line 3 = visits, SI-abbreviated (`947`, `1.2k`, `34k`, `1.1m`).
     Lines 2 and 3 are dropped when `cell < 34 px`, line 2 first.
8. **Hover PV** — when the pointer is over a candidate blob (a `gtk::EventControllerMotion` sets
   `hover: Option<usize>`), replace layers 4–7 with the position after playing that candidate's `pv`, drawing the
   PV stones with their sequence numbers. Leaving the widget restores the normal view. This is preview-only and
   never mutates the tree.

Input: `gtk::GestureClick` — primary click plays a move at the nearest intersection (or, in edit mode, places the
selected setup stone / mark); secondary click undoes the last move if it is the current node's move, otherwise
opens a context menu (`gtk::PopoverMenu`: "Play here", "Set as main line", "Delete branch", "Copy SGF").
`gtk::EventControllerScroll` navigates one move per notch.

### Step 10 — Analysis panel, winrate graph, move tree (depends on Steps 8–9)

* **Candidate list** — `gtk::ColumnView` over a `gio::ListStore` of a `CandidateObject` GObject
  (`gtk::SignalListItemFactory`): columns Move, Winrate, Score, Visits, Prior, PV (the PV rendered as a single
  ellipsised label). Selecting a row pins the PV preview on the board; activating it plays the PV's first move.
  Reuse the `ListStore` across reports (`splice`) instead of rebuilding it — at 10 Hz a rebuild would thrash.
* **`WinrateGraph`** (`mirai/src/widgets/winrate.rs`, custom widget) over the current main line from the root:
  * black win-rate curve (0–100 %, left axis) as a `gsk::PathBuilder` polyline, `append_stroke`, 2 px;
  * score-lead curve (right axis, symmetric range `±max(5, ceil(max|lead|))`), dashed, 1.5 px, accent colour;
  * a 10 px **blunder strip** along the bottom: one bar per move, colour by the win-rate drop the move caused for
    the player who made it — `< 2 %` transparent, `2–5 %` yellow `#E8C838`, `5–10 %` orange `#E8802C`,
    `> 10 %` red `#E85038`;
  * a vertical cursor line at the current move; click/drag jumps the cursor (`gtk::GestureClick` +
    `gtk::EventControllerMotion`).
* **Move tree** (`mirai/src/widgets/tree.rs`, custom widget inside a `gtk::ScrolledWindow`): lay out nodes on a
  grid — column = move number, row = branch lane assigned by a depth-first walk that keeps the main line on lane 0
  — cache the layout in the widget and recompute only when the tree's revision counter changes. Draw edges with
  `gsk::PathBuilder`, nodes as filled circles (black/white by move colour, hollow for setup nodes, an accent ring
  on the current node), and set `width_request`/`height_request` from the layout so the `ScrolledWindow` handles
  panning (do not implement `gtk::Scrollable`).
* **Comment pane**: `gtk::TextView` bound to the current node's `comment`, writing back on focus-out and on
  navigation.
* Live analysis wiring: on cursor change or tree mutation, drop the old `Subscription` (which terminates the
  KataGo query) and open a new one with
  `max_visits = settings.live_max_visits` (default **1 000 000** — ~250 MB of search tree, the practical ceiling
  for pondering one position), `report_every_ms = settings.report_interval_ms` (default **100**),
  `priority = 4`, `want = OWNERSHIP | PV_VISITS` (+ `POLICY` when the policy overlay is on).

### Step 11 — Play mode (depends on Steps 9–10)

`mirai/src/play.rs`, `PlaySession { human: Color or both, tc: TimeControl, remaining: [f32; 2], byo_left: [u8; 2], strength: Strength, resign_streak: u8, state: PlayState }`.

* **New game dialog** (`adw::Dialog` + `adw::PreferencesGroup` rows): board size, colour (Black/White/both),
  handicap 0/2–9 (placing fixed handicap stones as root `AB` setup and setting `HA`), komi (auto-filled from the
  ruleset default and reset to 0.5 when handicap > 0), ruleset, time control (main / byo-yomi periods+seconds /
  Fischer increment / none), and AI strength.
* **`Strength`**: `Visits(u32)` (default 800), `TimeMs(u32)`, or `Human { profile: String }`. `Human` is only
  selectable when `EngineDesc.has_human_model` is true; it sends `overrides = [("humanSLProfile", profile)]`.
* **AI turn**: open a subscription for the current position with
  `max_visits = strength.visits().unwrap_or(u32::MAX)`,
  `max_time_ms = think_budget(...)` from `mirai_core::clock` (`None` when the strength is visit-based and the
  time control is unlimited), `priority = 8`, `report_every_ms = Some(200)` (so the "thinking" indicator has a
  live visit count), `want = OWNERSHIP`, `overrides = [("wideRootNoise", "0.0")]` — the analysis config ships
  `wideRootNoise = 0.04`, which deliberately weakens play (`configs/analysis_example.cfg:36`).
* **Move selection** from the final `Report`: with `settings.play_temperature <= 0.0` take `moves[0]`
  (`order == 0`); otherwise sample index `i` with weight `moves[i].play_value as f64` raised to `1.0 / t`,
  using a `SplitMix64` seeded from the system clock (no `rand` dependency).
* **Resign**: if `root.winrate` seen from the AI's colour is below `settings.resign_threshold` (default 0.05) on
  `settings.resign_streak` (default 3) consecutive AI turns **and** `move_number > size.points() / 4`, the AI
  resigns; set `GameInfo.result` to `"B+R"`/`"W+R"` and end the game.
* **Game end**: two consecutive passes (or a resignation) → `PlayState::Scoring`. Scoring runs one analysis query
  with `want = OWNERSHIP`, `max_visits = 400`, derives `DeadSet::from_ownership`, and shows the result in an
  `adw::AlertDialog`; clicking a group on the board toggles its life/death and rescoring is immediate and local
  (no new query). Territory is drawn as the small squares from `ScoreResult::territory`.
* **Clock**: a `glib::timeout_add_local` at 100 ms decrements the active player's remaining time; the header bar
  shows both clocks. Running out of main time consumes a byo-yomi period; running out of the last period loses on
  time (`"B+T"`/`"W+T"`).
* **"Analyse after game"**: when the game ends, offer a button that runs Step 12 over the finished game.
* Undo (<kbd>ctrl+z</kbd>) in play mode retracts both the AI's move and the human's, restores the clocks from the
  node snapshot, and cancels any in-flight AI subscription.

### Step 12 — Whole-game analysis (depends on Step 10)

`mirai/src/batch.rs`: walk the main line, and for each node open a subscription with
`max_visits = settings.batch_visits` (default 1000), `report_every_ms = None`, `priority = 0`,
`want = OWNERSHIP`. Keep `min(EngineDesc.analysis_threads * 2, 16)` in flight — the engine batches across
concurrent positions, which is why this is much faster than sequential `kata-analyze`. Store each final `Report`
into the node's `NodeAnalysis`. Show progress in an `adw::Banner` with a cancel button; cancelling drops all
subscriptions. On completion, compute per-move win-rate drops and populate the blunder strip and a "Blunders"
list in the sidebar (move number, player, drop, best move) whose rows jump to the node.

### Step 13 — Settings and profiles (depends on Step 8)

`$XDG_CONFIG_HOME/mirai/config.toml`, read/written with `toml`, located via `directories::ProjectDirs`:
```toml
active_engine = "local-default"

[[engine_profile]]              # kind = "local"
name = "local-default"
kind = "local"
katago = "…/katago"
model  = "…/default.bin.gz"
config = "…/analysis.cfg"
analysis_threads = 2
search_threads = 16

[[engine_profile]]              # kind = "remote"
name = "workstation"
kind = "remote"
url = "mirai://192.168.1.10:9678"
token = "…"
engine = "default"
cert_sha256 = "…"               # TOFU pin, filled on first connect

[analysis]
live_max_visits = 1000000
report_interval_ms = 100
batch_visits = 1000
max_suggestions = 10

[play]
strength = { kind = "visits", visits = 800 }
temperature = 0.0
resign_threshold = 0.05
resign_streak = 3

[ui]
show_coordinates = true
show_move_numbers = false
ownership_overlay = false
policy_overlay = false
save_analysis_in_sgf = false
```
Preferences UI: `adw::PreferencesDialog` with pages **Engines** (an `adw::PreferencesGroup` list of profiles,
each an `adw::ActionRow` with edit/delete; add-local uses `gtk::FileDialog` for the three paths, add-remote asks
for URL + token and shows the certificate fingerprint for confirmation), **Analysis**, **Play**, **Appearance**.
On first run with no config, if
`/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/linux-x64/katago` exists, seed the
`local-default` profile from the bundled paths; otherwise open Preferences with an `adw::StatusPage` prompt.

---

## Critical files & anchors

* `/home/ykpcx/KataGo/docs/Analysis_Engine.md` — request/response field names (`moveInfos`, `rootInfo`,
  `isDuringSearch`, `noResults`) and the row-major top-left ordering statement for `ownership`/`policy`
  (lines 260, 280–282). The single source of truth for Steps 4–5.
* `/home/ykpcx/KataGo/cpp/command/analysis.cpp:77-101, 944-979` — `-override-config` availability, the
  `-analysis-threads` vs `numAnalysisThreads` conflict, and how `overrideSettings` is applied. Read before
  writing the spawn command.
* `/home/ykpcx/KataGo/cpp/game/rules.cpp:273-372` — the exact ko/scoring/tax/suicide/button/handicap-bonus tuple
  per named ruleset, transcribed into `mirai-core/src/rules.rs`.
* `/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/configs/analysis.cfg` — the config file the
  default profile points at; already sets `reportAnalysisWinratesAs = BLACK` (line 30),
  `numAnalysisThreads = 2` (line 95), `wideRootNoise = 0.04` (line 36, the reason play mode overrides it).
* `/home/ykpcx/probe/lizzieyzy-next/src/main/java/featurecat/lizzie/rules/SGFParser.java` — property coverage and
  the komi ≥ 200 normalisation rule; consult only for the SGF quirk list, not for structure.

---

## Verification

Run everything from `/home/ykpcx/probe/mirai`. Shorthand used below:
```
KATA=/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/linux-x64/katago
MODEL=/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/weights/default.bin.gz
CFG=/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/configs/analysis.cfg
```

1. **Build**: `cargo build --workspace --all-targets` succeeds; `cargo clippy --workspace -- -D warnings` clean.
2. **Rules and SGF unit tests** (`cargo test -p mirai-core`) — each must fail on a plausible bug, so assert
   observable outcomes, not plumbing:
   * `A19` ⇄ `Point(0)` and `T1` ⇄ `Point(360)` on 19×19; SGF `aa` ⇄ `Point(0)`, `ss` ⇄ `Point(360)`.
   * a ko capture is rejected under `Ko::Simple` and permitted one move later; the same position repeated is
     rejected under `Ko::Positional` but allowed under `Ko::Situational` when the side to move differs.
   * single-stone suicide illegal under every ruleset; a 2-stone suicide legal under `NewZealand`/`TrompTaylor`
     and illegal under `Chinese`.
   * area vs territory scoring of one hand-built 9×9 endgame position gives the two known point totals.
   * `/home/ykpcx/2026-04-26-linux64.with-katago/save/autoGame1.sgf` (a real LizzieYzy file, 25 765 bytes,
     19×19, `KM[7.5]`, UTF-8 with Chinese comments) parses to a tree whose main line replays without an illegal
     move, and re-serialises with its `DZ[G]` and `LZOP[...]` properties byte-identical to the input values —
     that is the unknown-property preservation contract, tested against real data rather than a synthetic
     `FOO[bar]`.
3. **Local engine, real KataGo** — `cargo run -p mirai-engine --example probe -- --katago $KATA --model $MODEL
   --config $CFG` (write this example as part of Step 4). It must print the version from `query_version`, then
   analyse the empty 19×19 board with `max_visits=200, want=OWNERSHIP|PV_VISITS, report_every_ms=100` and print,
   for each report, `visits`, the top 5 moves as `<gtp> <winrate%> <lead>`, and `ownership.len()`.
   **Expected**: ≥ 2 intermediate reports then a final one; `ownership.len() == 361`; the top move is a 4-4/3-4
   point (`D4`/`Q4`/`D16`/`Q16`/`R16`-family, never `A1`); winrate within `45.0..55.0`. Anything else means the
   ownership order or the Black-perspective conversion is wrong.
4. **Remote round-trip, the new protocol end to end** — terminal A:
   `cargo run -p mirai-server -- --listen 127.0.0.1:9678 --generate-token` (prints a token and the certificate
   fingerprint; write a `server.toml` pointing at `$KATA`/`$MODEL`/`$CFG`). Terminal B:
   `cargo run -p mirai-engine --example probe -- --remote mirai://127.0.0.1:9678 --token <token>` — the **same**
   example binary behind `RemoteEngine`. **Expected**: byte-identical output shape to check 3 (same field values
   modulo search nondeterminism), proving the `Engine` abstraction and the quantisation round-trip.
   Then, while a live subscription with `max_visits=100000000` is streaming, kill terminal B with Ctrl-C:
   terminal A must log the subscription dropping and the KataGo query being terminated within ~1 s (check with
   `grep -c '"action":"terminate"'` on the engine's request log if `logAllRequests` is temporarily enabled).
5. **Wire-size proof** (this is the performance claim, so measure it): add
   `cargo test -p mirai-proto --test wire_size`, which builds a `Report` with 50 `MoveInfo`s (PV length 15,
   `pv_visits` populated) plus a 361-entry `ownership`, encodes it with the frame codec, and asserts the framed
   length is `< 4096` bytes. Print the actual size. Also assert the dequantised round-trip error:
   winrate ≤ 1e-4 absolute, score lead ≤ 0.02 points, ownership ≤ 0.005.
6. **GUI smoke test — analysis** (this is the deliverable, so drive it, do not just build it):
   `cargo run -p mirai`. Open `/home/ykpcx/2026-04-26-linux64.with-katago/save/autoGame1.sgf`. Toggle live
   analysis on. **Expected**: candidate blobs appear on the board within
   a few seconds with three-line labels; pressing <kbd>o</kbd> shows the ownership overlay and the black/white
   regions match the stones on the board (a sanity check that the row-major mapping is not transposed — a
   transpose is immediately visible as a mirrored map); hovering a blob shows a numbered PV; <kbd>Left</kbd>/
   <kbd>Right</kbd> navigate and the analysis restarts for the new node; the winrate graph draws a curve and
   clicking it jumps the cursor.
7. **GUI smoke test — play**: New game, 9×9, handicap 0, komi 7.5, Chinese, AI strength 400 visits, human plays
   Black. **Expected**: the AI replies within a few seconds per move; passing twice opens the scoring dialog with
   a plausible result; clicking a dead group flips its status and the total updates instantly; "Analyse after
   game" fills the blunder strip.
8. **GUI over the network**: with `mirai-server` running (check 4), add a remote profile in Preferences, confirm
   the fingerprint, switch the engine drop-down to it, and repeat check 6. **Expected**: identical behaviour;
   stopping the server shows the reconnecting banner and restarting it clears it.

---

## Assumptions & contingencies

* **QUIC only.** If UDP/9678 turns out to be blocked on the target network, do not redesign: `frame.rs` is generic
  over `AsyncRead + AsyncWrite`, so add a second `transport` impl over `tokio-rustls` TCP that carries the control
  stream on the socket and prefixes every subscription frame with its 4-byte `sub` id on the same stream (losing
  only the reset-on-cancel property), selected by a `mirai+tcp://` URL scheme.
* **TOFU certificate pinning** rather than a CA. If you would rather use a real certificate, point `cert`/`key` at
  it and put the fingerprint in the client profile anyway — the verifier only ever compares fingerprints.
* **`live_max_visits = 1_000_000`** assumes ≲ 250 MB of search tree per pondering position. If pondering shows
  runaway RSS on the target machine, lower the default to 200 000; the value is a preference, not a constant.
* **Board sizes 2..=19.** KataGo's `MAX_LEN` is 19 unless recompiled (`docs/Analysis_Engine.md:84`), and the
  bundled binary is a stock release. If a 25×25 board is ever needed, `Size::new` is the only place to widen, and
  `Point` already has the range.
* **Analysis engine for play, not GTP.** If a future need appears for something only GTP provides
  (`kata-raw-nn`, server-side time control), it is added as a *second* `Engine` impl behind the same trait, not by
  changing the GUI.
* **Bundled paths are seeded, not hardcoded.** Everything under `/home/ykpcx/2026-04-26-linux64.with-katago/` only
  ever appears as a first-run default in `config.toml`; no crate may reference it in code.
