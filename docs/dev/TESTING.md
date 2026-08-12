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

`cargo fmt --all --check` and the clippy line above are both quiet: the tree is formatted with
stock `rustfmt` defaults and lint-free under the pinned nightly. Keep it that way. Between full
runs, `cargo fmt` and `cargo clippy -p <crate>` on what you touched is enough; a toolchain
bump that lights up untouched code is its own commit, not part of a feature.

| Never run casually | Why |
|---|---|
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
| `tuning.rs` | The generated analysis config: KataGo refuses to start unless `numAnalysisThreads`, `numSearchThreadsPerAnalysisThread` and `nnMaxBatchSize` are all present, so the rendered file must carry every one of them; the batch must cover the thread product; rewriting an unchanged file would churn a config two windows share | **`the_rendered_config_carries_every_key_katago_demands`**, **`writing_is_idempotent_so_shared_engines_do_not_churn`**, `the_defaults_are_internally_consistent`, `a_batch_smaller_than_the_thread_product_is_reported` |
| `calibrate.rs` | Automatic tuning uses fixed visits and ownership on distinct positions, stops at the first search-thread doubling below 10%, picks aggregate analysis throughput, preserves the cache decision and grows the batch to cover the winner | **`benchmark_requests_are_fixed_visit_owned_and_position_distinct`**, `search_stops_before_low_gain`, `analysis_uses_maximum_throughput`, `final_tuning_preserves_cache_and_covers_threads` |
| **mirai-server** | | |
| `main.rs` | Token shape and entropy; the cert carries the names a client will dial; the CLI does not drift from the docs | `a_generated_token_is_64_lowercase_hex_characters`, `the_cli_matches_the_documented_flags` |
| `config.rs` | A misspelled key is an error, never silently ignored; relative paths resolve against the config directory so a config plus its cert moves as a unit; an engine with no `config` file gets a generated one; `server.example.toml` stays parseable | **`typos_and_duplicates_are_rejected_rather_than_silently_ignored`**, `relative_paths_resolve_against_the_config_directory`, `an_engine_without_a_config_file_gets_a_generated_one`, `the_documented_minimal_example_parses` |
| `session.rs` | Exact-match auth, and no `[[token]]` means reject everyone rather than admit everyone; the zero-copy send path is byte-identical to what a client decodes; a client cannot escalate its own priority | **`authentication_accepts_only_an_exact_token`**, **`sub_msg_ref_is_byte_identical_to_sub_msg`**, `priority_is_clamped_into_the_served_band` |
| **mirai (GUI)** — display-free logic only; everything visual is section 5 | | |
| `config.rs` | A missing file is a first run, a corrupt one is an error; model/config directories have a fixed XDG order, every valid file is merged without duplicates, and GTP config is excluded; a TOFU pin can never land on a local profile; concurrent-window saves merge correctly | `missing_file_yields_a_seeded_config_not_an_error`, `xdg_system_directories_keep_precedence_and_ignore_relative_entries`, `discovery_directories_follow_the_documented_order`, `network_discovery_merges_every_bin_gz_and_ignores_everything_else`, `analysis_config_discovery_returns_every_analysis_config_but_not_gtp`, `pins_are_recorded_on_remote_profiles_only`, **`a_save_keeps_another_windows_edit`**, `a_removed_profile_is_removed_from_the_file` |
| `window.rs` | `tree_has_content`: never autosave an empty board and never offer to restore one, with the boundaries that matter (a pass counts, marks alone do not, content deep in a variation is found); INV-8 | **`a_blank_record_is_not_worth_autosaving`** + five boundary cases, **`handlers_do_not_keep_the_window_alive`** |
| `fox.rs` | Fox's literal property separators and quarter-point komi are normalised; both documented handicap encodings become root setup stones without losing variations; query text and result metadata stay safe at the URL and GTK markup boundaries | `fox_escapes_and_quarter_point_komi_are_normalised`, `fox_setup_nodes_become_one_root_handicap`, `consecutive_black_handicap_moves_are_promoted_too`, `handicap_normalisation_preserves_root_variations`, `query_values_are_utf8_percent_encoded` |
| `play.rs` | Deterministic at temperature 0 and genuinely spread above it; one bad report never resigns and a good one clears the streak; clock transitions | **`temperature_zero_always_plays_the_engines_choice`**, **`a_lone_bad_report_does_not_resign`**, `the_last_period_expiring_loses_on_time` |
| `batch.rs` | INV-2 applied to blunder detection — the drop is measured from the mover's side; batch concurrency follows `numAnalysisThreads` and saturates | **`white_blunder_is_measured_from_whites_perspective`**, `in_flight_scales_with_threads_and_saturates_at_sixteen` |
| `widgets/board.rs`, `widgets/tree.rs`, `widgets/winrate.rs` | Click→`Point` mapping, board geometry, tree lane assignment and graph axis inversion — pure functions deliberately lifted out of `snapshot()` so they are testable at all | `hit_test_snaps_to_the_nearest_intersection`, `lanes_keep_the_main_line_on_zero`, **`deep_lines_do_not_recurse`** (a long game must not blow the stack), `a_white_blunder_is_not_a_black_blunder` |
| `app.rs` | The search-speed meter: a steady search reads its true rate, a real slowdown is followed, and the final report — echoed once as `Done` with the same visit count — never drags the reading to zero | **`a_repeated_final_report_does_not_zero_the_rate`**, `speed_meter_measures_visits_per_second`, `the_meter_tracks_a_slowdown` |
| `engines.rs` | Engines are shared only between identical profiles, and a dropped engine is never handed out again — the pool holds weak references so the last window closing takes KataGo with it | `only_identical_profiles_share_an_engine`, `a_dropped_engine_is_no_longer_running` |
| `prefs.rs` | A discovered-file chooser names candidates by file name, and only a name two directories share carries that directory | `only_candidates_sharing_a_name_carry_their_directory` |
| `util.rs`, `harness.rs` | Formatting helpers; the harness grammar, including that malformed numeric steps are dropped rather than silently becoming zero | `script_parsing_covers_every_step_kind`, `unknown_and_empty_steps_are_dropped_not_fatal` |

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
**The recipes were not executed while writing this document** — except (g), whose command and
output below are transcribed from a real run; treat the other `wait:` values as starting
points and read the harness's stderr trace to see what actually happened.

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
and clicks the first **visible** `gtk::Button` matching the needle. Three things are tried per
button, in order: `GtkButton:label`; the first `gtk::Label` in its subtree, which is how an
`adw::ButtonContent` child is matched; then `tooltip_text`, which is how an icon-only button
with no label at all is reached — `press:Edit this profile` in Preferences works on the tooltip.
A `gtk::MenuButton` matching by label or tooltip is popped up instead of clicked, which is how
the discovered-file choosers in the local-engine editor are opened; its entries are then
ordinary buttons labelled with the file name, so `press:kata1-b18…` picks one.
A presented `adw::Dialog` is a descendant of the window, so this reaches dialog buttons too.
`harness::fill` similarly finds a visible `gtk::SearchEntry` by placeholder substring and sets
its text, so network-backed search dialogs can be exercised without a test-only application
path.
Four traps:

- Mnemonic underscores are stripped before matching (`press:Save` matches `_Save`).
- Substring match, depth-first from the window root: the first hit wins. `press:New game`
  finds the *main header bar's* button, not a dialog. Pick a needle unique to the dialog —
  `press:Start` in `dialogs::new_game` is unambiguous.
- A popover lives in its own surface, so its contents never appear in a `shot`. Verify a
  chooser by what picking an entry *does* — the row subtitle, and the `katago …` start line
  after **Save profile** — not by a screenshot of the open list.
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
| `select:<row title substring>=<index>` | Set the first visible matching `adw::ComboRow`; index 0 is its prompt/default entry | 250 ms |
| `fill:<entry placeholder substring>=<text>` | Fill the first visible `gtk::SearchEntry` whose placeholder matches | 120 ms |
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
on first run if it finds both a `katago` on `PATH` and a `*.bin.gz` in the ordered model
locations described in the user guide; otherwise configure one by hand. A local KataGo takes
~6 s to load its net,
which is why each first `wait:` is generous; toggling analysis before it is ready is safe,
because `AppState::set_engine` calls `restart_analysis` when the engine lands.

```sh
export SGF=$PWD/crates/mirai-core/tests/data/lizzieyzy-autoGame1.sgf   # the 30-node fixture
export RUST_LOG=info,mirai=debug
```

**Isolation — a harness run must not touch the developer's session.**

A scripted run writes to three things the developer is also using, and each has bitten us:

| Shared thing | What happens without isolation |
|---|---|
| The bus name | mirai is a unique `GApplication`. A second launch hands its SGF to the running instance — which opens it in a new window — and exits 0, so the script runs nowhere and someone else's session gets the file |
| `~/.config/mirai/config.toml` | `connect_close` calls `save_config`, so a run that toggled a display option persists it into the developer's settings. The merge in `save_merged` keeps *other windows'* keys, not other people's |
| `$XDG_DATA_HOME/mirai` | the autosaves and `katago-logs` live here; a harness run that crashes leaves the developer a restore prompt for a record they never opened |

The first is handled in-tree: with `MIRAI_HARNESS` set, `harness::application_flags` adds
`NON_UNIQUE` (debug builds only), so a harnessed run is always its own primary instance.
The other two are yours to redirect — `directories::ProjectDirs` honours the XDG variables:

```sh
scratch=$(mktemp -d); mkdir -p "$scratch/config/mirai" "$scratch/data"
cp ~/.config/mirai/config.toml "$scratch/config/mirai/"        # keep the engine profile
XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
  MIRAI_HARNESS="…" ./target/debug/mirai "$SGF"
rm -rf "$scratch"
```

**Never reach for `dbus-run-session` to get a second instance.** It works, and it costs the
whole login session its accessibility bus: the GTK client activates `org.a11y.Bus` on the
private bus, `at-spi-bus-launcher` unconditionally rewrites `$XDG_RUNTIME_DIR/at-spi/bus_0`,
and when the private session exits the socket file outlives its listener. Every GTK
application started afterwards logs `Unable to connect to the accessibility bus … Connection
refused` until someone runs `systemctl --user restart at-spi-dbus-bus`. `GTK_A11Y=none` does
not prevent it (measured: the socket's inode still changes). `NON_UNIQUE` removes the reason
to want a private bus at all.

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
rm -f "$XDG_DATA_HOME/mirai"/autosave-*.sgf     # scratch data dir; see Isolation above

MIRAI_HARNESS="wait:2000,action:win.next10,action:win.toggle-analysis,wait:15000,shot:/tmp/mirai-before-close.png,action:window.close" \
  ./target/debug/mirai "$SGF"
echo "exit=$?"

ls "$XDG_DATA_HOME/mirai"       # no autosave-*.sgf: the window deleted its own
pgrep -a katago                 # must show nothing left from this run
```

`window::connect_close` runs, in order: flush the comment, delete this window's autosave,
cancel the batch, stop play, abort the score task, switch live analysis off so the pump
releases its `Subscription` (INV-3), save the config, drop the engine (terminating KataGo once
no other window holds it), and drop the last strong `Rc<Ui>` (INV-8). `exit=0`, no autosave
left, no orphaned `katago`.

Conversely, `kill -9` on a run with a loaded record leaves its `autosave-<pid>-<start>-<n>.sgf`
behind, and the next start offers it — one file per window, most recent first, deleted once the
prompt is answered either way. That asymmetry is the feature; a script ending in `quit` behaves
like a kill, because `app.quit()` destroys windows rather than emitting `close-request`.

**(f) Two windows, one KataGo.** Sharing is the reason `EnginePool` exists, and the count is
the proof:

```sh
./target/debug/mirai "$SGF" &                       # scratch XDG dirs, as above
sleep 10; pgrep -f 'linux-x64/katago analysis' | wc -l   # 1
./target/debug/mirai "$OTHER_SGF"; echo "exit=$?"   # 0: adopted by the running instance
sleep 10; pgrep -f 'linux-x64/katago analysis' | wc -l   # still 1
sleep 30; ls "$XDG_DATA_HOME/mirai"                 # two autosave-… files: two live windows
```

**(g) The local-engine editor in Preferences — managed and custom analysis config.** *This
recipe was executed; the trace below is its real output.* The icon button on a profile row is
matched by its tooltip, so the editor page is drivable.

```sh
s=/tmp/mirai-ui; mkdir -p "$s/config/mirai"
# one local profile in config.toml, carrying only the katago and model paths
XDG_CONFIG_HOME="$s/config" XDG_DATA_HOME="$s/data" \
  MIRAI_HARNESS="wait:4000,action:win.preferences,wait:1000,press:Edit this profile,wait:1200,shot:/tmp/ui-managed.png,quit" \
  ./target/debug/mirai
```

```text
harness: press "Edit this profile" -> ok
harness: wrote /tmp/ui-managed.png
```

| Profile in `config.toml` | What the PNG must show |
|---|---|
| No `config` key (mirai generates the analysis config) | Configuration shows `Managed by mirai`; the custom-config row is hidden. Search subtitles read `0 uses mirai's default (4)` and `(16)`. Batching and memory is visible. With model candidates, the model row carries a list button beside its folder button |
| `config = "…"` (a custom file) | Custom mode shows the config row, with a list button of its own when configs were discovered. Batching and memory is hidden; Search subtitles read `0 keeps the value from your analysis config` |

Use short paths — a symlink under `/tmp` for the KataGo binary and network — or the rows grow
wide enough that the page no longer fits in the captured window.

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

`win.open` and `win.save-as` open file choosers the harness cannot fill in — pass the SGF on
the command line instead of driving `win.open`. `win.preferences` *is* drivable: its rows are
ordinary buttons, so `press:` reaches them (recipe (g)).

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
| "mirai did not shut down cleanly" every start | An autosave is only deleted by the `close-request` teardown; something is killing the app, or a leftover from an earlier crash has not been answered yet | `window::connect_close`, `window::stale_autosaves` |
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
| Engine config generation (`tuning.rs`, `engines.rs`) or the local-engine page in `prefs.rs` | Recipe (g) in both modes, and look at both PNGs: managed hides the file row and shows Batching and memory, custom does the opposite |

- [ ] Every new file carries the `SPDX-License-Identifier: GPL-3.0-or-later` header.
- [ ] No `GDK_BACKEND` anywhere in the tree.
- [ ] No test-only branch inside feature code.
- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` are clean.
- [ ] Invariants you touched still read true in [ARCHITECTURE.md](ARCHITECTURE.md) and
      [PROTOCOL.md](PROTOCOL.md).

## 9. Known gaps

Reported, not fixed.

| Gap | Detail |
|---|---|
| **First-run discovery is best-effort** | `discover_katago` searches `PATH` for the executable, then user XDG `katago/models` and `mirai/models`, `~/.katago/models`, and finally both namespaces under the system XDG data roots. A downloaded release tree not installed into those locations still lands the user in Preferences. Deliberate: discovery follows explicit application-data contracts instead of package-layout guesses; there is no `~/.mirai` fallback |
| **No end-to-end client↔server test** | `remote.rs` covers the pin store, backoff, engine selection and two failure paths. Nothing spawns a server on loopback and runs a real `Open`/`Report`/`Cancel` exchange, so the handshake and INV-3's remote half are hand-verified only. This is the largest gap; a `#[tokio::test]` with an in-process server and a stub `Engine` would close it without needing KataGo |
| **No test for a changed fingerprint** | Pins round-trip, but nothing asserts that a *different* fingerprint is refused |
| **`app.rs` tests cover only `SpeedMeter`** | `AppState` is the single source of truth (INV-7) and its property/signal wiring — precisely what the `explicit_notify` defect broke — is still uncovered. A display-free test could assert that every generated setter still emits `notify::` |
| **`panels/analysis.rs` and `dialogs.rs` have no tests; `prefs.rs` has one** | All display-bound. `prefs.rs` covers only the chooser's naming rule (`only_candidates_sharing_a_name_carry_their_directory`); `panels/analysis.rs` has the most extractable pure logic (candidate row formatting, blunder rows) |
| **The harness is only parser-tested** | `activate`, `press` and `shot` need a display and are exercised only by the recipes above |
| **No golden-image comparison** | Screenshots are read by a human; nothing detects a slow visual regression between runs |
