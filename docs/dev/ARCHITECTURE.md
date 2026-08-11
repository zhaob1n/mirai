# mirai — system architecture

Contributor entry point is [`../../AGENTS.md`](../../AGENTS.md). The wire format is specified
normatively in [`PROTOCOL.md`](PROTOCOL.md); how to prove a change works is
[`TESTING.md`](TESTING.md); why the design is what it is, with the history, is
[`../archive/RETROSPECTIVE.md`](../archive/RETROSPECTIVE.md); the user's view is
[`../user/GUIDE.md`](../user/GUIDE.md).

Citations are file plus symbol. Signatures and field lists are deliberately not repeated here —
run `cargo doc --workspace --open` for those. `[INFERENCE]` marks anything reasoned rather than
read.

---

## 1. The system

mirai analyses and plays Go with KataGo. The GTK4/libadwaita application holds a game record in
memory and, for whatever position the cursor sits on, asks an *engine* for a stream of analysis
reports. An engine is anything implementing `Engine` (one method that matters, `subscribe`):
`LocalEngine` drives a `katago analysis` subprocess over its JSON stdio protocol, `RemoteEngine`
forwards the same request to a `mirai-server` over QUIC. Both yield the identical `Report` type,
so the GUI has one code path and never knows which kind of engine it holds. Every request carries
its whole position, so nothing anywhere holds engine-side session state and cancelling an
analysis is just dropping a handle.

```
                    ┌───────────────────────────────┐
                    │  mirai  (GTK4 + libadwaita)   │  bin
                    └───┬──────────┬────────────┬───┘
                        v          v            v
   ┌────────────────────┐  ┌──────────────┐  ┌──────────────────┐
   │   mirai-engine     │─>│ mirai-proto  │─>│   mirai-core     │
   │ Engine/Local/Remote│  │ MRP/1 + QUIC │  │ rules, tree, SGF │
   └─────────┬──────────┘  └──────┬───────┘  └──────────────────┘
             │      ┌─────────────┴──────────────┐          ^
             └─────>│  mirai-server  (headless)  │──────────┘
                    └────────────────────────────┘  bin

   mirai-engine ──spawn/stdio──> katago analysis            (LocalEngine)
   mirai        ──QUIC/MRP/1───> mirai-server ──> katago    (RemoteEngine)
```

| crate | owns | depends on | may **not** depend on |
|---|---|---|---|
| `mirai-core` | geometry, rulesets, legality, superko, scoring, game tree, SGF, time control | serde, smallvec, arrayvec, encoding_rs, base64, zstd, postcard | any workspace crate; GTK; tokio; anything doing real I/O |
| `mirai-proto` | MRP/1 value types, messages, frame codec, QUIC transport, SHA-256 for cert pins | `mirai-core`, quinn, rustls, rcgen, postcard, zstd, bitflags, tokio | `mirai-engine`, `mirai`, serde_json, **anything KataGo-specific** |
| `mirai-engine` | the `Engine` trait and its two implementations; KataGo query building and response decoding | `mirai-core`, `mirai-proto`, tokio, serde_json, dashmap, quinn | GTK/glib/adw, `mirai`, `mirai-server` |
| `mirai-server` | headless host: one KataGo per configured engine, multiplexed across clients, token auth | `mirai-core`, `mirai-engine`, `mirai-proto`, clap, toml, subtle, quinn | GTK, `mirai` |
| `mirai` | `AppState`, window, custom `gsk` widgets, panels, play mode, batch analysis, preferences, config | all three libraries, gtk4, libadwaita, glib, tokio, toml, directories | — |

Versions are pinned once in the root `[workspace.dependencies]`; members use
`dep.workspace = true`. `mirai` and `mirai-server` are binaries; nothing depends on either.

**The layering rule** — a rule, not an observation:

- **`mirai-core` has no I/O and no GUI.** SGF reading takes a byte slice, writing returns a
  `String`; nothing in the crate opens a file or socket.
- **`mirai-proto` knows nothing about KataGo.** It defines what a `Report` *is*, never where one
  came from. The single place KataGo JSON becomes a `Report` is `decode_report` in
  `mirai-engine/src/decode.rs`.
- **`mirai-engine` knows nothing about GTK.** tokio only, `Send + Sync`, so a server, a CLI
  example and a GUI can all drive it. `mirai-engine/examples/probe.rs` runs both backends with
  no GUI at all, which is how the two paths get compared.
- **Only `mirai` links GTK.** A `use gtk::` elsewhere is a design break; fix the design.

---

## 2. Design decisions that shape everything

### 2.1 The KataGo JSON analysis engine, never GTP — and stateless queries (INV-4)

One engine interface; requests are whole positions (stones, moves, rules, komi, caps). KataGo's
`analyzeTurns` is never emitted, so one request is exactly one position and exactly one report
stream.

- **Rationale.** GTP is a stateful command channel (`play`/`undo`/`clear_board`) whose board a
  client must mirror or diverge from. Analysis queries have no state to mirror.
- **Buys.** No replay/undo machinery. Arbitrary tree navigation is one new request, not a diff.
  The remote protocol is a pure request/stream forwarder, so the server keeps no per-client
  board state and can multiplex clients onto one KataGo. Cancellation is a drop.
- **Costs.** Every request re-sends the position (a 200-move `Open` frames to 500 bytes) and
  KataGo re-walks the move list; its NN cache absorbs most of that. Anything genuinely stateful
  — pondering that persists across cursor moves — is out of reach by construction.

### 2.2 mirai generates KataGo's analysis config

`katago analysis` refuses to start without a `-config` file, so a local profile that names none
gets one written for it by `EngineTuning` (`mirai-engine/src/tuning.rs`); naming a file still
uses that file as it stands.

- **Rationale.** Only three keys in that file are required — `numAnalysisThreads`,
  `numSearchThreadsPerAnalysisThread`, `nnMaxBatchSize`; drop any one and KataGo aborts with
  `Could not find key`. Everything else either travels on the query (rules, komi, board size,
  search limits, which outputs to include), is forced through `-override-config` (INV-2 and
  logging), or is better left at KataGo's own default, so an upstream improvement to those
  defaults arrives without mirai having to track it.
- **Buys.** A working local engine needs two paths, the binary and the model — no config file
  to find, write or keep in step with the query builder. The defaults are one fixed set of
  constants, nothing is measured or detected at run time: 4 analysis threads, 16 search threads
  each, `nnMaxBatchSize` 64, `nnCacheSizePowerOfTwo` 20 (about 3 GiB). Measured on a Radeon
  RX 6800: 1 → 4 analysis threads takes the cost of moving the cursor to another node from
  112 ms to 2.4 ms with no loss of single-position speed; 8 → 16 search threads is worth 14% on
  a single position, where 16 → 32 adds only 8% and worsens in-tree contention. A batch of 64
  covers those 4 × 16 threads. The cache is the one value set for a reason beyond being
  required: KataGo's analysis default of 2^23 is sized for the batch server the engine was
  written for, and settles near 24 GiB with ownership for 128 MiB of pointers up front.
  2^20 — KataGo's GTP default — settles near 3 GiB for 16 MiB, and a miss only costs a
  re-evaluation. `nnMutexPoolSizePowerOfTwo` is *not* set: its cost is a few MiB either way,
  so there is nothing to gain by disagreeing with KataGo about it.
- **Costs.** mirai owns a file on disk, in the profile's log directory. Its name carries the
  four values (`katago-analysis-a4-s16-b64-c20.cfg`), so profiles tuned differently cannot race
  each other through one path — engines start concurrently, one per window — while an identical
  tuning resolves to a byte-identical file that is left alone, keeping a shared config from
  being churned under a running KataGo. And the local-engine editor has two modes: with a
  custom file the generated values are meaningless, so Preferences hides the batching and
  cache rows and only the two thread values are still passed as overrides.

### 2.3 KataGo's own point encoding (INV-1)

`index = y * width + x`, `y = 0` the top row, `PASS` as the maximum `u16`.

- **Rationale.** KataGo emits `ownership` and `policy` in exactly this order. Any other
  convention means writing a remap, and remaps are where transposed heat maps come from.
- **Buys.** `report.ownership[i]` and `board.stones()[i]` are the same intersection, so overlays
  are a straight texture blit with no index arithmetic. SGF is also top-left-origin, so
  `Size::to_sgf`/`from_sgf` need no flip.
- **Costs.** GTP is bottom-left-origin and 1-based, so display coordinates flip
  (`row = h - y`). Two conventions exist; the GTP one is confined to `Size::to_gtp`/`from_gtp`.

### 2.4 Everything is stored Black-perspective (INV-2)

KataGo runs with `reportAnalysisWinratesAs=BLACK`; every stored, cached and transmitted value is
Black's. Conversion happens only at display time via `winrate_for` / `score_lead_for`.

- **Rationale.** A side-to-move value is only interpretable together with the node it came from.
  Black-perspective values are sign-stable, so a cached series can be plotted across a whole game
  without asking whose turn each node was.
- **Buys.** Blunder detection is a subtraction after one flip each side
  (`blunder_severity` in `widgets/winrate.rs`, `blunders` in `batch.rs`). SGF-persisted analysis
  needs no perspective metadata. `Color::sign()` is also KataGo's ownership sign.
- **Costs.** A double conversion is nearly invisible on screen — 45% looks as plausible as 55%.
  Every new display site must be checked; that is why no call site hand-rolls `1.0 - x`.

### 2.5 `tokio::sync::watch` for subscriptions — a lagging consumer *should* skip

A `Subscription` wraps a `watch::Receiver`; producers use `send_replace`, so an unread report is
overwritten rather than queued.

- **Rationale.** Reports arrive at ~10 Hz and each supersedes the previous one for the same
  position. A GUI busy laying out a `ColumnView` must never accumulate obsolete reports, and must
  never be the reason the engine stalls.
- **Buys.** Coalescing for free: no bounded queue, no drop policy, no backpressure design.
  `current()` is always the newest state, so a consumer that just woke up is immediately correct.
  Terminal events are safe under the same rule *because one query is one position* — the last
  value written is always the final report.
- **Costs.** Intermediate reports are genuinely lost; nothing may treat the stream as a log.
  Anything needing every value (there is nothing in-tree) needs a different channel.

### 2.6 Quantised wire values (INV-6)

Quantisation is part of the type: a candidate's win rate *is* a `u16`. Scales live only in
`mirai-proto/src/types.rs`; `*_f32` accessors are the only way back to floats.

- **Rationale.** The equivalent KataGo JSON for a 50-candidate report with ownership is ~45 KB.
  At 10 Hz per client that is the difference between a protocol that works on a LAN and one that
  does not.
- **Buys.** 2622 bytes framed for that worst case (`mirai-proto/tests/wire_size.rs`). The local
  and remote paths quantise with the same functions, so the two engines produce bit-identical
  reports — which is what permits one GUI code path. Ownership is one byte per point.
- **Costs.** A tested error budget (winrate ≤ 1e-4, lead ≤ 0.02 pt, ownership ≤ 0.005). Changing
  a scale is a protocol change: bump `PROTO_VERSION` and update `wire_size.rs`. `POLICY_ILLEGAL`
  burns one sentinel value because KataGo reports illegal moves as `-1`.

### 2.7 Custom `gsk` widgets, not `DrawingArea` + cairo (INV-9)

Board, win-rate graph and move tree are `gtk::Widget` subclasses drawing in `snapshot()`.

- **Rationale.** `snapshot` builds a retained render-node tree for the GPU; `DrawingArea`
  rasterises through cairo on the CPU every frame. At 10 Hz with dozens of candidate blobs and
  three text lines each, that is structural rather than a micro-optimisation.
- **Buys.** The static board (wood, grid, star points, coordinates) is built once into a
  `gsk::RenderNode`, cached against `StaticKey`, and replayed with one `append_node` per frame.
  Heat maps upload as a single `gdk::MemoryTexture`. Being real widgets, they take part in
  layout, CSS and input controllers normally.
- **Costs.** No cairo conveniences: circles are `gsk::PathBuilder` paths, text is a
  `pango::Layout` per string. Static-layer invalidation is manual. And the only honest way to see
  what was drawn is to render through the app's own renderer — that is what `harness.rs` is for.

---

## 3. Module map

Every `.rs` file under `crates/`. Open the file named in the row; the symbols are greppable.

### `mirai-core` — geometry, rules, tree, SGF

| file | owns | key symbols |
|---|---|---|
| `src/lib.rs` | crate root, flat re-exports, the shared PRNG | `SplitMix64` — deterministic across runs, which the Zobrist contract requires |
| `src/point.rs` | **INV-1.** Point encoding, board dimensions, coordinate parsing/formatting, neighbours, star points | `Point`, `Size`, `Color`, `Neighbors`, `MIN_DIM`/`MAX_DIM`, `COLUMNS`, `to_gtp`/`from_gtp`, `to_sgf`/`from_sgf`, `star_points` |
| `src/rules.rs` | the nine rulesets, mirrored from KataGo's `rules.cpp` so local legality never disagrees with the engine | `RuleSet` (+`ALL`, `katago_name`, `label`, `rules`, `default_komi`), `Rules`, `Ko`, `Scoring`, `Tax`, `Whb` |
| `src/board.rs` | stones, capture, suicide, simple ko, incremental Zobrist, chain flood fill | `Board` (`play`, `is_legal`, `set`, `chain`, `liberties`, `zobrist`, `situational_hash`, `ko_ban`), `Captured`, `IllegalMove`, `WHITE_TO_MOVE_HASH` |
| `src/score.rs` | final-position scoring: dead stones, connected components, area and territory counts, seki tax | `score`, `DeadSet` (`from_ownership`, `toggle_chain`), `ScoreResult` (`margin`, `result_string`), private `components`, `MAX_EYE` |
| `src/handicap.rs` | fixed handicap placement in conventional order | `fixed_handicap` — empty for unsupported counts, non-square, `< 7x7` or even-sided boards |
| `src/clock.rs` | time control and the per-move thinking budget | `TimeControl`, `think_budget` |
| `src/tree.rs` | the game record: node arena, tombstoned deletes, cached `Position`, superko enforcement | `GameTree`, `NodeId`, `Node`, `Position`, `GameInfo`, `Setup`, `Marks`/`MarkKind`, `NodeAnalysis`, `Candidate`, `revision` |
| `src/sgf.rs` | hand-written SGF FF[4] read/write, unknown-property preservation, the `MRAI` analysis property | `parse`, `parse_str`, `write`, `SgfError`, private `build`, `write_node`, `is_root_info_prop`, `encode_analysis`/`decode_analysis`, `MAX_DEPTH`, `MRAI_VERSION` |

### `mirai-proto` — MRP/1

| file | owns | key symbols |
|---|---|---|
| `src/lib.rs` | crate root and re-exports | — |
| `src/types.rs` | **INV-2, INV-5, INV-6.** Quantised wire values, the request and report types | `AnalyzeReq`, `Report`, `RootInfo`, `MoveInfo`, `EngineDesc`, `Want`, `AvoidSpec`, `PROTO_VERSION`, `POLICY_ILLEGAL`, the `*_SCALE` constants, `q16`/`dq16`/`qs`/`dqs`/`qu`/`dqu`/`q_own`/`q_policy`, `winrate_for`, `score_lead_for` |
| `src/msg.rs` | the three message enums and the error code set | `ClientMsg`, `ServerMsg`, `SubMsg`, `ErrCode` |
| `src/frame.rs` | length-prefixed postcard framing with optional zstd, generic over `AsyncRead`/`AsyncWrite` | `read_msg`/`write_msg`, `encode`/`decode`, `FrameBuf` (reused per connection, so steady state does not allocate), `MAX_FRAME`, `COMPRESS_THRESHOLD`, `FrameError`, private `Bounded` decompression-bomb guard |
| `src/transport.rs` | QUIC endpoints, stream topology, TOFU certificate verification, URL parsing | `connect`, `client_endpoint`, `server_endpoint`, `TofuVerifier`, `load_or_generate_cert`, `fingerprint_of`, `parse_url`, `transport_config`, `ALPN`, `DEFAULT_PORT`, `URL_SCHEME` |
| `src/sha256.rs` | in-tree SHA-256, only for certificate fingerprints | `sha256`, `fingerprint` |
| `tests/wire_size.rs` | measures the size claim and pins the quantisation error budget | — |

### `mirai-engine` — engine drivers

| file | owns | key symbols |
|---|---|---|
| `src/lib.rs` | **the engine contract** | `Engine`, `Subscription`, `SubEvent`, `CancelGuard`, `EngineError` |
| `src/query.rs` | KataGo analysis-engine query construction — the only place KataGo field names are written | `build_query`, `action_query`, `terminate_query` |
| `src/decode.rs` | the only place KataGo JSON becomes a `Report`; classifies every stdout line | `RawResponse::classify`, `decode_report`, `RAW_VAR_TIME_SCALE` |
| `src/local.rs` | one `katago analysis` subprocess: three tasks, id routing, startup handshake, shutdown | `LocalEngine::spawn`, `LocalEngineConfig`, private `Inner` (`cancel`, `handle`, `deliver`, `fail_all`), `write_lines`, `read_responses`, `drain_stderr`, `supervise`, `handshake`, `override_config` |
| `src/tuning.rs` | the analysis config mirai writes for itself: the three required KataGo keys plus the cache size, one fixed set of defaults, everything else left to KataGo | `EngineTuning` (`analysis_threads`, `search_threads`, `nn_max_batch_size`, `nn_cache_size_power_of_two`; `Default`, `render`, `write_to` — rewrites only on change, `cache_bytes`, `batch_covers_threads`, the `MAX_*`/`*_CACHE_POWER` bounds), `CACHE_ENTRY_BYTES` |
| `src/remote.rs` | MRP/1 client: one background task owns the connection, control stream and subscription table | `RemoteEngine::connect`, `RemoteStatus`, `TofuStore`, private `run`, `serve`, `handle_cmd`, `handle_int`, `control_reader`, `uni_acceptor`, `sub_reader`, `reconnect`, `backoff` |
| `examples/probe.rs` | CLI that drives either backend through the same trait and prints every report — the local-vs-remote comparison harness | `--katago/--model/--config` or `--remote/--token` |

### `mirai-server` — headless host

| file | owns | key symbols |
|---|---|---|
| `src/main.rs` | CLI, engine startup, endpoint bind, accept loop, Ctrl-C shutdown | `Args`, `run`, `start_engines` (a broken engine is logged and skipped, never fatal), `generate_token`, `cert_hostnames`, `report_missing_config` |
| `src/config.rs` | `server.toml`: engines, tokens, limits. Relative paths resolve against the config file's directory, never the CWD | `ServerConfig` (`parse`, `load`, `listen_addr`), `EngineCfg` (`config` is optional, plus `nn_max_batch_size`; `to_local_config` is fallible because omitting `config` makes it write the generated config into `log_dir`), `TokenCfg`, `MINIMAL_EXAMPLE`, `resolve_listen`, private `rebase`. All three structs are `deny_unknown_fields` |
| `src/session.rs` | one QUIC connection: control stream, token check, one task per subscription | `serve`, `Host` (`authenticate` — constant-time via `subtle`, no early exit; `resolve_engine`; `describe`), `NamedEngine`, `Token`, private `session_loop`, `pump`, `SubMsgRef` (a zero-copy mirror of `SubMsg`, pinned byte-identical by a test), `PRIORITY_RANGE`, `CODE_CANCELLED` |
| `server.example.toml` | commented example config (not Rust) | — |

### `mirai` — the application

| file | owns | key symbols |
|---|---|---|
| `build.rs` | compiles `resources/mirai.gresource.xml` into the binary | — |
| `src/main.rs` | process entry: tracing, the single tokio runtime, resources, CSS, the shared `EnginePool`, `activate`/`open` (one window per file) | `APP_ID`, `RESOURCE_PREFIX` |
| `src/app.rs` | **INV-7.** `AppState`: the single source of truth *for one window* — properties, signals, tree/cursor API, engine activation, the live-analysis pump, the search-speed meter | `AppState`, `mod signal`, `with_tree_mut`, `with_tree_cached`, `set_cursor`, `play_move`, the `go_*` navigators, `activate_profile`, `remember_active`, `request_for_node`, `restart_analysis`, `set_report`, `analysis_speed`, `SpeedMeter` (+`SPEED_SMOOTHING`) |
| `src/engines.rs` | the application-wide engines: one per profile, shared by every window, held weakly so the last window to let go takes KataGo with it; writes the generated analysis config when a local profile has no custom one | `EnginePool` (`running`, `acquire`), `Built`, private `key`, `start`, `build` |
| `src/config.rs` | `$XDG_CONFIG_HOME/mirai/config.toml`: engine profiles and preferences | `Config` (`load`, `save`, `save_merged`, `seeded`, `profile`, `active_profile`, `set_pin`, `default_path`, `data_dir`), private `overlay`, `EngineProfile`, `ProfileKind` (`Local`'s `config` is optional — `None` means mirai generates it — plus `analysis_threads`, `search_threads`, `nn_max_batch_size`, `nn_cache_size_power_of_two`, and `tuning()` which fills the unset ones from `EngineTuning::default`), `AnalysisSettings`, `PlaySettings`, `StrengthSetting`, `UiSettings` |
| `src/util.rs` | formatting helpers and the one `Report` → `NodeAnalysis` conversion | `analysis_of`, `si_visits`, `visits_per_second`, `pct1`, `signed1`, `clock_text`, `gtp` |
| `src/window.rs` | **INV-8.** The window: layout, every `win.*` action and accelerator, SGF I/O, autosave, score estimate, shortcuts/about | `Ui`, `present`, `with_ui`, `connect_close`, `install_actions`, `primary_menu`, the `update_*` refreshers, `load_sgf`/`do_open`/`do_save`/`do_save_as`, `adopt`, `write_autosave`/`next_autosave_path`/`stale_autosaves`/`offer_restore`/`tree_has_content`, `do_score`/`show_estimate`, `delete_branch`, `AUTOSAVE_SECS`, `AUTOSAVE_PREFIX`, `SCORE_VISITS`, `DEAD_THRESHOLD` |
| `src/widgets/mod.rs` | widget root; states the no-cairo rule | re-exports `BoardView`, `MoveTreeView`, `WinrateGraph` |
| `src/widgets/board.rs` | the goban: static-layer cache, stones, marks, move numbers, ownership/policy heat maps, candidate blobs, PV preview, click/hover/context menu | `BoardView` (`point_at`, `set_click_hook`, `set_score_overlay`, `set_pv_preview`), `Layout` (`compute`, `hit`), `Scene`, `StaticKey`, **`VISIT_RAMP`/`ramp_rgb`**, `draw_stones`/`draw_territory`/`draw_numbers`/`draw_marks`/`draw_candidates`, `blit`, `text_on` |
| `src/widgets/winrate.rs` | win-rate curve, score-lead curve and blunder strip, drawn from cached `NodeAnalysis` on the main line | `WinrateGraph`, `Severity` (+`color`), `severity_of_drop`, `blunder_severity`, private `Sample`, `Geom` |
| `src/widgets/tree.rs` | the branch graph; lane layout cached against `GameTree::revision()` | `MoveTreeView` (+`in_scroller`), `lay_out`, `TreeLayout`, `Placed`, `cell_xy` |
| `src/panels/mod.rs` | sidebar panel root | re-exports `AnalysisPanel` |
| `src/panels/analysis.rs` | the Analysis page: root readout, candidate `ColumnView` spliced in place at report rate, blunder list | `AnalysisPanel` (`connect_pv_preview`, `set_blunders`, `clear_blunders`), `CandidateObject`, `Row`, `Headline`, `severity_class`, `pv_text`, `text_column` |
| `src/play.rs` | play mode: turn tracking, clocks, the AI's subscription, resignation, end-of-game scoring | `PlayController` (`start`, `stop`, `on_human_move`, `pass`, `resign`, `undo`, `clocks`, `attach_board`, `set_analyse_hook`), `PlaySession`, `PlayState`, `GameSetup`, `Strength`, `tick_clock`, `resign_check`, `select_move_index`, `result_phrase` |
| `src/batch.rs` | whole-game analysis: bounded-concurrency sweep over the main line, blunder extraction | `BatchAnalysis` (`start`, `cancel`, `banner`, `connect_finished`), `blunders`, `Blunder`, `in_flight`, `BLUNDER_MIN_DROP`, private `run_worker` |
| `src/dialogs.rs` | New Game, score summary, certificate confirmation | `new_game`, `show_score_with`, `confirm_fingerprint`, `grey_out_human` |
| `src/prefs.rs` | preferences dialog, local/remote profile editors, the header-bar engine menu; writes straight through to `Config`. The local editor forks on `Analysis config`: managed by mirai, or a custom file — the file row appears only in the custom mode, which hides the batching-and-memory group because only the two thread values can still be overridden | `present`, `engine_menu_model`, `no_engine_status_page`, private `engines_page`, `refresh_profiles`, `editor_shell`, `open_editor`, `local_editor`, `tuned_row`, `cache_subtitle` |
| `src/harness.rs` | debug-only scripted-UI harness, inert unless `MIRAI_HARNESS` is set; renders through the app's own GSK renderer | `install`, `parse`, `activate`, `press` (matches a button's visible text: `GtkButton` label, then the first `gtk::Label` in its subtree so `adw::ButtonContent` works, then the tooltip), `shot` |

Non-Rust in `crates/mirai`: `resources/style.css` (`board-area`, `mirai-clock`, `mirai-readout`,
`mirai-winrate`, `mirai-movetree`) and `resources/mirai.gresource.xml`.

### Quick index

| I want to change… | open |
|---|---|
| how candidate moves are coloured | `widgets/board.rs` — `VISIT_RAMP` (the blue→green→red visit ramp) and `ramp_rgb`; applied in `draw_candidates`, which also draws the white ring on `order == 0` and picks label colour with `text_on` |
| which numbers appear in a candidate blob | `widgets/board.rs` — `draw_candidates` (win rate always; score lead and visits appear as the cell grows) |
| how many candidates are drawn or listed | `config.rs` — `AnalysisSettings::max_suggestions` |
| the ownership or policy heat map | `widgets/board.rs` — `BoardView`'s `snapshot`, then `blit` |
| a keyboard shortcut, or what an action does | `window.rs` — `install_actions` (action bodies and the accel table), `show_shortcuts` for the help window |
| live-analysis visit cap / report rate | `config.rs` — `AnalysisSettings`, consumed by `AppState::restart_analysis` |
| the search-speed reading (visits per second) | `app.rs` — `SpeedMeter`, fed from `set_report` and reset by `restart_analysis`; formatted by `util::visits_per_second`, shown in `panels/analysis.rs` (`Headline::speed`) and `window.rs` (`update_readout`) |
| the KataGo command line or its config overrides | `mirai-engine/src/local.rs` — `LocalEngine::spawn`, `override_config` |
| what goes into KataGo's analysis config | `mirai-engine/src/tuning.rs` — `EngineTuning::render` and its defaults; the profile side is `ProfileKind::tuning` in `config.rs`, the write site `build` in `engines.rs` |
| a KataGo query field, or how a response is read | `mirai-engine/src/query.rs` — `build_query`; `mirai-engine/src/decode.rs` — `decode_report` |
| a wire message or field | `mirai-proto/src/msg.rs`, `types.rs`, then [`PROTOCOL.md`](PROTOCOL.md) |
| scoring | `mirai-core/src/score.rs` — `score`; rule flags in `rules.rs` — `RuleSet::rules` |
| an SGF property | `mirai-core/src/sgf.rs` — `build` (read) and `write_node` (write) |
| the move-tree layout | `widgets/tree.rs` — `lay_out` |
| blunder colours or thresholds | `widgets/winrate.rs` — `Severity::color`, `severity_of_drop` |
| AI move choice or resignation | `play.rs` — `select_move_index`, `resign_check` |

---

## 4. Data model

### Point, Size, Color

```
19x19, Point index          GTP name            SGF value
  0   1   2 ...  18         A19 B19 ... T19     aa ba ... sa
 19  20  21 ...  37         A18 B18 ... T18     ab bb ... sb
  .                          .                   .
342 343 ...     360         A1  B1  ... T1      as bs ... ss
```

`Size` owns every conversion, so no caller does index arithmetic; `Neighbors` yields the four
orthogonal neighbours in a fixed order, skipping off-board ones. GTP is the only
bottom-left-origin representation in the tree; SGF shares our origin. `Point::PASS` is a sentinel
`u16`, not a separate variant, so a move is always one `u16` — anything indexing an array must
first check `is_pass()` or `Size::contains` (which rejects `PASS`).

### Board: Zobrist and ko

One `[[u64; 361]; 2]` table (colour-major, then point index) seeded from a fixed constant through
`SplitMix64`. The fixed seed is a contract, not a detail: hashes must be reproducible across runs
and machines. The private `put` XORs the old stone out and the new one in, so the hash is
incremental and `Board::play` never rehashes. Turn parity is folded in at read time —
`situational_hash(to_play)` is the positional hash, XOR `WHITE_TO_MOVE_HASH` for White — so both
superko histories come off one hash pipeline.

Superko is split deliberately across two levels:

| ko rule | enforced by | how |
|---|---|---|
| `Ko::Simple` | `Board` | `ko_ban`, set only when a move captures exactly one stone leaving a single-stone, single-liberty chain; checked in `is_legal` and `play` |
| `Ko::Positional` | `GameTree` | candidate board's Zobrist looked up in `Position::hash_history` |
| `Ko::Situational` | `GameTree` | candidate's `situational_hash(other colour)` looked up in `Position::situational_history` |

`Board::is_legal` cannot see superko — that needs a whole game's history — so a legality check
outside the tree is incomplete by design. A pass cannot repeat a position by itself and is exempt.
Suicide is legal only under `multi_stone_suicide` and only for chains larger than one stone.

### GameTree: arena, tombstones, revision, position cache

Nodes are addressed by `NodeId`, never by reference — no `Rc`, no lifetimes — so an id can be
parked in a widget, a `Cell` or a `Blunder` without borrowing the tree. `delete_branch` unlinks a
subtree and then *tombstones* its nodes, so ids are never reused and never shift, and every
surviving `NodeId` held elsewhere stays correct. Consequences:

- `node(id)` panics on a tombstone; use `get`/`contains` wherever deletion is possible.
- `len()` counts live nodes, so it diverges from the arena's length after a delete.
- Deleting the root is a no-op; the cached position is dropped if it pointed into the subtree.
- `revision()` is bumped by every mutation so a view can test staleness in O(1) — `MoveTreeView`
  relayouts only when it moves. A mutating method that forgets to bump it silently freezes that
  widget.

`Position` is derived state, never stored on a node: board, side to move, move number, and the two
hash histories. `position(id)` has three cases:

```
cache is (id, pos)            -> return it, zero work
cache is (parent(id), pos)    -> apply one node                <- O(1): the arrow-key path
otherwise                     -> replay path_to(id) from empty
```

Forward navigation is therefore one `Board::play` plus two hash pushes rather than a replay.
Backwards is a full replay: `Board::play` records nothing that could reverse a capture, and making
positions undoable would cost more than the few hundred microseconds a replay takes. `position`
takes `&mut self`, which is why `AppState` exposes `with_tree_cached` for read-only callers.

The private `step` applies setup stones, the handicap turn flip, the move, then the `PL`/`MN`
overrides, and appends both hashes. Replay never rejects an illegal move: a stored line is history,
not a legality question. `Node` is all-public data — the tree owns structure, not content — and
`children[0]` is the main line.

### NodeAnalysis and Candidate

`NodeAnalysis` is the *stored* form of an evaluation: dequantised, Black-perspective, truncated to
`AnalysisSettings::max_suggestions`, produced only by `util::analysis_of`, with ownership left
quantised at one byte per point. It exists so the win-rate graph and move tree can draw a whole
game after the live report for a node is gone. The live `Report` — all fields, still quantised —
lives separately in `AppState::last_report()` and is discarded the moment the cursor moves.

```
KataGo JSON ─decode_report─> Report (quantised, Black) ─analysis_of─> NodeAnalysis (f32, Black)
                              │                                        │
                              └─ AppState::last_report()               └─ Node::analysis
                                 board candidates, analysis panel         winrate graph, move
                                                                          tree, SGF `MRAI`
```

### SGF mapping

Read in `build`, written in `write_node` (`mirai-core/src/sgf.rs`).

| SGF | model | note |
|---|---|---|
| `SZ` | `GameInfo::size` | `n` or `w:h`; absent means 19x19 |
| `RU` | `GameInfo::rules` | via `RuleSet::from_katago_name`; unknown falls back to the default ruleset |
| `KM` `HA` `RE` `DT` `EV` `TM` `OT` `PB`/`PW` `BR`/`WR` | `GameInfo` fields | — |
| `B` / `W` | `Node::mv` | an empty value, or `tt` on boards ≤ 19, is a pass |
| `AB` / `AW` / `AE` | `Node::setup` | point lists; `aa:cc` rectangles expanded on read |
| `C` `PL` `MN` | `comment`, `to_play_override`, `move_number_override` | — |
| `LB` | `Marks::labels` | composed `coord:text`, so `:` is escaped on write |
| `TR` `SQ` `CR` `MA` | `Marks` shape lists | — |
| `MRAI` | every node's `analysis` | mirai's own, see below |
| anything else | `Node::unknown_props` | verbatim |

**Unknown-property preservation** is why there is no SGF crate here. Any property mirai does not
model is kept on its node and written back at the end of that node, escaped but byte-preserved, so
a round trip does not destroy another program's analysis blobs (LizzieYzy's `LZ`/`LZOP`/`DZ`). The
one exception is deliberate: root properties mirai models or regenerates must not be echoed back
or they appear twice. `is_root_info_prop` is that list, applied only at the root; **a new root
property must be added there.**

**`MRAI`** carries cached analysis on one root property:

```
MRAI[ base64( zstd( MRAI_VERSION ++ postcard(Vec<(u32, NodeAnalysis)>) ) ) ]
                                     └─ u32 = the node's index in document order
```

The index contract is load-bearing: `collect_analysis` walks in exactly the order `write_sequence`
emits and `build` creates nodes in document pre-order, so the recorded index is the `NodeId` a
reload assigns. Decoding is best-effort — foreign, truncated or future-versioned blobs mean "no
analysis", and a vanished index is skipped — and writing is opt-in via
`UiSettings::save_analysis_in_sgf`, off by default. Two parser facts worth knowing before touching
it: the charset is located by scanning raw bytes for a `CA[` that starts a property identifier,
because the text cannot be decoded until it is known; and nesting is capped by `MAX_DEPTH` against
hostile files.

---

## 5. Concurrency and threading

### Two schedulers, one process

| | GTK main context (main thread) | tokio multi-thread runtime (`mirai-rt` threads) |
|---|---|---|
| owns | every widget; `AppState` (a `glib::Object`, not `Send`); the `GameTree` in a `RefCell` | `LocalEngine`'s writer/reader/supervisor/stderr tasks; `RemoteEngine`'s connection task, control reader, stream acceptor and per-subscription readers |
| futures | `glib::spawn_future_local`: the analysis pump, batch workers, the score pump, the play thinker | everything else |

One runtime is built in `main` and never rebuilt; only its `Handle` is stored, in `AppState`
(`AppState::runtime()`). After `application.run()` returns, `main` calls `shutdown_timeout` so
dropped engines get a moment to let their KataGo processes exit. Nothing on a tokio thread ever
touches `AppState` or the tree.

### The two crossings

**GUI → runtime for a one-shot result:** `runtime().spawn` + a `oneshot` +
`glib::spawn_future_local` to land the result back on the main context. The canonical instance is
`AppState::activate_profile`. Its `activation` counter is mandatory, not decorative: a local
KataGo takes seconds to load its net, so an older activation can complete *after* a newer one and
would otherwise install itself over it. Copy the whole pattern, counter included, for any new
slow operation that installs state.

**Runtime → GUI for a stream:** `Engine::subscribe` is synchronous — it returns a handle
immediately and the engine's own tasks do the work — and the GUI then awaits
`Subscription::next()` inside a `glib::spawn_future_local`, so every report is handled on the main
thread.

### What must not happen on the main thread

- **No `block_on`, no `blocking_recv`, no thread joins.** There are none in `crates/mirai`; the
  oneshot pattern above is the replacement.
- **No engine startup inline.** `LocalEngine::spawn` waits for KataGo's handshake, which on a cold
  OpenCL install includes autotuning and is allowed minutes.
- **No blocking on a `Subscription`.** `finish()` is async and is used by the probe and the
  server, never by the GUI.
- SGF parsing *is* inline and fast enough for real records; a multi-megabyte collection would
  stutter. `[INFERENCE]`

### The cancellation chain (INV-3)

There is one cancellation mechanism — **drop the `Subscription`** — and everything else is
plumbing that leads to that drop.

```
navigate / toggle / close window
        │
        v  AppState::restart_analysis: pump.take() -> glib::JoinHandle::abort()
        │  aborting the future drops its captured locals
        v  Subscription dropped -> CancelGuard::drop runs the boxed closure
   ┌────┴───────────────────────┐
   v                            v
LocalEngine: Inner::cancel      RemoteEngine: Cmd::Cancel
  terminate action on stdin       ClientMsg::Cancel  +  stop_sending on that
  (query.rs terminate_query)      subscription's stream
  KataGo stops the search         server resets the stream; buffered stale reports
                                  are dropped in the network stack, never decoded
```

`AppState`'s `generation` counter is a belt-and-braces guard so a pump already inside
`next().await` exits instead of writing a stale report; it is not the cancellation mechanism.
The same chain is what makes server-side cleanup free: `session.rs`'s `pump` returning drops its
`Subscription`, so a client that simply disappears stops KataGo. Measured: a client SIGINT makes
the server drop the subscription in under a millisecond and KataGo falls to 0% CPU.

Other participants in the same discipline: the score pump (`window.rs`), the batch workers
(`batch.rs`) and the play-mode thinker (`play.rs`) are all `glib::JoinHandle`s that are aborted
rather than signalled.

### One live-analysis cycle: pressing Left

```
 user     GTK main context                                 tokio                    KataGo
  │
 Left ──> accel "win.prev" -> SimpleAction
  │         with_ui(weak, …)                    window.rs   (Weak upgrade, INV-8)
  │         AppState::go_prev -> go_back(1)
  │           borrow tree, walk parents, DROP the borrow    (INV-10)
  │         AppState::set_cursor
  │           cursor := id;  last_report := None
  │           emit cursor-changed ─┬─ BoardView    : clear hover/pin, queue_draw
  │           emit report ─────────┤  MoveTreeView : queue_draw + scroll to cursor
  │                                │  WinrateGraph : queue_draw
  │                                │  AnalysisPanel: rebuild (no report yet)
  │                                └─ window       : flush+load comment, scale,
  │                                                  readout, clocks
  │         restart_analysis
  │           abort old pump ──> Subscription dropped ──> terminate ──────────> search stops
  │           generation += 1
  │           request_for_cursor: whole position + Want flags (INV-4)
  │           engine.subscribe(req) ── sync ──> watch channel, id in DashMap,
  │                                            query line to stdin ──────────> query
  │           glib::spawn_future_local(pump)
  │                                                                      (~100 ms later)
  │                                            reader task: classify, decode_report,
  │                                            send_replace(Report)  <──── isDuringSearch
  │  pump wakes on sub.next()
  │    generation still matches?
  │    AppState::set_report: store report; borrow_mut tree, cache analysis_of(...),
  │                          DROP the borrow, then emit report      (INV-10)
  │           emit report ─┬─ BoardView / WinrateGraph : queue_draw
  │                        ├─ AnalysisPanel            : splice rows in place
  │                        └─ window                   : update_readout
  │  GTK frame clock -> BoardView::snapshot
  │    static node │ heat map │ stones+territory │ numbers/marks │ candidates
  v  redrawn board          … repeats at ~10 Hz until Done, then the pump idles
```

Two properties to internalise: the board never asks an engine for anything — it reads `AppState`
— and a report that arrives after the cursor moved cannot be drawn, because the pump that would
deliver it was aborted and its generation no longer matches.

---

## 6. Engine layer

`Engine` has two methods, `subscribe` and `describe`, plus a blanket impl for `Arc<T>` so the GUI
can hold `Arc<dyn Engine>`. `subscribe` is deliberately **not** async — the caller gets a handle
at once and the engine's own tasks do the work, which is what lets a GTK signal handler start an
analysis without awaiting; errors detected before dispatch come back as an already-failed
subscription rather than a `Result`, so callers have one code path.

`SubEvent` is `Pending` (the `watch` channel's initial value), `Report`, and the terminal `Done`
and `Failed`. `current` never awaits and is always meaningful; `next` awaits a change and yields
`None` once the engine side is gone; `finish` checks `current` first so an already-finished
subscription cannot hang. `CancelGuard` holds the boxed closure that `Drop` runs — that closure
*is* INV-3. `EngineError` variants are acted on: `Startup` and `EngineExited` make the GUI clear
the active engine, the rest are reported and survivable.

### LocalEngine

The command line is built in `LocalEngine::spawn`:

```
<katago> analysis -model <model> -config <config> -quit-without-waiting
                  -override-config <k=v,k=v,…>
```

stdio piped, `kill_on_drop` set. `override_config` always sets:

| override | value | why |
|---|---|---|
| `reportAnalysisWinratesAs` | `BLACK` | **INV-2**; everything downstream assumes it |
| `logDir` | the profile's log directory | one KataGo log file per run |
| `logToStderr` | `false` | stderr is ours — we tail it for diagnostics |
| `logAllRequests`, `logAllResponses` | `false` | detail stays in KataGo's own log |

plus `numAnalysisThreads`, `numSearchThreadsPerAnalysisThread` and `nnCacheSizePowerOfTwo` when
the caller set them — which the GUI does only for a custom config file; with mirai's generated
config (§2.2) that file is the single source of those values and no thread override is passed.
KataGo splits this value on commas, so a value containing one is rejected up front with a
`Startup` error rather than producing a mangled config.

**Handshake as readiness probe.** Two action queries (`query_version`, `query_models`) are written
before anything else and `handshake` reads until both are answered; KataGo only answers once its
model is loaded, so "handshake done" *is* "ready". Non-JSON banner lines are ignored. A
`query_models` rejection is tolerated with a warning (the action is newer than the rest of the
protocol); any other query error or engine fault aborts the start. `has_human_model` is derived
here and gates the Human-like strength option. The timeout is `LocalEngineConfig::startup_timeout`
— minutes by default, because a cold OpenCL install tunes itself — and on failure the child is
killed and the error carries the tail of stderr.

**Three tasks plus a stderr drain.** The *writer* drains an unbounded channel of query lines and
flushes once per batch; returning closes KataGo's stdin, which with `-quit-without-waiting` asks
it to quit. The *reader* line-splits stdout into `Inner::handle`, holding only a `Weak`, so it
exits when the engine handle drops. The *supervisor* selects on process exit and a shutdown
oneshot: on handle drop it waits a short grace period and then kills, and on exit it marks the
engine dead and fails every live subscription. The *stderr drain* logs each line and keeps a
bounded ring for error messages.

**Routing.** A `DashMap` from a monotonic query id to the `watch::Sender` plus the three facts
needed to decode that query's responses (size, turn, side to move). The id is stringified into the
query and echoed back by KataGo. `DashMap` rather than a mutex because `subscribe` runs on the GTK
thread while the reader task delivers. A terminal event removes the entry before sending; `cancel`
instead *keeps* the entry and marks it terminating, so the tail of a terminated search is still
routed and discarded correctly, and a response for an unknown id is a trace, not an error. A
terminated query that never searched still terminates cleanly, as an empty `Done`.

### RemoteEngine

Same trait, same reports, over QUIC. One background task owns the connection, the control stream
and the subscription table; the handle only sends commands down an unbounded channel, which is how
`subscribe` stays synchronous. Per-connection reader tasks feed the owner: one for the control
stream, one acceptor for server-opened unidirectional streams, and one per subscription that reads
the 4-byte id preamble and then `SubMsg` frames. Dropping the handle drops a oneshot, which is what
stops the task.

`handshake` connects (TOFU-verified), sends `Hello`, awaits `Welcome`, then resolves the requested
engine name against the server's list. Its error classification is load-bearing:
`EngineError::Protocol` means reconnecting cannot help (bad token, version mismatch, fingerprint
mismatch), `Disconnected` means retry. Backoff is 0.5 s, 1 s, 2 s, 4 s then capped; while waiting,
commands are still answered so the handle never deadlocks. `RemoteStatus::Failed` is sticky for
unfixable causes so the UI can say something truthful instead of spinning, while the task keeps
retrying at maximum backoff in case the server is fixed and restarted.

**No subscription replay.** On connection loss every live subscription fails with `Disconnected`
and is forgotten, and requests made while the link is down fail immediately rather than queueing.
That is INV-4 again: the GUI knows which node the user is looking at *now* and re-requests that
one when the banner clears, whereas a queued stale query would only burn the server's search
threads. Reconnects reuse the accepted fingerprint and the *resolved* engine name, so a server
that gains engines later cannot silently switch the client to a different one.

### Local vs remote

| | LocalEngine | RemoteEngine |
|---|---|---|
| transport | subprocess stdio, one JSON line per message | QUIC: one bidi control stream + one uni stream per subscription |
| routing | `DashMap` keyed by query id | `HashMap` owned by the connection task, keyed by subscription id |
| cancel | `terminate` action | `Cancel` message **and** `stop_sending` |
| one query fails | KataGo per-query error | `SubMsg::Failed` / `ServerMsg::Error` with a sub id |
| engine fails | process exit → fail all | connection loss → fail all, then reconnect |
| recovery | none; the GUI clears the engine | automatic with backoff, never replayed |
| `describe()` | from the startup handshake | from `Welcome`, narrowed to the picked engine |

---

## 7. GUI architecture

### AppState is the only source of truth (INV-7)

`AppState` is a `glib::Object` subclass holding the game tree, the cursor, the config, the runtime
handle, the active engine, the last report and the live-analysis pump. Widgets never hold pointers
to each other: they take an `AppState` clone (a GObject ref), read from it, and subscribe to its
notifications. Anything that looks like widget-to-widget coupling is either a hook installed once
by `window::present` (board ↔ analysis panel PV preview, batch → panel blunder list, play → batch)
or a bug.

Two implications: a new piece of shared state belongs on `AppState`, not on a widget; and a widget
that needs to *cause* something calls an `AppState` method or activates a `win.*` action.

**Properties** (bindable, `notify::`-observable):

| property | fires when | notes |
|---|---|---|
| `live-analysis` | toggled by the user or an action | custom setter; also calls `restart_analysis` |
| `ownership-overlay` | toggled | custom setter; turns `policy-overlay` off, notifying it |
| `policy-overlay` | toggled | custom setter; turns `ownership-overlay` off and restarts analysis, because the policy array is only requested when the overlay is on |
| `show-coordinates`, `show-move-numbers` | toggled | derive-generated setters |
| `engine-label`, `status`, `busy`, `file-path`, `modified` | set by the code that owns the state | derive-generated setters |

`explicit_notify` belongs **only** on the three properties with hand-written setters that emit
`notify_*()` themselves. On a derive-generated setter it silently disables notification and breaks
every `notify::` handler and `bind_property` target that depends on it.

**Signals** (names live in `app::signal`, so a typo is a missing constant rather than a silent
no-op):

| signal | emitted by | consumed by |
|---|---|---|
| `tree-changed` | `with_tree_mut`, `set_tree`, and `batch.rs` after a sweep | board (geometry may have changed), move tree (relayout), winrate graph, analysis panel, window (move scale, title) |
| `cursor-changed` | `set_cursor`, `play_move`, `set_tree` | board (clears hover/pin), move tree (redraw + scroll), winrate graph, analysis panel, window (comment flush/load, scale, readout, clocks) |
| `report` | `set_report`, and by `set_cursor`/`play_move` to announce the *cleared* report | board, winrate graph, analysis panel, window readout |
| `engine-changed` | `set_engine`, and `prefs.rs` after editing profiles | window (engine menu, Analysis page, subtitle) |
| `toast(String)` | `AppState::toast` from anywhere | window, which turns it into an `adw::Toast` |
| `play-changed` | `PlayController` via `notify_play_changed` | window (clocks) |
| `batch-progress(u32, u32)` | `BatchAnalysis` via `notify_batch_progress` | nothing in-tree today; the sweep updates its own banner. Treat it as the hook to use if progress needs to appear elsewhere |

Ordering rule: emit `cursor-changed` before `report` (as `set_cursor` does), because handlers of
`report` assume the cursor is already current.

### Widget tree

```
adw::ApplicationWindow
└ adw::ToastOverlay                     ← every toast lands here
  └ adw::ToolbarView
    ├ top:    adw::HeaderBar            open · save · live-analysis toggle · engine menu
    │                                   · title · clocks · New game · primary menu
    ├ bottom: gtk::Box                  first/prev/next/last · branch up/down
    │                                   · move scale · readout
    └ content: adw::OverlaySplitView    sidebar collapses under a breakpoint
      ├ content: gtk::Box
      │   ├ adw::Banner                 batch-analysis progress + Cancel
      │   └ gtk::Paned (vertical)
      │       ├ BoardView               expands; all extra space goes here
      │       └ WinrateGraph            fixed height request, still user-draggable
      └ sidebar: adw::ToolbarView
          ├ adw::HeaderBar              title widget IS the ViewSwitcher
          └ adw::ViewStack
              ├ Analysis  gtk::Stack{ AnalysisPanel | no-engine StatusPage }
              ├ Moves     MoveTreeView in a ScrolledWindow
              └ Comment   gtk::TextView
```

The graph must use `resize_end_child(false)` plus a size request rather than a hardcoded `Paned`
position, and the sidebar header must keep `show_title` on — it hides the title *widget*, which is
the switcher.

Every user-triggerable operation is a `win.*` action registered in `install_actions`, so the menu,
the buttons, the accelerators, the shortcuts window and the debug harness all drive the same code.
Add an action there, with its accelerator in the same table; never wire a button's `clicked`
directly to logic.

### The `Rc<Ui>` rule (INV-8)

`Ui` owns the window and everything hanging off it. Handlers hang off widgets and off the
`AppState` that `Ui` itself owns, so a strong `Rc<Ui>` inside a long-lived handler is a cycle
nothing breaks: it would keep the window, its engine, the KataGo process behind that engine and
the runtime handle alive for the rest of the session.

The rule, as a contributor follows it:

1. In a long-lived handler — a GObject signal, a `notify::`, a `GAction`, a `glib::timeout` —
   capture `Rc::downgrade(ui)` and enter through `with_ui`, which upgrades or does nothing.
2. Exactly one strong `Rc<Ui>` exists, parked in a `Cell<Option<Rc<Ui>>>` inside the
   `close-request` handler in `connect_close`. It **takes** the value out rather than borrowing,
   so the `Ui` — and with it `AppState`, the engine and the runtime handle — is dropped as the
   window closes. That is the single release point.
3. A transient capture may be strong, and each one in the tree says why in a comment: file-dialog
   futures, dialog responses, the clipboard paste future and the score pump all end on their own
   and release the clone.
4. A strong capture in a long-lived handler is a leak, not a style preference: review rejects it.
   `with_ui` is generic over the pointee purely so the discipline is unit-testable without a
   display.

`connect_close` also defines the shutdown order: flush the comment, delete this window's autosave
(a window that closed cleanly leaves nothing to restore), cancel the batch, stop play mode, abort
the score pump, turn live analysis **off** (so the pump releases its subscription), save the
config, clear the engine, then drop.

### More than one window

`activate` and `open` both call `window::present`, and `open` calls it once per file, so several
windows in one process is a normal state, not an edge case. Each owns a complete `AppState`
(INV-7 is per window); three things are process-wide and each had to be made safe for it:

| Shared | Rule |
|---|---|
| Engines | `EnginePool` in `main`, keyed on the whole `EngineProfile`. A window adopts a running engine synchronously through `running`, or joins an in-flight start through `acquire`; entries are `Weak`, so KataGo exits with the last window using it. Sharing is only sound because of INV-4 — no engine-side session state — and because `LocalEngine` already multiplexes queries by id |
| `config.toml` | Each window holds the `Config` it loaded, so writing the whole thing back would revert another window's edits. `Config::save_merged` applies only this window's own diff onto the file as it stands |
| Autosave | One file per window, `autosave-<pid>-<start>-<n>.sgf`. A clean close deletes it; anything found at startup is therefore a crash leftover, and each new window is offered one, most recent first. This replaced the single `autosave.sgf` plus `clean-exit` flag, which could not say which window had exited |

### RefCell discipline (INV-10)

`AppState` keeps the tree in a `RefCell`. Handlers of `tree-changed`, `cursor-changed` and
`report` all borrow it again — that is their job — so **a borrow must be released before a signal
is emitted**, or a user action turns into a panic.

Where it is enforced, and the patterns to copy:

- `with_tree_mut` scopes the mutable borrow to the closure and emits `tree-changed` only after it
  returns. Route edits through it rather than reaching for `imp().tree`.
- `set_report` scopes the `borrow_mut` that caches the analysis in a block, then emits.
- `set_tree` scopes the swap in a block, then emits both signals.
- Navigation helpers (`go_last`, `go_back`, `go_forward`, `go_sibling`) compute the target inside a
  block or explicitly `drop(tree)` before calling `set_cursor`.
- Widgets do the same on the read side: `BoardView::snapshot` pulls what it needs out of short
  scoped borrows rather than holding one across a draw.

Reviewer's heuristic: if a `Ref`/`RefMut` binding is alive on the same statement as an
`emit_by_name`, a `set_cursor`, a `set_report` or a `toast`, it is a latent panic.

---

## 8. Invariants — the detailed reference

[`../../AGENTS.md`](../../AGENTS.md) states these in summary form. This is where each is enforced
and how a violation shows up.

| # | invariant | enforced in | how you would notice it broken |
|---|---|---|---|
| **INV-1** | Point encoding: `index = y * width + x`, `y = 0` is the **top** row, `PASS` is the maximum `u16`, boards are 2..=19 per side. KataGo's `ownership`/`policy` index identically to the board array; never add a remap | `mirai-core/src/point.rs` (`Point`, `Size`, `MIN_DIM`/`MAX_DIM`); consumed unremapped by `BoardView::snapshot` | The ownership overlay is mirrored or transposed — dark shading sits over the opponent's group. This is the canary check for the whole encoding design |
| **INV-2** | Perspective: KataGo runs `reportAnalysisWinratesAs=BLACK`; everything stored and transmitted is Black's, converted only at display time via `winrate_for` / `score_lead_for` | `override_config` in `mirai-engine/src/local.rs` sets it; `mirai-proto/src/types.rs` provides the only converters; `util::analysis_of` keeps stored values Black | A win rate that reads `1 - x`: plausible on screen, so check it as an *invariant*, not by eye. A komi sweep must be monotonically decreasing for Black |
| **INV-3** | Cancellation: dropping a `Subscription` is the only mechanism. Local enqueues a KataGo `terminate`; remote sends `Cancel` **and** `stop_sending` on that subscription's stream | `CancelGuard` in `mirai-engine/src/lib.rs`; the closures installed by `LocalEngine::subscribe` and `RemoteEngine::subscribe`; `AppState::restart_analysis` and `session.rs`'s `pump` are the drop sites | KataGo keeps burning CPU with no subscription open, or stale reports appear for the previous position. Prove it by sampling a CPU *rate*, not a total |
| **INV-4** | Stateless queries: every request carries its whole position; there is no engine-side session state | `AnalyzeReq` in `mirai-proto/src/types.rs`; built in one place, `AppState::request_for_node`; `build_query` never emits `analyzeTurns` | An analysis that is correct only after visiting nodes in a particular order; a remote client that needs resynchronising after a reconnect |
| **INV-5** | Komi crosses the wire as a doubled integer (`komi_x2`), because KataGo accepts only integer or half-integer komi | `AnalyzeReq` (`komi_x2`, `komi()`) in `mirai-proto/src/types.rs` | Komi silently rounded, or a rejected query from a fractional komi |
| **INV-6** | Quantisation: wire floats are fixed-point; every scale lives in `mirai-proto/src/types.rs`. Round-trip error budget: winrate ≤ 1e-4, score lead ≤ 0.02 pt, ownership ≤ 0.005 | the `q*`/`dq*` helpers and `*_SCALE` constants; both engine paths use them, so reports are bit-identical | `mirai-proto/tests/wire_size.rs` fails on frame size or error budget. Changing a scale means bumping `PROTO_VERSION` and updating that test and [`PROTOCOL.md`](PROTOCOL.md) |
| **INV-7** | One source of truth: `AppState` owns application state; widgets read it and listen to its signals, never to each other | `mirai/src/app.rs`; the cross-widget hooks installed once in `window::present` | A widget holding another widget; state that two widgets disagree about after an edit |
| **INV-8** | `Rc<Ui>` discipline: long-lived handlers capture `Weak<Ui>` through `with_ui`; exactly one strong `Rc<Ui>`, parked in a `Cell` and taken by `close-request` | `with_ui` and `connect_close` in `mirai/src/window.rs` | Closing the window leaves KataGo running: the `Ui` was never dropped, so the engine was never dropped |
| **INV-9** | Rendering: board, win-rate graph and move tree are `gtk::Widget` subclasses drawn with `gsk` in `snapshot()`. No `DrawingArea`, no cairo | `mirai/src/widgets/` | A cairo context or a `DrawingArea` appearing in a review diff |
| **INV-10** | Release every `RefCell` borrow of the tree **before** emitting a signal | `with_tree_mut`, `set_tree`, `set_report` and the navigation helpers in `mirai/src/app.rs` | `already borrowed: BorrowMutError` in the user's hands, on a specific action — not in tests, which rarely have the full handler set attached |

---

## 9. Extension recipes

### A new engine backend

1. Implement `Engine` in a new module of `mirai-engine`; return `Subscription::new(rx, guard)`
   where the guard performs your cancellation (INV-3), and `Subscription::failed(..)` for
   pre-dispatch errors.
2. Produce `Report`s through `mirai-proto`'s quantisers, or reuse `decode_report` if the source is
   KataGo JSON. Do not invent a second report type (INV-6).
3. Re-export the type from `mirai-engine/src/lib.rs`.
4. Add a `ProfileKind` variant in `mirai/src/config.rs` (`#[serde(tag = "kind")]`, so the variant
   name is the on-disk discriminant).
5. Handle that variant in `build` in `mirai/src/engines.rs`, and check that `key` still tells
   two profiles of the new kind apart — an over-broad key shares the wrong engine.
6. Add an editor subpage in `mirai/src/prefs.rs` alongside the local and remote ones.
7. Verify with `examples/probe.rs` before touching the GUI — it exercises the trait with no GTK.

### A new sidebar panel

1. Create `mirai/src/panels/<name>.rs`; a `gtk::Box` subclass built from stock widgets is enough
   (INV-9 applies only to the custom-drawn widgets).
2. Re-export it from `panels/mod.rs`.
3. Construct it in `window::present` with the `AppState`, and add it to the sidebar `ViewStack`
   with `add_titled_with_icon`.
4. Subscribe to `AppState` signals inside the panel; do not accept references to other widgets
   (INV-7). If the window must wire it to another widget, install a hook the way `present` does
   for the PV preview.
5. Store any user-visible option in `UiSettings` in `mirai/src/config.rs` and mirror it in
   `AppState::save_config`.

### A new board overlay

1. If the overlay needs extra engine data, add the flag to `Want` in `mirai-proto/src/types.rs`,
   map it in `build_query`, and decode it in `decode_report` into a new `Report` field.
2. Add a `bool` property to `AppState`. If enabling it changes the request, give it a hand-written
   setter that calls `restart_analysis` and `notify_*()` itself, and mark it `explicit_notify` —
   copy `policy_overlay` exactly. Otherwise use the derive-generated setter and **do not** add the
   flag.
3. Add the request bit in `AppState::restart_analysis`.
4. Draw it in `BoardView`'s `snapshot`; for a per-point heat map build an RGBA8 premultiplied
   buffer indexed by `Point` and hand it to `blit` — no remapping (INV-1).
5. Add the property name to `BoardView::observe` so a toggle redraws, and clear the static layer
   too if your overlay changes the board's geometry or padding.
6. Add a `win.toggle-*` action and accelerator in `install_actions`, and a `UiSettings` field so it
   survives a restart.

### A new protocol message

1. **Read [`PROTOCOL.md`](PROTOCOL.md) first** — it is normative, and it must be updated in the
   same change.
2. Add the variant to `ClientMsg`, `ServerMsg` or `SubMsg` in `mirai-proto/src/msg.rs`. Postcard
   encodes an enum by declaration index, so **append**; inserting or reordering renumbers every
   later variant and breaks every existing peer.
3. Handle it server-side in `session_loop` (`mirai-server/src/session.rs`) and client-side in
   `handle_int`/`handle_cmd` (`mirai-engine/src/remote.rs`). An unknown message must be an error
   with an `ErrCode`, never a panic.
4. Bump `PROTO_VERSION` if an old peer would misread the stream; the handshake rejects a mismatch
   with `ErrCode::BadVersion`.
5. If the message carries a report or a large payload, check `MAX_FRAME` and extend
   `tests/wire_size.rs`.

### A new SGF property

1. Decide where it lives: whole-game metadata goes on `GameInfo`, per-node data on `Node`
   (`mirai-core/src/tree.rs`).
2. Parse it in `build` in `mirai-core/src/sgf.rs`, next to the existing arm for its shape (single
   value, point list, or composed value).
3. Write it in `write_node`, before the `unknown_props` loop so ordering stays stable.
4. **If it is a root property, add it to `is_root_info_prop`.** Otherwise it will be captured into
   `unknown_props` as well and written twice.
5. Add a round-trip test: parse a file containing it, write, parse again, compare — and keep an
   unknown property in the same fixture so the preservation contract stays covered.

---

## 10. Known simplifications

Deliberate. Each is commented at the source; do not "fix" one by accident.

| simplification | where | why it is right |
|---|---|---|
| **Territory scoring counts territory plus the prisoners you hold** — captures made during the game and the opponent's dead stones in your area — rather than subtracting the prisoners you lost | the `Scoring::Territory` arm of `score` | Both formulations give the same margin, but the mirror one displays totals lower than any Japanese scorer would show. This one reproduces conventional totals |
| **Seki detection is narrower than the rules describe.** A chain counts as being in seki only if it shares an empty region with the enemy *and* owns no region bigger than eye space | the `in_seki` computation in `score`, with `MAX_EYE` | The bare "adjacent to a shared region" test also catches every settled group standing next to an unfilled dame, and would then tax ordinary territory |
| **`Tax::All` (stone scoring) is approximated** with the `Tax::Seki` result and reports `approximate = true` | `score`, `ScoreResult::approximate` | Full stone-scoring tax is rarely used; the UI labels the result an estimate rather than pretending to exactness |
| **`has_button` is a flat +0.5 to White** | end of `score` | mirai never plays the button, so there is no game state that could decide who takes it |
| **Dead stones are decided per chain by majority vote** over an ownership threshold, not per stone | `DeadSet::from_ownership` | Per-stone marking produces speckled half-dead groups on unsettled boundaries |
| **Setup stones become `initialStones` only when they precede every move**; a later setup node falls back to sending the replayed board with no move history | `AppState::request_for_node` | The KataGo query has no way to express "setup in the middle of a move list". The analysis is still correct, just without move history |
| **Replay never rejects an illegal move** | the private `step` in `mirai-core/src/tree.rs` | A stored line is history. Refusing to display a file because it contains an illegal move would be worse than showing it |
| **`max_board` is fixed at 19x19** | `LocalEngine::spawn`'s `EngineDesc` | Stock KataGo builds cap `MAX_LEN` at 19; a larger board would need a custom build, and nothing else in the tree assumes otherwise |
| **SHA-256 is implemented in-tree** | `mirai-proto/src/sha256.rs` | ~60 lines on no hot path, versus a dependency and its API churn. Pinned by the standard test vectors |
