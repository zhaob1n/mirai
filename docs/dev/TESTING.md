<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Testing and verification

How to prove a change to mirai works.
[architecture](ARCHITECTURE.md) · [protocol](PROTOCOL.md) ·
[retrospective](../archive/RETROSPECTIVE.md) ·
[user guide](../user/GUIDE.md) · [AGENTS.md](../../AGENTS.md).

Section 5 is the reason this file exists: this is a GTK4 app on Wayland, where external
screen capture returns black frames, so the application screenshots itself.

## 1. Quick reference

```sh
cargo test --workspace                                   # must be green
cargo clippy --workspace --all-targets -- -D warnings    # must exit 0
cargo build --release --workspace
cargo test -p mirai-proto --test wire_size -- --nocapture   # prints the measured byte budget
```

| Never run casually | Why |
|---|---|
| `cargo fmt` | Formatting here is hand-placed in the query and quantisation tables; a blanket reformat buries the real diff. Format only the lines you touched. |
| `cargo clippy --fix` | Rewrites files you did not read and may be held by another agent. Fix lints individually. |

There are no doc-tests: the `//!` examples in `harness.rs` and `probe.rs` are plain `text`
fences and are never compiled. They can rot; treat them as prose.

## 2. What is covered where

One row per source file. **Bold** tests are the ones that would catch a real regression;
the rest defend the surrounding behaviour and are named so you can find them.

| File | Contracts defended | Load-bearing tests |
|---|---|---|
| **mirai-core** | | |
| `point.rs` | INV-1 encoding: `y = 0` is the top row, A19 is index 0, neighbours never wrap a row; rectangular geometry | `gtp_corners_19x19`, `sgf_corners_19x19`, `rectangular_geometry`, `neighbors_clip_at_edges` |
| `rules.rs` | Each `RuleSet` maps to the exact KataGo rules string and the right `(ko, suicide, whb)` tuple | `katago_names_round_trip`, `ruleset_tuples_match_katago` |
| `board.rs` | Ko released after exactly one move; suicide legality per ruleset — the one place rulesets genuinely disagree; capture counts; Zobrist order-independent and turn-aware, which superko depends on | **`simple_ko_is_banned_for_exactly_one_move`**, **`single_stone_suicide_illegal_under_every_ruleset`**, **`multi_stone_suicide_follows_the_ruleset`**, **`is_legal_agrees_with_play_over_random_games`** (randomised differential) |
| `tree.rs` | Positional and situational superko really differ; simple ko stays in `Board`; passes never enter the superko set; edits keep `NodeId`s valid; handicap flips the first player | **`positional_superko_rejects_what_situational_allows`**, **`incremental_stepping_matches_a_cold_replay`** (the proof that fast navigation equals a cold replay) |
| `score.rs` | Each side scores territory **plus the prisoners it holds** — the margin alone is not enough, both totals must read as a human scorer would write them; per-chain dead toggling; seeding dead stones from ownership needs a confident majority | **`endgame_area_and_territory`** (one 9×9 endgame under both rule families), `handicap_compensation_variants`, `from_ownership_needs_a_confident_majority` |
| `handicap.rs`, `clock.rs` | Conventional stone placement; time allocation never spends over half the main time and mostly consumes a byo-yomi period | `nine_stones_on_19x19_are_the_star_points`, `main_time_branch_spends_a_twentieth_and_never_over_half` |
| `sgf.rs` | Escaping, soft line breaks, `SZ[w:h]`, multi-game collections, branches; a corrupt or future `MRAI[…]` blob is ignored rather than fatal | **`lizzieyzy_file_parses_replays_and_preserves_unknown_properties`** — a real 25 765-byte file whose `DZ`/`LZOP`/`LZ` properties survive parse → write → parse unchanged; the only test defending data we did not author |
| **mirai-proto** | | |
| `types.rs` | INV-6 quantisation and INV-2 perspective; `Want` stays one byte; side to move is derived, never stored | **`quantisation_round_trips_within_tolerance`** (fails on any `*_SCALE` edit), **`perspective_conversion_flips_for_white`** |
| `frame.rs` | EOF is not an error; the DoS length cap is enforced *before* allocating; `FrameBuf` is reused | `oversized_length_is_rejected_without_reading_the_body` |
| `sha256.rs`, `transport.rs` | The hand-rolled digest behind cert fingerprints and token comparison; a regenerated cert would silently break every TOFU pin, so reuse is a contract | `known_vectors`, `generated_cert_is_reused_and_fingerprint_is_stable` |
| `tests/wire_size.rs` | The MRP/1 size claim, measured not asserted, plus the error budget | **`a_full_live_report_frames_under_4_kb`** (50 candidates, PV 15, 361 ownership → **2622 bytes**), **`dequantisation_error_stays_inside_the_documented_tolerances`**, `an_open_request_is_tiny` (200 moves → **500 bytes**) |
| **mirai-engine** | | |
| `query.rs` | The exact JSON handed to KataGo, key for key; empty `avoidMoves` specs dropped because one makes KataGo reject the whole query; INV-4 (never `analyzeTurns`); the `terminate` action behind INV-3 | **`empty_board_query_has_only_the_required_keys`**, **`full_query_matches_katago_field_names`**, `never_emits_analyze_turns`, `action_and_terminate_queries` |
| `decode.rs` | INV-1 at the decode boundary; candidates in KataGo's `order` with Black-perspective values; policy has a pass slot and `POLICY_ILLEGAL` markers; malformed output errors instead of panicking | **`ownership_keeps_katago_row_major_top_left_order`** (the unit-test counterpart of recipe (b)), **`decodes_a_report_in_order_with_black_perspective_values`** |
| `local.rs` | INV-2 at the source; unset tuning is omitted rather than overriding the user's `analysis.cfg`; a comma in a path is a startup error because KataGo splits `-override-config` on commas | **`overrides_always_pin_the_reporting_perspective_and_logging`**, `a_comma_in_a_path_is_reported_rather_than_silently_truncated` |
| `remote.rs` | Pin storage, reconnect backoff cap, engine-name resolution, and that a dead address fails instead of hanging | `tofu_store_round_trips_pins`, `connect_to_a_dead_port_never_succeeds` |
| **mirai-server** | | |
| `main.rs` | Token shape and entropy; the cert carries the names a client will dial; the CLI does not drift from the docs | `a_generated_token_is_64_lowercase_hex_characters`, `the_cli_matches_the_documented_flags` |
| `config.rs` | A misspelled key is an error, never silently ignored; relative paths resolve against the config directory so a config plus its cert moves as a unit; `server.example.toml` stays parseable | **`typos_and_duplicates_are_rejected_rather_than_silently_ignored`**, `relative_paths_resolve_against_the_config_directory`, `the_documented_minimal_example_parses` |
| `session.rs` | Exact-match auth, and no `[[token]]` means reject everyone rather than admit everyone; the zero-copy send path is byte-identical to what a client decodes; a client cannot escalate its own priority | **`authentication_accepts_only_an_exact_token`**, **`sub_msg_ref_is_byte_identical_to_sub_msg`**, `priority_is_clamped_into_the_served_band` |
| **mirai (GUI)** — display-free logic only; everything visual is section 5 | | |
| `config.rs` | A missing file is a first run, a corrupt one is an error; a TOFU pin can never land on a local profile | `missing_file_yields_a_seeded_config_not_an_error`, `pins_are_recorded_on_remote_profiles_only` |
| `window.rs` | `tree_has_content`: never autosave an empty board and never offer to restore one, with the boundaries that matter (a pass counts, marks alone do not, content deep in a variation is found); INV-8 | **`a_blank_record_is_not_worth_autosaving`** + five boundary cases, **`handlers_do_not_keep_the_window_alive`** |
| `play.rs` | Deterministic at temperature 0 and genuinely spread above it; one bad report never resigns and a good one clears the streak; clock transitions | **`temperature_zero_always_plays_the_engines_choice`**, **`a_lone_bad_report_does_not_resign`**, `the_last_period_expiring_loses_on_time` |
| `batch.rs` | INV-2 applied to blunder detection — the drop is measured from the mover's side; batch concurrency follows `numAnalysisThreads` and saturates | **`white_blunder_is_measured_from_whites_perspective`**, `in_flight_scales_with_threads_and_saturates_at_sixteen` |
| `widgets/board.rs`, `widgets/tree.rs`, `widgets/winrate.rs` | Click→`Point` mapping, board geometry, tree lane assignment and graph axis inversion — pure functions deliberately lifted out of `snapshot()` so they are testable at all | `hit_test_snaps_to_the_nearest_intersection`, `lanes_keep_the_main_line_on_zero`, **`deep_lines_do_not_recurse`** (a long game must not blow the stack), `a_white_blunder_is_not_a_black_blunder` |
| `util.rs`, `harness.rs` | Formatting helpers; the harness grammar, including that a malformed `wait:soon` is dropped rather than silently becoming zero | `script_parsing_covers_every_step_kind`, `unknown_and_empty_steps_are_dropped_not_fatal` |

## 3. Testing philosophy

A test defends an **observable contract** and must **fail on a plausible bug**. Anything else
is cost without payoff.

| Write a test when the change… | Example |
|---|---|
| Introduces a rule with an edge case | superko vs simple ko; suicide per ruleset |
| Moves a number across a boundary | any `*_SCALE` in `mirai-proto` `types.rs` |
| Touches a wire or file format | KataGo query keys, framing, SGF unknown-property preservation |
| Touches a perspective conversion | every INV-2 site: `winrate_for`, `score_lead_for`, blunder detection |
| Fixes a bug | add the case that was wrong, plus its boundaries |
| Adds a pure function extractable from a widget | board hit-testing, tree lane assignment |

| Do **not** write a test when the change… |
|---|
| Only moves code, renames a private item, or edits a comment |
| Is layout, colour or spacing — screenshot it instead; a pixel assertion is wrong at the next margin tweak |
| Would assert on source text, a field list, or that a builder was called — passes on real bugs, fails on refactors |
| Needs a display. `cargo test` stays headless; drive it through the harness |

Two classes of defect unit tests here **structurally cannot** catch, so do not pretend:

1. **Rendered output.** A transposed overlay is obvious in a PNG and invisible to any
   affordable assertion about `snapshot()`.
2. **Real concurrent timing.** The engine-activation race needed two engines with different
   startup latencies selected in quick succession. Its guard is the activation counter in
   `AppState::activate_profile`; its proof was a server log beside a screenshot.

## 4. Verifying against a real engine

`cargo test` never starts KataGo. `crates/mirai-engine/examples/probe.rs` does: it drives a
`LocalEngine` or a `RemoteEngine` behind the same `Engine` trait object, so the two paths can
be diffed line for line. `probe --help` is the authoritative flag list; the ones that matter
are `--katago/--model/--config` (local), `--remote/--token/--engine/--fingerprint` (remote),
and `--visits/--size/--moves/--komi/--rules/--report-every-ms` (the position).

```sh
export KATA=/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/linux-x64/katago
export MODEL=/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/weights/default.bin.gz
export CFG=/home/ykpcx/2026-04-26-linux64.with-katago/Lizzieyzy/engines/katago/configs/analysis.cfg

# local
cargo run -p mirai-engine --example probe -- \
    --katago "$KATA" --model "$MODEL" --config "$CFG" \
    --moves D4,Q16 --visits 500 --komi 7.5 --rules chinese

# remote, against a running mirai-server
cargo run -p mirai-engine --example probe -- \
    --remote mirai://127.0.0.1:9678 --token "$TOKEN" --engine default \
    --fingerprint "$(cargo run -q -p mirai-server -- --print-fingerprint)" \
    --moves D4,Q16 --visits 500
```

Correct output: one `engine:` line, a run of `[report]` blocks, exactly one `[final]`.

```text
engine: local katago 1.16.4 model <name> threads 2 human_model false
[report] visits=57 ownership=361
  D4 35.61 -1.23 14
```

Columns are GTP point, win rate **as a percentage for the side to move**, score lead for the
side to move, visits; the list is in KataGo's `order`, so line one is the engine's choice.
`ownership=361` proves the ownership array came back at one entry per point. Exit code is 0
on `Done`, 1 on `Failed`, on error, and on Ctrl-C.

Run both modes with identical arguments and diff the `[final]` block: candidate order and
visits must match, values must agree to within the quantisation tolerance from section 2.
Anything larger is a defect in the remote path, not noise.

### Telling a defect from a model difference

The empty-board win rate with the bundled net is **~35.6 %**, not the 45–55 % one might
assume. That number is correct. The procedure for settling any such question:

| Step | Command | Expected |
|---|---|---|
| 1. Get ground truth outside mirai | pipe the identical query into `katago analysis` (below) | `rootInfo.winrate = 0.353740327` — mirai reproduces it exactly |
| 2. Confirm the perspective convention | komi sweep, `--komi 0 / 7.5 / 30` | 94.3 % → 35.4 % → 0.1 %, monotonically decreasing. Only possible if the value is Black's (INV-2) |
| 3. Confirm index order | probe an asymmetric position and read the ownership ends | `ownership[0]` = A19 top-left = Black, `ownership[360]` = T1 bottom-right = White (INV-1) |

```sh
printf '%s\n' '{"id":"gt","boardXSize":19,"boardYSize":19,"rules":"chinese","komi":7.5,"moves":[],"maxVisits":200,"reportAnalysisWinratesAs":"BLACK"}' \
  | "$KATA" analysis -model "$MODEL" -config "$CFG" | head -1
```

| Observation | Verdict |
|---|---|
| Differs from an expectation, but raw KataGo agrees with mirai | Model difference. Fix the expectation, never tune code toward a number |
| Differs from raw KataGo on the identical query | Real defect — start at `decode.rs`, then `query.rs` |
| Differs between local and remote for the same query | Real defect in the quantiser or wire format — start at `mirai-proto` `types.rs` |
| Moves run to run at the same visit cap | Expected; KataGo search is not deterministic across thread schedules. Compare order, sign and magnitude, not digits |

## 5. Testing the GUI

Everything below is derived from `crates/mirai/src/harness.rs`, `window.rs` and `main.rs`.
**The recipes were not executed while writing this document**; treat the `wait:` values as
starting points and read the harness's stderr trace to see what actually happened.

### Why external capture does not work

The session is Wayland with XWayland for X11 clients.

| Attempt | Result |
|---|---|
| ImageMagick `import` | Fails with a misleading `missing an image filename` — that build has no X11 delegate |
| `ffmpeg -f x11grab -i :0` | Succeeds and produces a pure-black PNG: under Wayland the compositor owns window contents, and XWayland's root window has nothing drawn into it |
| `grim`, `xwd`, `spectacle`, `flameshot`, `maim` | Not installed |

`GDK_BACKEND=x11` makes an external grab work and is **not acceptable**: mirai ships on
Wayland, and a test that only passes on another backend is not testing the shipped
configuration. There is no `GDK_BACKEND` anywhere in the repo; adding one is a regression in
the test, not a fix.

### How the harness solves it

The renderer that draws the window lives in the process, so `harness::shot` asks it directly:

```rust
let paintable = gtk::WidgetPaintable::new(Some(&window));
let snapshot = gtk::Snapshot::new();
paintable.snapshot(&snapshot, w as f64, h as f64);
let node = snapshot.to_node().ok_or("nothing was drawn")?;

let renderer = window.native().and_then(|n| n.renderer()).ok_or("the window has no renderer")?;
let texture = renderer.render_texture(&node, None);
texture.save_to_png(path)
```

Compositor-independent, no external tooling, native Wayland backend, and it captures exactly
the pixels GSK produces — including the custom `snapshot()` of `BoardView`, `WinrateGraph`
and `MoveTreeView` (INV-9), which are the parts most likely to be wrong.

### Real code paths, not a test-only path

`harness::activate` calls `WidgetExt::activate_action` on the active window, which resolves
the prefix through the widget's action muxer. `action:win.toggle-analysis` therefore reaches
**exactly the handler <kbd>space</kbd> reaches**: same `GAction`, installed once in
`window::install_actions`. There is no parallel test code path and no scaffolding inside
feature code. Names beginning `app.` are routed to the `adw::Application` instead.

Dialogs are not action-driven, so `harness::press` walks the widget tree from the window root
and clicks the first **visible** `gtk::Button` whose label contains the needle; a presented
`adw::Dialog` is a descendant of the window, so this reaches dialog buttons. Three traps:

- Mnemonic underscores are stripped before matching (`press:Save` matches `_Save`).
- Substring match, depth-first from the window root: the first hit wins. `press:New game`
  finds the *main header bar's* button, not a dialog. Pick a needle unique to the dialog —
  `press:Start` in `dialogs::new_game` is unambiguous.
- `adw::AlertDialog` responses (`dialogs::show_score_with`, `dialogs::confirm_fingerprint`)
  are declared as response ids, not as buttons we construct. [INFERENCE] `press:Close` works
  only if libadwaita realises them as labelled buttons; unverified. End such recipes with
  `shot` then `quit` instead of dismissing the dialog.

### Step grammar

`MIRAI_HARNESS` holds one comma-separated script, parsed by `harness::parse`.

| Step | Meaning | Delay after |
|---|---|---|
| `wait:<ms>` | Sleep. Non-numeric is **dropped**, not treated as zero | — |
| `action:<prefix.name>` | Activate an action with no parameter | 120 ms |
| `action:<prefix.name>=<string>` | Activate with a string parameter (`action:win.set-engine=workstation`) | 120 ms |
| `press:<label substring>` | Click the first visible matching button | 250 ms |
| `shot:<path.png>` | Render the active window to PNG | see below |
| `quit` | `app.quit()`, ending the script | — |

Unknown kinds are logged and skipped; whitespace around steps is trimmed, so a script may be
wrapped across lines. `mod harness` is `#[cfg(debug_assertions)]` and `install` returns
immediately when `MIRAI_HARNESS` is unset — **a release build ignores the variable entirely,
so always use a debug build.**

Every step logs to stderr; this trace is your evidence:

```text
harness: 6 steps
harness: action win.next10 -> ok            # MISSING = no such action on the window
harness: press "Start" -> ok                # NOT FOUND = no visible button matched
harness: wrote /tmp/mirai-a.png             # or: harness: screenshot failed: <reason>
harness: quitting
```

**The retry loop.** `WidgetPaintable::snapshot` yields no render node if the window has not
drawn since the last change, so `shot` `queue_draw()`s and retries twelve times at 120 ms
before giving up. A `shot:` step therefore costs 120 ms at best and ~1.4 s at worst.
`screenshot failed: nothing was drawn` after all twelve means the window genuinely never
mapped — usually a too-short preceding `wait:`, or a modal that grabbed before `present()`.

### Recipes

Preconditions: a debug build, and an engine profile already in
`~/.config/mirai/config.toml` — with none, `window::present` opens Preferences at startup and
every action below lands on the wrong window. `Config::seeded` writes a working local profile
on first run *if* the bundled KataGo and network exist at the paths its `SEED_*` constants
name; otherwise configure one by hand once. A local KataGo takes ~6 s to load its net, which
is why each first `wait:` is generous; toggling analysis before it is ready is safe, because
`AppState::set_engine` calls `restart_analysis` when the engine lands.

```sh
export SGF=/home/ykpcx/2026-04-26-linux64.with-katago/save/autoGame1.sgf   # the 30-node fixture
export RUST_LOG=info,mirai=debug
```

**(a) Load an SGF, navigate, live analysis, screenshot.** `mirai` sets
`ApplicationFlags::HANDLES_OPEN`, so a positional path is opened through `connect_open`.

```sh
MIRAI_HARNESS="wait:2000,action:win.next10,action:win.next10,action:win.toggle-analysis,wait:12000,shot:/tmp/mirai-a.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Expect `harness: 7 steps`, three `-> ok` lines, `wrote /tmp/mirai-a.png`. The PNG shows the
board at move 20, the move tree with the cursor 20 nodes along the main line, a populated
win-rate graph, and blue candidate overlays with win-rate and visit labels.

**(b) Ownership overlay — the INV-1 canary.** Live analysis always requests
`Want::OWNERSHIP`, so `win.toggle-ownership` only switches the drawing on.

```sh
MIRAI_HARNESS="wait:2000,action:win.last,action:win.toggle-analysis,wait:15000,action:win.toggle-ownership,wait:1500,shot:/tmp/mirai-own.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Correct: dark shading sits squarely over Black's stones and the territory they surround, light
over White's. On this fixture the dark region is the bottom-left, over Black's group there.

```text
correct (index = y*w + x)        transposed (index = x*h + y)
+-------------------+            +-------------------+
|                   |            |     ##            |
|                   |            |     ##            |
|  ####             |            |     ##            |
|  ######   O       |            |  ..........       |
|  ####             |            |                   |
+-------------------+            +-------------------+
 shading covers the stones        mirrored about the main diagonal:
                                  blobs on empty points, a bottom-left
                                  group shading top-right
```

A vertical flip is subtler: right shape, reflected top-to-bottom, so a corner group shades
the opposite corner of the same file. Either way the giveaway is shading that does not touch
the stones it belongs to.

**(c) New game, engine reply, undo, score estimate.**

```sh
MIRAI_HARNESS="wait:8000,action:win.new-game,wait:800,press:Start,wait:1200,action:win.pass,wait:12000,shot:/tmp/mirai-play1.png,action:win.undo,wait:1200,shot:/tmp/mirai-play2.png,action:win.score,wait:20000,shot:/tmp/mirai-score.png,quit" \
  cargo run -p mirai
```

| Step | Why it works |
|---|---|
| `wait:8000` | `dialogs::new_game` reads `engine_desc()` to decide whether the human-like strength mode is offered, so the engine should be up first |
| `press:Start` | Dialog defaults: 19×19, Chinese, komi 7.5, no handicap, **you play Black**, no time control, 800 visits per engine move |
| `action:win.pass` | With a session active this routes to `PlayController::pass`, accepted only on `PlayState::HumanTurn`. Black passes, so it becomes the engine's turn — this is how the harness triggers an engine move without clicking the board |
| `shot:…play1.png` | One White stone, move list `pass` then White's reply |
| `action:win.undo` | Routes to `PlayController::undo`, which removes up to two nodes so the human is on move again |
| `action:win.score` | `window::do_score` subscribes at high priority with `Want::OWNERSHIP` and shows the `Result` dialog with `size × size · rules · komi` |

`press:Start` logging `NOT FOUND` means the dialog was not presented yet — raise the
preceding `wait:`. A `No engine to estimate the score with` toast means the engine never came
up; check the KataGo log directory (section 7).

**(d) Whole-game analysis.** `win.analyse-game` is `BatchAnalysis::start`; each node is
analysed to `analysis.batch_visits` (1000 by default) with concurrency scaled to the engine's
`numAnalysisThreads`.

```sh
MIRAI_HARNESS="wait:2000,action:win.analyse-game,wait:90000,shot:/tmp/mirai-batch.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Expect the win-rate graph filled end to end rather than a single point, blunder markers on
the worst moves, and the Analysis sidebar listing blunder rows. If the progress indicator is
still running in the PNG, raise the wait.

**(e) Clean shutdown, proving teardown ran.** `quit` calls `app.quit()`; the teardown is
connected to **`close-request`**, so drive GTK's built-in `window.close` action instead.

```sh
rm -f ~/.local/share/mirai/clean-exit ~/.local/share/mirai/autosave.sgf

MIRAI_HARNESS="wait:2000,action:win.next10,action:win.toggle-analysis,wait:15000,shot:/tmp/mirai-before-close.png,action:window.close" \
  cargo run -p mirai -- "$SGF"
echo "exit=$?"

ls -l ~/.local/share/mirai/autosave.sgf ~/.local/share/mirai/clean-exit
pgrep -a katago      # must show nothing left from this run
```

`window::connect_close` runs, in order: flush the comment, write the autosave, write
`clean-exit`, cancel the batch, stop play, abort the score task, switch live analysis off so
the pump releases its `Subscription` (INV-3), save the config, drop the engine (terminating
KataGo), and drop the last strong `Rc<Ui>` (INV-8). `exit=0`, both files present, no orphaned
`katago`.

Related: `window::write_autosave` deletes the file rather than writing a contentless record,
and the restore offer is armed only for an autosave with a move, setup stone or comment — so
the same recipe on a blank instance must leave no `autosave.sgf`. Conversely, killing a run
leaves `clean-exit` missing and arms the restore prompt on the next start; that asymmetry is
the feature. [INFERENCE] a script ending in `quit` behaves like a kill here, since
`app.quit()` destroys windows rather than emitting `close-request`.

### Drivable action names

All installed in `window::install_actions`, all reachable as `action:<name>`; the keyboard
accelerators bound to them are listed in the [user guide](../user/GUIDE.md).

```text
win.first win.last win.prev win.next win.prev10 win.next10 win.branch-prev win.branch-next
win.toggle-analysis win.toggle-ownership win.toggle-policy win.toggle-coords win.toggle-move-numbers
win.pass win.undo win.delete-branch win.resign win.new-game win.score win.analyse-game
win.open win.save win.save-as win.copy-sgf win.paste-sgf
win.preferences win.shortcuts win.about
win.set-engine=<profile name>        (stateful, takes a string argument)
window.close                         (GTK built-in; the only way to trigger teardown)
```

`win.open`, `win.save-as` and `win.preferences` open dialogs the harness cannot fill in —
pass the SGF on the command line instead of driving `win.open`.

## 6. Verifying the remote path

**Bring up a server.**

```sh
cargo run -q -p mirai-server -- --generate-token        # 64 hex chars, needs no config file
mkdir -p ~/.config/mirai && cp crates/mirai-server/server.example.toml ~/.config/mirai/server.toml
$EDITOR ~/.config/mirai/server.toml                     # paste the token, set the engine paths
cargo run -q -p mirai-server -- --print-fingerprint     # creates the cert if missing
RUST_LOG=info,mirai_server=debug cargo run --release -p mirai-server
```

`server.example.toml` is the annotated reference and `the_documented_minimal_example_parses`
keeps it honest. `--config` and `--listen` override the file. With no `[[token]]` block the
server starts and warns that every client will be rejected. Boot log:

```text
INFO engine ready engine=default katago_version=1.16.4 analysis_threads=2 human_model=false
INFO mirai-server listening listen=127.0.0.1:9678 engines=1 tokens=1
INFO certificate fingerprint (pin this in the client) sha256=<64 hex>
```

**Point a client at it.**

`probe --remote` is the fastest check. For the GUI, add a `kind = "remote"` profile
(`url`, `token`, optional `engine`) to `~/.config/mirai/config.toml`; the exact shape is
pinned by `parses_the_documented_config_shape`. Leave `cert_sha256` out initially: on first
connect the app shows `dialogs::confirm_fingerprint` with the colon-grouped digits, and
accepting writes the pin via `Config::set_pin`. Compare against `--print-fingerprint` before
accepting. Switch engines at runtime with `action:win.set-engine=<profile>`.

**Confirm a subscription opened.**

```text
INFO connection open session=1 peer=127.0.0.1:53412
INFO authenticated session=1 token=laptop max_subs=4
INFO open subscription session=1 sub=1 engine=default moves=20 max_visits=Some(1000000) priority=4
INFO subscription done session=1 sub=1 visits=6500
```

`moves=20` is INV-4 on display: the whole position travelled with the request, there is no
server-side session state. Priority identifies the caller — live analysis 4, whole-game
analysis 0, score estimate 8 — each clamped into the served band.

**Prove cancellation.**

```sh
# terminal 1
RUST_LOG=info cargo run --release -p mirai-server
# terminal 2, then Ctrl-C
cargo run -p mirai-engine --example probe -- --remote mirai://127.0.0.1:9678 --token "$TOKEN" --visits 1000000
```

Compare timestamps; the recorded run showed the server logging `connection closed` and
`subscription dropped` **within 1 ms** of the client's SIGINT. That only works because
`probe` has a `ctrl_c` arm that drops the subscription *and* the engine and gives quinn
200 ms to flush `CONNECTION_CLOSE`. A `kill -9` sends nothing, and UDP has no FIN, so the
server can only notice via the ~30 s QUIC idle timeout — if you measure 30 s, check how you
killed the client before filing a bug.

Then prove KataGo actually stopped rather than being abandoned, by sampling
`/proc/<pid>/stat` utime+stime as a **rate**:

```sh
pid=$(pgrep -n katago)
read _ _ _ _ _ _ _ _ _ _ _ _ _ u1 s1 _ < /proc/$pid/stat; sleep 4
read _ _ _ _ _ _ _ _ _ _ _ _ _ u2 s2 _ < /proc/$pid/stat
echo "$(( (u2+s2-u1-s1) * 100 / (4 * $(getconf CLK_TCK)) ))% CPU over 4s"   # expect 0.0%
```

> `ps` is useless here: its `%CPU` is a **lifetime average** and read 11.4 % for a completely
> idle process. To prove something stopped, measure a rate, not a total.

The same sampling proves the local path, where the subscription guard enqueues a KataGo
`terminate` action.

## 7. Debugging playbook

The short trap table is in [AGENTS.md](../../AGENTS.md); this is the detailed version. Every
row is a defect that happened or a guard that exists because one did.

| Symptom | Cause | Where to look |
|---|---|---|
| A `notify::` handler or `bind_property` target silently stopped firing; a header or toggle shows stale state | `explicit_notify` on a **derive-generated** setter. It is a pspec flag that turns OFF automatic `notify::` emission, so it belongs only on a property whose hand-written setter emits the signal itself | `app.rs`, the `#[properties]` block on `imp::AppState`. Only the three properties naming a custom setter may carry it |
| A widget will not shrink, or one pane eats the window | `gtk::Paned` resize flags with a hardcoded position | `window.rs`: the graph gets `set_size_request` plus `resize_end_child(false)`, so all growth goes to the board while the split stays draggable |
| The sidebar page switcher is missing and a stray `✕` sits in its place | `adw::HeaderBar::show_title(false)` hides the *title widget*, and the title widget **is** the `ViewSwitcher`; the `✕` is a second set of window controls | `window.rs` header construction — keep `show_title` on, disable the duplicate title buttons |
| An engine connects then vanishes seconds later; the server logs a connection opening and closing with no subscription | Activation race: `activate_profile` is async and a local KataGo takes ~6 s, so an older activation can finish last and install itself over a newer one | The activation counter in `AppState::activate_profile`. `discarding a superseded engine activation` at debug level means the guard worked |
| Live analysis restarts but reports keep arriving for the old position | The other counter: `generation`, bumped by `restart_analysis` and checked by the pump before applying a report | `app.rs` `restart_analysis` |
| "mirai did not shut down cleanly" every start | `clean-exit` is only written by the `close-request` teardown; something is killing the app | `window::connect_close`, `window::autosave_paths` |
| The restore prompt offers an empty board | `tree_has_content` regressed | `window.rs` and its six boundary tests |
| Overlay shading in the wrong place | Someone remapped indices; INV-1 says ownership and policy index identically to the board | `decode.rs` `ownership_keeps_katago_row_major_top_left_order`, then recipe (b) |
| Win rates inverted for one side | INV-2 violated: a conversion applied somewhere other than display | `MoveInfo::winrate_for` / `score_lead_for` are the only sanctioned sites |
| Territory totals low by the prisoner count while the margin is right | The "territory minus prisoners you lost" formula; each side gets territory **plus the prisoners it holds** | `score.rs`, `endgame_area_and_territory` |
| Engine will not start, no useful message in the GUI | KataGo's own log | `$XDG_DATA_HOME/mirai/katago-logs` (GUI, `Config::data_dir`), `$TMPDIR/mirai-katago-logs` (`LocalEngineConfig` default), or `log_dir` in `server.toml` |
| `Startup(...)` error mentioning `logDir` or a config key | A comma in a path; KataGo splits `-override-config` on commas | `local.rs` `override_config` |
| Client cannot connect though the server is up | Fingerprint mismatch after a regenerated cert, wrong token, or wrong `[[engine]]` name | `--print-fingerprint` vs `cert_sha256`; the server logs the token name that authenticated |
| Engine and runtime survive window close | INV-8: a long-lived handler captured a strong `Rc<Ui>` instead of going through `with_ui` | `window::connect_close`, `handlers_do_not_keep_the_window_alive` |
| `harness: action … -> MISSING` | No such action on the active window, or a typo. `app.*` goes to the application, everything else to the window | `harness::activate`, `window::install_actions` |
| `harness: screenshot failed: nothing was drawn` | The window never mapped, or a modal grabbed before `present()` | Lengthen the preceding `wait:` |
| The harness does nothing at all | Release build (`#[cfg(debug_assertions)]`), `MIRAI_HARNESS` unset, or every step malformed | `harness::install` |
| The app opens Preferences instead of the game window | No engine profile configured | `window::present` |
| `cargo test` fails only in `sgf.rs` with `fixture` | The LizzieYzy fixture is not on this machine | section 9 |

`RUST_LOG` (both binaries use `EnvFilter`, defaulting to `info`):

| Value | Use |
|---|---|
| `info,mirai=debug` | GUI internals: engine activation, the superseded-activation guard, autosave warnings |
| `info,mirai_engine::local=trace` | Every line exchanged with the KataGo process |
| `info,mirai_engine::remote=debug` | Reconnect backoff, TOFU pinning, stream lifecycle |
| `info,mirai_server=debug` | Adds `cancel for an unknown subscription`, `subscription stream closed early` |
| `debug` | Everything, including quinn and rustls. Loud — scope it when you can |

## 8. Before declaring work done

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --workspace          # different codegen; the harness compiles out
cargo doc --workspace --no-deps            # catches broken intra-doc links
```

| Change touches | Also required |
|---|---|
| `mirai-core` rules, scoring, SGF | Nothing more; the suite is the proof |
| `mirai-proto` types, scales, framing | `cargo test -p mirai-proto --test wire_size -- --nocapture`, and read the three printed numbers |
| `mirai-engine` query or decode | `probe` locally, and diff a `[final]` block against raw `katago analysis` on the identical query |
| `remote.rs` or `mirai-server` | `probe` in **both** modes on the same position, the subscription log check, and the cancellation measurement |
| Anything drawn | At least one harness recipe, and actually look at the PNG. Recipe (b) for anything touching `Point`, ownership or policy |
| Signals, properties, `Rc` capture, teardown | Recipe (e): `exit=0`, both files written, no orphaned `katago` |
| A new `GAction` or accelerator | Drive it once through `action:` and confirm `-> ok`, not `MISSING` |

- [ ] Every new file carries the `SPDX-License-Identifier: GPL-3.0-or-later` header.
- [ ] No `GDK_BACKEND` anywhere in the tree.
- [ ] No test-only branch inside feature code.
- [ ] `cargo fmt` was not run across the tree.
- [ ] Invariants you touched still read true in [ARCHITECTURE.md](ARCHITECTURE.md) and
      [PROTOCOL.md](PROTOCOL.md).

## 9. Known gaps

Reported, not fixed.

| Gap | Detail |
|---|---|
| **The SGF fixture is not in the repo** | `sgf.rs`'s `REAL_SGF` const is an absolute path under `/home/ykpcx/…`; the LizzieYzy test panics with `fixture` on any other machine, so the suite is not reproducible off this box. Vendoring the 25 765-byte file under `crates/mirai-core/tests/data/` and using `include_bytes!` would fix it |
| **First-run seed paths are machine-specific** | `config.rs`'s `SEED_*` constants point at the same tree. Harmless — the code checks `exists()` — but a fresh checkout elsewhere always lands in Preferences |
| **No end-to-end client↔server test** | `remote.rs` covers the pin store, backoff, engine selection and two failure paths. Nothing spawns a server on loopback and runs a real `Open`/`Report`/`Cancel` exchange, so the handshake and INV-3's remote half are hand-verified only. This is the largest gap; a `#[tokio::test]` with an in-process server and a stub `Engine` would close it without needing KataGo |
| **No test for a changed fingerprint** | Pins round-trip, but nothing asserts that a *different* fingerprint is refused |
| **`app.rs` has no test module** | `AppState` is the single source of truth (INV-7) and its property/signal wiring — precisely what the `explicit_notify` defect broke — is uncovered. A display-free test could assert that every generated setter still emits `notify::` |
| **`panels/analysis.rs`, `prefs.rs`, `dialogs.rs` have no tests** | All display-bound. `panels/analysis.rs` has the most extractable pure logic (candidate row formatting, blunder rows) |
| **The harness is only parser-tested** | `activate`, `press` and `shot` need a display and are exercised only by the recipes above |
| **No golden-image comparison** | Screenshots are read by a human; nothing detects a slow visual regression between runs |
