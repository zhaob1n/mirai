<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Testing and verification

How to prove a change to mirai works. Contributor entry is
[`AGENTS.md`](../../AGENTS.md). Frame cost is [`RENDERING.md`](RENDERING.md); the
candidate-colour ramp is [`CANDIDATE_COLOUR.md`](CANDIDATE_COLOUR.md).

This is a GTK4 app on Wayland. An external grab of the XWayland root is black, so
the application screenshots itself (§5). A `shot:` proves *what* is drawn, not how
fast: capture re-enters `snapshot()` after the geometry has settled
([`RENDERING.md`](RENDERING.md) §6). Do not rerun frame benchmarks after every
iteration. Measure the completed feature set before merging, or sooner when a
change touches a per-frame path or a stutter was observed. The instrument is
`crates/mirai/src/render_probe.rs` and `tools/perf/`.

## 1. Quick reference

```sh
cargo test --workspace                                   # must be green
cargo clippy --workspace --all-targets -- -D warnings    # must exit 0
cargo build --release --workspace
cargo test -p mirai-proto --test wire_size -- --nocapture   # prints the measured byte budget
```

For documentation edits, run `python tools/docs/check-links.py`: it checks local files and
heading anchors outside fenced examples, and reports the optional adjacent `mirai-hmos`
link separately.

`cargo fmt --all --check`, `blueprint-compiler lint crates/mirai/src/window.blp
crates/mirai/src/new_game.blp crates/mirai/src/label_editor.blp
crates/mirai/src/preferences.blp crates/mirai/src/profile_editor.blp
crates/mirai/src/fox_picker.blp crates/mirai/src/panels/analysis.blp`, and the clippy line above are quiet under the current
stable toolchain. Between full runs, `cargo fmt` and `cargo clippy -p <crate>` on what you touched are
enough. A toolchain bump that lights up untouched code is its own change.

| Never run casually | Why |
|---|---|
| `cargo clippy --fix` | Rewrites files you did not read and may be held by another agent. Fix lints individually. |

There are no doc-tests: the `//!` examples in `harness.rs` and `probe.rs` are plain `text`
fences and are never compiled. They can rot; treat them as prose.

## 2. What is covered where

Six crates. The suite defends behaviour. It is not a name index: when a test name is
needed, `cargo test -p <crate> -- --list`.

| Crate | Behaviour the suite proves | Command / fixture |
|---|---|---|
| `mirai-core` | INV-1 geometry; each ruleset's KataGo string; ko, suicide, captures, Zobrist; positional versus situational superko, and that edits keep `NodeId`s; scoring totals (territory **plus the prisoners that side holds**), not the margin alone; SGF escaping, collections, branches, and a corrupt `MRAI` ignored rather than fatal | `cargo test -p mirai-core`. Parser fixtures: `crates/mirai-core/tests/data/katago-selfplay.sgf` (mirai's writer; regenerate in §4) and `lizzieyzy-autoGame1.sgf` (another writer's bytes, vendored) |
| `mirai-proto` | INV-6 quantisation and the INV-2 flip; the frame length cap enforced *before* allocate; a control frame's zstd plaintext stops inflating at the read's limit; trailing bytes and a flag the stream does not allow refused; a subscription stream decodes frame by frame, compresses a repeat to a back-reference, refuses a bomb and an oversized window, and restores ownership sent as changes; a stream cannot run more than one window ahead of its reader; cert reuse; a mismatched pin refused as a mismatch, not as a generic failure; an openssl-style pin of the real certificate connects | `cargo test -p mirai-proto`. The byte budget is the `wire_size` command in §1 — read the printed numbers, do not copy them into this table |
| `mirai-engine` | The JSON handed to KataGo (empty `avoidMoves` dropped, never `analyzeTurns`, `terminate` behind INV-3); decode order and Black-perspective values; a candidate cap keeps the engine's best, not KataGo's first; reporting perspective forced on the command line; a comma in an override path is an error; the generated analysis config carries every key KataGo demands and is not rewritten when unchanged; calibration selects the measured winner and covers the thread product; a dead remote address fails instead of hanging; the MRP session rules against a scripted server | `cargo test -p mirai-engine`. These tests never start KataGo. A real engine is §4 |
| `mirai-client` | A request built from the last setup/`PL` boundary, with territory komi taken from that boundary; a sweep hands back a result before the rest of the plan is dispatched; blunder drop measured from the mover, above the 2% noise floor; an illegal move changes nothing; dirty is a document token; TOFU waits until trusted; temperature 0 is deterministic and one bad report does not resign; Fox dialect normalised before `sgf::parse` | `cargo test -p mirai-client` |
| `mirai-server` | Token shape; a misspelled key is an error; relative paths resolve against the config directory; the example config parses; exact-match auth (no `[[token]]` rejects everyone); a client cannot raise its own priority; an off-board or oversized request is refused with `BadRequest` before the engine sees it, and only allow-listed overrides reach the engine; a silent pre-auth connection is closed and its session slot returns; a cancelled subscription stops even when its stream is not read, and keeps its `max_subs` slot until it has; an oversized first frame, compressed or not, or `Hello` field is refused as `BadRequest` and kept out of the log | `cargo test -p mirai-server`. Example: `crates/mirai-server/server.example.toml` |
| `mirai` | Display-free projections only: config merge and discovery order, Clear Board keeps size/rules/komi, the slider spans the line through the cursor, Fox normalisation, widget geometry lifted out of `snapshot()`, harness grammar, a dropped engine acquire does not strand the profile, two acquires share one start, and dropping the pool mid-start closes that start's receiver. Nothing drawn | `cargo test -p mirai`. What the window looks like is §5 |

## 3. Testing philosophy

What deserves a test, and what does not, is [AGENTS.md](../../AGENTS.md) (Testing
expectations). This section is only the boundary `cargo test` cannot cross.

The suite is headless. It does not start GTK and it does not start KataGo. Two
defects it structurally cannot catch:

1. **Rendered output.** A transposed overlay is obvious in a PNG and invisible to
   any affordable assertion about `snapshot()`. Drive it through §5 and look at
   the picture.
2. **Real concurrent timing.** The engine-activation race needed two engines with
   different startup latencies selected in quick succession. Its guard is the
   activation counter in `AppState::activate_profile`. Its proof is a log beside
   a screenshot, not another unit test.

## 4. Verifying against a real engine

`cargo test` never starts KataGo. `crates/mirai-engine/examples/probe.rs` does: it
drives a `LocalEngine` or a `RemoteEngine` behind the same `Engine` trait, so the
two paths can be diffed line for line. `probe --help` is the authoritative flag
list. The ones that matter are `--katago/--model/--config` (local),
`--remote/--token/--engine/--fingerprint` (remote), and
`--visits/--size/--moves/--komi/--rules/--report-every-ms` (the position).

```sh
# KataGo on PATH, a network, and the analysis config mirai generated for itself —
# the same three the application runs with. The model path below is one net the
# empty-board figures in this section were measured on, not a claim about which
# net is current. Another net moves those figures.
#
# `a4-s16-b64-c20` in the config name *is* `EngineTuning::default()`: 4 analysis
# threads, 16 search threads, batch 64, cache 2^20. Change anything in
# Preferences → Engines and mirai writes a differently named file beside it.
# `sweep` and the fixture generator write the default themselves when `--config`
# is omitted.
export KATA=katago
export MODEL=~/.katago/models/b10c512h8nbt3tflrs-fson-silu-rsnh.bin.gz
export CFG=~/.local/share/mirai/katago-logs/katago-analysis-a4-s16-b64-c20.cfg

# local
cargo run -p mirai-engine --example probe -- \
    --katago "$KATA" --model "$MODEL" --config "$CFG" \
    --moves D4,Q16 --visits 500 --komi 7.5 --rules chinese

# remote, against a server brought up as in §6
cargo run -p mirai-engine --example probe -- \
    --remote mirai://127.0.0.1:9678 --token "$TOKEN" --engine default \
    --fingerprint "$(cargo run -q -p mirai-server -- --print-fingerprint)" \
    --moves D4,Q16 --visits 500
```

Correct shape: one `engine:` line, a run of `[report]` blocks, exactly one
`[final]`. Version, thread count and the numbers move with the binary and the
config; this is the layout, from one recorded local run:

```text
engine: local katago 1.16.4 model <name> threads 2 human_model false
[report] visits=57 ownership=361
  D4 35.61 -1.23 14
```

Columns are GTP point, win rate **as a percentage for the side to move**, score
lead for the side to move, visits. The list is in KataGo's `order`, so line one
is the engine's choice. `ownership=361` on a 19×19 board is one entry per point.
Exit code is 0 on `Done`, 1 on `Failed`, on error, and on Ctrl-C.

Run both modes with identical arguments and diff the `[final]` block. Candidate
order and visits must match. Values must agree to within the INV-6 tolerances in
[`AGENTS.md`](../../AGENTS.md). Anything larger is a defect in the remote path,
not noise. Bringing the server up, and proving a drop actually stops KataGo, is
§6 — do not keep a second copy of that procedure here.

### Sweeping a whole record

`crates/mirai-engine/examples/sweep.rs` is the same driver pointed at an SGF. It
analyses every n-th main-line position and prints one CSV row per candidate:
rank, visits, win rate, score lead, and the four losses against the engine's own
pick — `dwin`, `dpts`, `dutil` (mean utility, what the live colour uses) and
`dlcb` (its lower bound). It exists so the ramp in `crates/mirai/src/palette.rs`
can be refitted against real games rather than guessed. What those columns mean,
and why the mean won, is [`CANDIDATE_COLOUR.md`](CANDIDATE_COLOUR.md).

```sh
cargo run -p mirai-engine --example sweep -- \
    --katago "$KATA" --model "$MODEL" --visits 1000 --every 4 game.sgf > sweep.csv
```

`--config` is optional; without it the built-in tuning writes one into the
temporary directory. The shipped ramp was fitted on a 197-move Fox game at 1 000
and 5 000 root visits, a 29-move engine self-play record, a decided 9×9 endgame
and a 9×9 opening. Search depth below ten visits is not a colour, not a label
and not full opacity (`TRUSTED_VISITS`). A breakpoint moved next should be
remeasured this way, not reasoned about.

### Measuring the wire

`crates/mirai-engine/examples/wire_bench.rs` prices a report on the wire from a
real search. Feed it raw `katago analysis` output; it decodes each line with the
engine's own decoder, frames each report alone and then the way a
subscription stream carries them, round-trips every frame, and prints bytes and
microseconds per report. A change to anything in [`PROTOCOL.md`](PROTOCOL.md)
§4 or §7 is measured this way before and after, not reasoned about; §11 there
holds the current figures.

```sh
Q='{"id":"cap","boardXSize":19,"boardYSize":19,"rules":"chinese","komi":7.5,"moves":[["B","Q16"],["W","D4"],["B","Q4"],["W","D16"],["B","R10"],["W","C10"],["B","K16"],["W","K4"],["B","O17"],["W","F17"],["B","F3"],["W","O3"],["B","C6"],["W","R14"],["B","C14"],["W","R6"],["B","J10"],["W","K10"]],"includeOwnership":true,"includePVVisits":true,"includePolicy":true,"analysisPVLen":15,"reportDuringSearchEvery":0.1,"maxVisits":10000000,"overrideSettings":{"maxTime":20}}'
printf '%s\n' "$Q" | "$KATA" analysis -model "$MODEL" -config "$CFG" \
    -override-config reportAnalysisWinratesAs=BLACK > /tmp/mrp-capture.jsonl
cargo run --release -p mirai-engine --example wire_bench -- /tmp/mrp-capture.jsonl
cargo run --release -p mirai-engine --example wire_bench -- /tmp/mrp-capture.jsonl --policy
```

The capture asks for everything (`pvVisits`, policy) so that one file serves
every row: the bench strips what a row does not send. `--cap` sets the candidate
cap (10, the default display), `--levels` the zstd levels to compare. The file
is several megabytes and stays out of the tree.

### The two SGF fixtures

`crates/mirai-core/tests/data/` holds one record from each side of the writer,
because they fail differently.

| Fixture | Defends | Regenerable |
|---|---|---|
| `katago-selfplay.sgf` | mirai's own round trip on real search output: evaluations packed into one root `MRAI` blob keyed by document order, so any drift in that keying shows up | yes, below |
| `lizzieyzy-autoGame1.sgf` | bytes mirai did not write. KataGo self-play too (`PB[]`/`PW[]`), serialised by LizzieYzy Next, so it carries a property order, empty values, embedded newlines and foreign analysis blobs mirai's writer cannot produce | no: vendored; the program that wrote it is not in this tree |

`crates/mirai-client/examples/selfplay.rs` produces the first one. KataGo plays
itself at a fixed visit cap, each position's evaluation is attached to its node,
and the last position fans out into the engine's top continuations. The path is
`request_for_node` → engine → `analysis_of` → `sgf::write`, which is the path
the GUI saves on. A fixture assembled any other way only proves the test agrees
with itself.

```sh
cargo run -p mirai-client --example selfplay -- \
    --katago "$KATA" --model "$MODEL" --moves 25 --visits 1000 \
    --out crates/mirai-core/tests/data/katago-selfplay.sgf
```

The search is not reproducible, so a rerun yields a different game. The test
asserts structure and legality rather than which moves were chosen. Byte length,
node count and candidate count come from the file — take the new ones from what
the run prints.

A replacement for the second fixture has to come from some *other* program's
writer. Do not reach for a downloaded human game: it is somebody's property
([`FOX_KIFU_API_SPEC.md`](FOX_KIFU_API_SPEC.md) §11), and the Fox dialect is
already covered by the tests in `mirai-client/src/fox.rs`, which run before
`sgf::parse` sees those bytes.

### Telling a defect from a model difference

The empty-board win rate is the network's, not a bug. On
`b10c512h8nbt3tflrs-fson-silu-rsnh`, at 200 visits, it was about **36.0%** for
Black, not the 45–55% one might assume. Another net moves it. Settle the
question against raw KataGo before touching code:

| Step | Command | What was observed, on that net |
|---|---|---|
| 1. Ground truth outside mirai | pipe the identical query into `katago analysis` (below) | `rootInfo.winrate` ≈ **0.3597**, against probe's **35.97%**. Two 200-visit runs differed by 0.13%, so "agrees" means inside that spread, not to the last digit |
| 2. Perspective | komi sweep, `--komi 0 / 7.5 / 30` | 95.2% → 36.0% → 0.1%, monotonically decreasing. Only possible if the value is Black's (INV-2) |
| 3. Index order | probe an asymmetric position and read the ownership ends | `ownership[0]` = A19 top-left = Black, `ownership[360]` = T1 bottom-right = White (INV-1) |

`reportAnalysisWinratesAs` is a **config** key, not a query field. KataGo answers
a query carrying it with `Unexpected or unused field` and then reports
side-to-move win rates, which reads exactly like an INV-2 violation that is not
there. mirai forces the key on the command line, so a hand-run cross-check has
to as well:

```sh
printf '%s\n' '{"id":"gt","boardXSize":19,"boardYSize":19,"rules":"chinese","komi":7.5,"moves":[],"maxVisits":200}' \
  | "$KATA" analysis -model "$MODEL" -config "$CFG" \
      -override-config reportAnalysisWinratesAs=BLACK | jq -c '.rootInfo'
```

| Observation | Verdict |
|---|---|
| Differs from an expectation, but raw KataGo agrees with mirai | Model difference. Fix the expectation, never tune code toward a number |
| Differs from raw KataGo on the identical query | Real defect — start at `decode.rs`, then `query.rs` |
| Differs between local and remote for the same query | Real defect in the quantiser or wire format — start at `mirai-proto` `types.rs` |
| Moves run to run at the same visit cap | Expected. KataGo search is not deterministic across thread schedules. Compare order, sign and magnitude, not digits |

## 5. Testing the GUI

Derived from `crates/mirai/src/harness.rs`, `window.rs`, `window_shell.rs` and
`main.rs`. Recipes are for a **debug** build. Tune `wait:` for the local engine.
stderr is the record of which steps completed.

The window this drives is review-first. The editor toolbar starts collapsed
(`editor_revealer`, `reveal-child: false`); `action:win.toggle-editor` opens it.
Navigation sits under the board only — first/prev/next/last, branch, slider,
`move_position` (`n / N · B|W`), Editing Tools, Board Menu — not in a
window-wide bottom bar, and not as a second analysis readout. Clocks, Undo,
Pass and Resign are a separate `play_bar`, hidden until a game is active; while
one is running or being scored, the graph, `nav` and the sidebar are hidden.
The sidebar's candidate list shows `# / Move / Win / Score / Visits`. Loss and
Prior stay hidden until `action:win.toggle-candidate-details`. The graph is
always Black's (`Black 36.0%` on the cursor; tooltip names Black win rate and
Black score lead). The sidebar opens with one status line — the side-to-move
stone, visits, and speed while a live search runs — and its rows are the side
to move.

Action names are whatever `window::install_actions` registers. A typo logs
`MISSING`. The recipes below name the actions they need. `win.open` and
`win.save-as` open file choosers the harness cannot fill — pass the SGF on the
command line.

### Why the process screenshots itself

The session is Wayland, with XWayland for X11 clients. The compositor owns
window contents, so a grab of the XWayland root is black. `GDK_BACKEND=x11`
makes an external grab work and is **not acceptable**: mirai ships on Wayland,
and a test that only passes on another backend is not testing the shipped
configuration. There is no `GDK_BACKEND` in the repo. Adding one is a regression
in the test, not a fix.

`harness::shot` asks the window's own GSK renderer:

```rust
let paintable = gtk::WidgetPaintable::new(Some(&window));
let snapshot = gtk::Snapshot::new();
paintable.snapshot(&snapshot, w as f64, h as f64);
let node = snapshot.to_node().ok_or("nothing was drawn")?;

let renderer = window.native().and_then(|n| n.renderer()).ok_or("the window has no renderer")?;
let texture = renderer.render_texture(&node, None);
texture.save_to_png(path)
```

That is the native Wayland backend, and it includes the custom `snapshot()` of
`BoardView`, `WinrateGraph` and `MoveTreeView` (INV-9).

### Real code paths, not a test-only path

`harness::activate` calls `WidgetExt::activate_action` on the active window.
`action:win.toggle-analysis` therefore reaches **exactly the handler
<kbd>space</kbd> reaches**: the same `GAction`, installed once in
`window::install_actions`. Names beginning `app.` go to the application.

`harness::press` walks the visible dialog when one is presented, otherwise the
window. It matches only visible, mapped, sensitive controls, so a modal toast's
**Undo** cannot hit the background editor's **Undo**. For a `gtk::Button` it
tries the label, then the first `gtk::Label` in the subtree (an
`adw::ButtonContent`), then the tooltip — which is how `press:Edit this profile`
and `press:Editing Tools` reach an icon-only button. A matching `gtk::MenuButton`
is popped up; its entries are then ordinary buttons. A visible menu item
(`MenuItem`, `MenuItemCheckbox`, `MenuItemRadio`) is activated by label, which
is how a board context-menu item is chosen after `board:menu:`. Allow the
popover to map first: `board:menu:D4,wait:1000,press:Set as Main Line`.

Native `adw::ButtonRow` and `adw::ActionRow` controls activate by title.
`gtk::Expander` toggles by label: `press:Blunders` folds the list. A blunder row
is one line: emoji stone, move number, loss, played point, and an optional best,
with the severity class on the title. `press:played` activates the first row
because that word is in the title.

`page:` opens a Preferences page by title (`Engines`, `Analysis`, `Play`,
`Appearance`). The dialog must already be up (`action:win.preferences`). Three
pages each end in **Restore Defaults**; switch the page first, then
`press:Restore Defaults`, so the visible row is the one you mean.
`set:` writes an `adw::SpinRow` by title (`set:Maximum Visits=2000`). Those rows
have no steppers, so `press:` cannot reach a number.
`stack:` shows an `adw::ViewStack` page. The sidebar switcher is not a
`GtkButton`, so `press:Moves` does not work; `stack:Moves` does.
`sort:` sorts the first `GtkColumnView` by column title. A header is not a
button. Repeating the step flips direction. Loss and Prior are not sortable
targets until `win.toggle-candidate-details` has shown them; hiding the column
that is the current sort returns the sort to `#`.

`board:` finds the mapped board, computes the intersection from its layout, and
calls the same hit-test handler as the released mouse gesture. In review,
`board:secondary:` deletes the branch in Play, toggles the opposite colour in
Setup, and does nothing with mark tools. `board:menu:` never edits. This
verifies the production handler, not physical Wayland delivery.

Traps:

- Mnemonic underscores are stripped (`press:Save` matches `_Save`).
- Substring match is depth-first. Choose a needle unique to the control, or
  switch the page first when titles repeat.
- A popover lives on its own surface, so its contents never appear in a `shot`.
  Verify a chooser by what picking an entry *does*.
- `adw::AlertDialog` responses are response ids, not buttons we construct.
  [INFERENCE] `press:Close` works only if libadwaita realises them as labelled
  buttons; unverified. End such recipes with `shot` then `quit`.
- Fox's search entry debounces. Use
  `fill:Exact Fox nickname=…,wait:500,press:Search`, not an immediate press
  while Search is disabled.

### Step grammar

`MIRAI_HARNESS` is one comma-separated script, parsed by `harness::parse`.

| Step | Meaning | Delay after |
|---|---|---|
| `wait:<ms>` | Sleep. Non-numeric is **dropped**, not treated as zero | — |
| `wait-status:<substring>` | Wait up to 10 s for a visible label on the active window to contain the text | — |
| `action:<prefix.name>` | Activate an action with no parameter | 120 ms |
| `action:<prefix.name>=<string>` | Activate with a string parameter (`action:win.set-engine=workstation`, `action:win.edit-tool=triangle`) | 120 ms |
| `press:<label substring>` | Activate the first matching mapped, sensitive button, action row or menu item; toggle an expander. Scoped to the visible dialog when one is up | 250 ms |
| `page:<preferences page title>` | Open that `adw::PreferencesPage` (`page:Analysis`). The dialog must already be presented | 250 ms |
| `set:<row title>=<number>` | Set the first visible matching `adw::SpinRow` (`set:Maximum Visits=2000`). Non-numeric is dropped, not treated as zero | 250 ms |
| `select:<row title>=<index>` | Set the first visible matching `adw::ComboRow`. Index 0 is its prompt or default | 250 ms |
| `stack:<view stack page title>` | Show that `adw::ViewStack` page — `stack:Moves` for the branch graph | 250 ms |
| `sort:<column title>` | Sort the first `GtkColumnView` by that column, and flip direction if it is already primary | 250 ms |
| `fill:<placeholder>=<text>` | Fill the first visible `gtk::SearchEntry` or `gtk::Entry` whose placeholder matches | 120 ms |
| `board:<primary\|secondary\|menu\|hover>:<GTP>` | Click the mapped board through its production handler, or with `hover` move the pointer there through the motion handler (ghost stone, candidate preview). `menu` is Shift+secondary. Invalid, pass and off-board coordinates fail without editing | 120 ms |
| `shot:<path.png>` | Render the active window to PNG | see below |
| `shot:<path.png>=<widget id>` | Same render, cropped to one widget. Ids are Blueprint's (`blunder_expander`, `nav`, …) | see below |
| `divider:<px>` | Move the board/graph divider so the graph is that tall, through the `set_position` a drag ends in; the paned clamps it at the graph's minimum (80 once laid out). `NOT LAID OUT` while the graph is hidden or unallocated | 250 ms |
| `scroll:<px>` | Scroll the first mapped `ScrolledWindow` that has room to scroll by that many pixels through its vertical adjustment, where the wheel and scrollbar end. Clamped to the range. `NOTHING TO SCROLL` when none is mapped or the content fits | 250 ms |
| `close-window` | Close only the active window through its normal shutdown path | 250 ms |
| `quit` | `app.quit()`, ending the script | — |

Unknown kinds are logged and skipped. Whitespace around steps is trimmed, so a
script may wrap. `mod harness` is `#[cfg(debug_assertions)]` and `install`
returns immediately when `MIRAI_HARNESS` is unset — **a release build ignores
the variable. Always use a debug build.**

Every step logs to stderr:

```text
harness: 6 steps
harness: wait-status "Ready" -> ok          # TIMEOUT = the text never became visible
harness: action win.next10 -> ok            # MISSING = no such window action
harness: page "Analysis" -> ok              # NOT FOUND = dialog not up, or no such page
harness: set "Maximum Visits"=2000 -> ok    # NOT FOUND = row hidden or title wrong
harness: close-window -> ok                 # NO WINDOW = none remained
harness: wrote /tmp/mirai-a.png             # or: harness: screenshot failed: <reason>
harness: quitting
```

**The retry loop.** `WidgetPaintable::snapshot` yields no render node if the
window has not drawn since the last change, so `shot` `queue_draw()`s and
retries twelve times at 120 ms. A `shot:` costs 120 ms at best and about 1.4 s
at worst. `screenshot failed: nothing was drawn` after all twelve means the
window never mapped — usually a too-short preceding `wait:`, or a modal that
grabbed before `present()`.

**Cropping.** `shot:/tmp/x.png=blunder_expander` renders the *window* and passes
the widget's bounds as the viewport. A `WidgetPaintable` of the widget alone
draws no ancestor background. Ids resolve by `GtkWidget:name` or buildable id.
`no widget id "x"` means the id is wrong; `"x" is not mapped yet` means the
widget is hidden — the blunder expander is invisible until a sweep finds
something.

### Recipes

Preconditions for anything that needs an engine: a debug build, and a profile
already in the isolated `config.toml`. A *missing* file is not "no engine".
`Config::load` calls `Config::seeded()`, which writes a local profile when it
finds `katago` on `PATH` and a `*.bin.gz` in the discovery directories. An
explicit `engine_profile = []` is the empty case, and it does **not** open
Preferences. The window comes up; the sidebar shows **No Engine Configured**
with a Preferences button. Opening an SGF that has `MRAI` on the current node
shows the analysis panel instead, with no visits/s speed. See recipe (h).

A local KataGo takes several seconds to load its net, which is why the first
`wait:` is generous. Toggling analysis before it is ready is safe:
`AppState::set_engine` calls `restart_analysis` when the engine lands.

```sh
export SGF=$PWD/crates/mirai-core/tests/data/katago-selfplay.sgf
export RUST_LOG=info,mirai=debug
```

**Isolation — a harness run must not touch the developer's session.**

| Shared thing | What happens without isolation |
|---|---|
| The bus name | mirai is a unique `GApplication`. A second launch hands its SGF to the running instance and exits 0, so the script runs nowhere |
| `~/.config/mirai/config.toml` | close saves config, so a run that changed a display option persists it into the developer's settings. `save_merged` keeps other *windows'* keys, not other people's |
| `$XDG_DATA_HOME/mirai` | autosaves and `katago-logs` live here. A crash leaves the developer a restore prompt for a record they never opened |
| Keyboard focus | the compositor focuses a new window. A scripted run takes the developer's focus mid-keystroke, though no step needs it |

With `MIRAI_HARNESS` set, `harness::application_flags` adds `NON_UNIQUE` (debug
builds only), so a harnessed run is its own primary instance, and
`harness::application_id` makes it `io.github.mirai.Mirai.Harness` — the Wayland
`app_id` a compositor matches. Keep focus with a rule on that id; on niri:

```kdl
window-rule {
    match app-id="io.github.mirai.Mirai.Harness"
    open-floating true
    open-focused false
}
```

`open-on-workspace` a named, unshown workspace keeps the window off screen too, and
`shot:` still captures it. Not for `MIRAI_FRAMES`: niri throttles frame callbacks
for windows no output shows.

Redirect the configuration and data yourself — `directories::ProjectDirs` honours the XDG
variables:

```sh
scratch=$(mktemp -d); mkdir -p "$scratch/config/mirai" "$scratch/data"
cp ~/.config/mirai/config.toml "$scratch/config/mirai/"   # keep the engine profile
XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
  MIRAI_HARNESS="…" ./target/debug/mirai "$SGF"
rm -rf "$scratch"
```

`tools/ui/sized-shot.sh WIDTH HEIGHT "<script>" [args…]` does exactly that, and on niri also
sets the window size: `0 0` keeps the size mirai asked for (needs a window rule that floats
the harness id), a nonzero size floats the window and forces that size.

**Never use `dbus-run-session` to get a second instance.** It costs the login
session its accessibility bus: the GTK client activates `org.a11y.Bus` on the
private bus, `at-spi-bus-launcher` rewrites `$XDG_RUNTIME_DIR/at-spi/bus_0`, and
when the private session exits the socket file outlives its listener. Every GTK
application started afterwards logs `Unable to connect to the accessibility bus`
until `systemctl --user restart at-spi-dbus-bus`. `GTK_A11Y=none` does not
prevent it. `NON_UNIQUE` removes the reason to want a private bus.

**(a) Load an SGF, navigate, live analysis, screenshot.** A positional path is
opened through `connect_open`.

```sh
MIRAI_HARNESS="wait:2000,action:win.next10,action:win.next10,action:win.toggle-analysis,wait:12000,shot:/tmp/mirai-a.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Expect `harness: 7 steps`, the actions `ok`, and `wrote /tmp/mirai-a.png`. The
PNG is the board at move 20, editor toolbar **absent**, position label under the
board, graph filled and labelled `Black …%`, sidebar on Analysis with five
columns. Candidate colour is utility loss against the pick (cyan at the cool
end, grey below ten visits) — not a visit heatmap;
[`CANDIDATE_COLOUR.md`](CANDIDATE_COLOUR.md). The sidebar percentage is the side
to move. Insert `action:win.toggle-candidate-details` to show Loss and Prior,
and `stack:Moves` for the branch graph: main line down from the root, variations
to the right, cursor wearing an accent ring.

**(b) Ownership overlay — the INV-1 canary.** Live analysis always requests
`Want::OWNERSHIP`. `win.toggle-ownership` only switches drawing on.

```sh
MIRAI_HARNESS="wait:2000,action:win.last,action:win.toggle-analysis,wait:15000,action:win.toggle-ownership,wait:1500,shot:/tmp/mirai-own.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Correct: dark shading sits over Black's stones and the territory they surround,
light over White's. On this fixture the dark region is the bottom-left.

```text
correct (index = y*w + x)        transposed (index = x*h + y)
+-------------------+            +-------------------+
|                   |            |     ##            |
|                   |            |     ##            |
|  ####             |            |     ##            |
|  ######   O       |            |  ..........       |
|  ####             |            |                   |
+-------------------+            +-------------------+
 shading covers the stones        mirrored about the main diagonal
```

A vertical flip is subtler: right shape, reflected top-to-bottom. Either way the
giveaway is shading that does not touch the stones it belongs to.

**(c) New game, engine reply, undo, score estimate.**

```sh
MIRAI_HARNESS="wait:8000,action:win.new-game,wait:800,press:Start Game,wait:1200,action:win.pass,wait:12000,shot:/tmp/mirai-play1.png,action:win.undo,wait:1200,shot:/tmp/mirai-play2.png,action:win.score,wait:20000,shot:/tmp/mirai-score.png,quit" \
  cargo run -p mirai
```

| Step | Why it works |
|---|---|
| `wait:8000` | `new_game::present` reads `engine_desc()` before offering human-like strength, so the engine should be up first |
| `press:Start Game` | Dialog defaults: 19×19, Chinese, komi 7.5, no handicap, you play Black, no time control, 800 visits per engine move |
| `action:win.pass` | With a session active this routes to `PlayController::pass`, accepted only on the human's turn. Black passes; the engine replies. The editor stays collapsed and its toggle is disabled for the game |
| `shot:…play1.png` | One White stone. Clocks, Undo, Pass and Resign are on `play_bar`, under the board nav, not in the header |
| `action:win.undo` | `PlayController::undo` removes up to two nodes so the human is on move again |
| `action:win.score` | `window::do_score` subscribes at high priority with `Want::OWNERSHIP` and shows the result dialog |

`press:Start Game` logging `NOT FOUND` means the dialog was not up yet — raise
the preceding `wait:`. A toast that there is no engine to estimate with means
KataGo never came up; check the log directory (§7).

**(d) Whole-game analysis.** `win.analyse-game` is `BatchAnalysis::start`. Each
node is analysed to `analysis.batch_visits` (100 by default). Concurrency follows
`numAnalysisThreads` and saturates at sixteen.

```sh
MIRAI_HARNESS="wait:2000,action:win.analyse-game,wait:90000,shot:/tmp/mirai-batch.png,shot:/tmp/mirai-blunders.png=blunder_expander,quit" \
  cargo run -p mirai -- "$SGF"
```

Expect the graph filled end to end, blunder marks on the worst moves, and
one-line blunder rows (emoji stone, coloured title, `played` / optional `best`
in the same line). If the progress indicator is still running, raise the wait.
The second capture is the list alone. A record where one side answers a corner
with `A19` produces both colours and all three severities within ten moves,
which reviews the row without a 90 s sweep.

**(e) Clean shutdown.** `close-window` calls `gtk::Window::close` on the active
window, the same path as the title-bar button. `dispose` is the backstop.

```sh
rm -f "$XDG_DATA_HOME/mirai"/autosave-*.sgf

MIRAI_HARNESS="wait:2000,action:win.next10,action:win.toggle-analysis,wait:15000,shot:/tmp/mirai-before-close.png,close-window" \
  ./target/debug/mirai "$SGF"
echo "exit=$?"

ls "$XDG_DATA_HOME/mirai"       # no autosave-*.sgf
pgrep -a katago                 # nothing left from this run
```

`MiraiWindow::shutdown` runs once and drops the window's `Ui`. `Drop for Ui` is
the release: flush the comment, cancel batch, play and analysis, save config,
clear the engine, delete the autosave. `exit=0`, no autosave, no orphaned
KataGo. There is no `Ui::shutdown`.

`kill -9` bypasses both close and dispose. A loaded record leaves its autosave,
and the next start offers it. That asymmetry is intentional.

**(f) Two windows, one KataGo.** Do **not** set `MIRAI_HARNESS` here: `NON_UNIQUE`
would start a second engine, which is the opposite of the proof. Match `pgrep`
to the binary this profile actually starts.

```sh
./target/debug/mirai "$SGF" &                       # scratch XDG dirs, as above
sleep 10; pgrep -af 'katago analysis' | wc -l       # 1
./target/debug/mirai "$OTHER_SGF"; echo "exit=$?"   # 0: adopted by the running instance
sleep 10; pgrep -af 'katago analysis' | wc -l       # still 1
sleep 30; ls "$XDG_DATA_HOME/mirai"                 # two autosave files: two live windows
```

**(g) Local-engine editor — managed and custom analysis config.** The icon
button on a profile row is matched by its tooltip.

```sh
s=/tmp/mirai-ui; mkdir -p "$s/config/mirai" "$s/data"
# one local profile, katago and model paths only
XDG_CONFIG_HOME="$s/config" XDG_DATA_HOME="$s/data" \
  MIRAI_HARNESS="wait:4000,action:win.preferences,wait:1000,press:Edit this profile,wait:1200,shot:/tmp/ui-managed.png,quit" \
  ./target/debug/mirai
```

| Profile in `config.toml` | What the PNG must show |
|---|---|
| No `config` key | Analysis config shows `Managed by mirai`. Search subtitles read `0 uses mirai's default (4)` and `(16)`. Batching and Memory is visible |
| `config = "…"` | Custom mode shows the config row. Batching and Memory is hidden. Search subtitles read `0 keeps the value from your analysis config` |

`page:Analysis,set:Maximum Visits=2000,press:Restore Defaults` is how a script
changes a number and puts it back. Undo on the toast restores the previous
value; profiles are not reset. Use short paths, or the rows grow wider than the
captured window.

**(h) No engine, and cached analysis without one.** Write the empty list. Do not
omit the file.

```sh
printf 'engine_profile = []\n' > "$scratch/config/mirai/config.toml"
XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
  MIRAI_HARNESS="wait:1500,shot:/tmp/mirai-empty.png,quit" \
  ./target/debug/mirai
```

Expect the game window, sidebar StatusPage **No Engine Configured**, a
Preferences button, board and nav usable, and **no** Preferences dialog.
`press:Preferences` opens it; startup does not.

```sh
XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
  MIRAI_HARNESS="wait:1500,shot:/tmp/mirai-cached.png,quit" \
  ./target/debug/mirai "$SGF"
```

The self-play fixture has `MRAI` on its nodes. Expect the analysis panel, not
the StatusPage; visits in the detail line and no `/s`; the graph still reads
`Black …%`. The sidebar percentage is the side to move.

## 6. Verifying the remote path

This is the loopback and cancellation runbook. §4 only diffs a `[final]` block
once the server is already up.

**Bring up a server.**

```sh
cargo run -q -p mirai-server -- --generate-token        # 64 hex chars; needs no config file
mkdir -p ~/.config/mirai && cp crates/mirai-server/server.example.toml ~/.config/mirai/server.toml
$EDITOR ~/.config/mirai/server.toml                     # paste the token, set the engine paths
cargo run -q -p mirai-server -- --print-fingerprint     # creates the cert if missing
RUST_LOG=info,mirai_server=debug cargo run --release -p mirai-server
```

`server.example.toml` is the annotated reference;
`the_documented_minimal_example_parses` keeps it honest. `--config` and
`--listen` override the file. With no `[[token]]` block the server starts and
warns that every client will be rejected. Lines to look for — version and
thread count are whatever this process started:

```text
INFO engine ready engine=default katago_version=<version> analysis_threads=<n> human_model=false
INFO mirai-server listening listen=127.0.0.1:9678 engines=1 tokens=1
INFO certificate fingerprint (pin this in the client) sha256=<64 hex>
```

**Point a client at it.**

`probe --remote` is the fastest check (§4). For the GUI, add a remote profile
(`url`, `token`, optional `engine`) to the isolated `config.toml`. Field
meanings are the client settings document [`AGENTS.md`](../../AGENTS.md) points
at. Leave `cert_sha256` out on the first connect: the app shows
`dialogs::confirm_fingerprint` with the colon-grouped digits, and accepting
writes the pin via `Config::set_pin`. Compare it with `--print-fingerprint`
before accepting. Switch engines with `action:win.set-engine=<profile>`.

**Confirm a subscription opened.**

```text
INFO connection open session=1 peer=127.0.0.1:53412
INFO authenticated session=1 token=laptop max_subs=4
INFO open subscription session=1 sub=1 engine=default moves=20 max_visits=Some(1000000) max_candidates=Some(10) priority=4
INFO subscription done session=1 sub=1 visits=6500
```

`moves=20` is INV-4 on display: the whole position travelled with the request.
Priority identifies the caller — live analysis 4, whole-game analysis 0, score
estimate 8 — each clamped into the served band.

**Prove cancellation.**

```sh
# terminal 1
RUST_LOG=info cargo run --release -p mirai-server
# terminal 2, then Ctrl-C
cargo run -p mirai-engine --example probe -- \
    --remote mirai://127.0.0.1:9678 --token "$TOKEN" --visits 1000000
```

Compare timestamps. The recorded run logged `connection closed` and
`subscription dropped` **within 1 ms** of the client's SIGINT, because `probe`
has a `ctrl_c` arm that drops the subscription *and* the engine and gives quinn
200 ms to flush `CONNECTION_CLOSE`. `kill -9` sends nothing, and UDP has no FIN,
so the server notices only via the ~30 s QUIC idle timeout. If you measure 30 s,
check how you killed the client before filing a bug.

Then prove KataGo stopped, by sampling `/proc/<pid>/stat` utime+stime as a
**rate**:

```sh
pid=$(pgrep -n katago)
read _ _ _ _ _ _ _ _ _ _ _ _ _ u1 s1 _ < /proc/$pid/stat; sleep 4
read _ _ _ _ _ _ _ _ _ _ _ _ _ u2 s2 _ < /proc/$pid/stat
echo "$(( (u2+s2-u1-s1) * 100 / (4 * $(getconf CLK_TCK)) ))% CPU over 4s"   # expect 0
```

`ps` `%CPU` is a lifetime average and read 11.4% for a completely idle process
in the recorded check. To prove something stopped, measure a rate, not a total.
The same sampling proves the local path, where the subscription guard enqueues
a KataGo `terminate`.

## 7. Debugging playbook

Symptom, then the cause or the guard, then where it lives. Measurements that
are already a section of [`RENDERING.md`](RENDERING.md) are not copied here.
A black external screenshot is §5, not a second essay.

| Symptom | Cause / protection | Where |
|---|---|---|
| A `notify::` handler or `bind_property` target silently stopped firing | `explicit_notify` on a **derive-generated** setter. It disables automatic `notify::`. It belongs only on a property whose hand-written setter emits the signal itself. Three properties name a custom setter: `live_analysis`, `ownership_overlay`, `policy_overlay` | `app.rs`, the `#[properties]` block on `imp::AppState` |
| A widget will not shrink, or one pane eats the window | `gtk::Paned` resize/shrink flags, or a hardcoded position. The graph wants `resize-end-child: false` and no fixed split. An unset paned sizes the graph by its *minimum* request and clips a shrinkable child instead of shrinking it, which is why the graph pins its remembered height as the minimum until the first layout | `window.blp` content paned (`resize-end-child: false`, `shrink-end-child: false`); `WinrateGraph::pin` / `release` |
| The first window leaves bare background beside or under the board | `fit_default_size` measured the chrome wrong, or the window is tiled: niri ignores the default size unless a window rule floats mirai | `window::fit_default_size`; `mirai::window` debug log `default window size` |
| The sidebar page switcher is missing, and a stray `✕` sits in its place | `adw::HeaderBar::show_title(false)` hides the *title widget*. That widget **is** the `InlineViewSwitcher`. The `✕` is a second set of window controls | `window.blp` sidebar header: `show-start-title-buttons` / `show-end-title-buttons` false, `show-title` left on |
| An engine connects, then vanishes seconds later; the server logs a connection with no subscription | Activation race. `activate_profile` is async and a local KataGo takes seconds, so an older activation can finish last | The activation counter in `AppState::activate_profile`. `discarding a superseded engine activation` at debug means the guard worked |
| Live analysis restarts, but reports keep arriving for the old position | `generation`, bumped by `restart_analysis` and checked before a report is applied | `app.rs` `restart_analysis` |
| "mirai did not shut down cleanly" on every start | The autosave is deleted by dropping the window's `Ui`. Either the process was killed, or a leftover from an earlier crash has not been answered | `Drop for Ui`, `window::stale_autosaves` |
| The restore prompt offers an empty board | `GameTree::has_content` regressed. An autosave with no move, setup, mark, to-play override or comment is neither written nor offered | `tree.rs` |
| Engine and runtime survive window close | A long-lived callback owns the window, or shutdown never took the `Ui`. There is no `Ui::shutdown`. `close-request`, `dispose` and `ApplicationImpl::shutdown` all call `MiraiWindow::shutdown`, which `take_ui`s once. `Drop for Ui` aborts tasks, flushes the comment, cancels batch, play and analysis, saves config, clears the engine, and drops the autosave | `MiraiWindow::shutdown`, `Drop for Ui`, weak-window callbacks in `window.rs`, `play.rs`, `batch.rs` |
| Sidebar says **No Engine Configured**, and that is treated as a failed open | No profile, no live report, and the current node has no cached analysis. The window is up. The StatusPage button is `win.preferences`; `present` does not open the dialog. A *missing* file may still seed a discovered engine (`Config::seeded`). An explicit `engine_profile = []` is the empty case | `update_analysis_page`, `prefs::no_engine_status_page`. Recipe (h) |
| Cached numbers missing on an SGF that has them, or a fake live speed with no engine | The panel is shown when the *current* node has analysis, even with an empty profile list. Speed is attached only to a live report | `update_analysis_page`, `AnalysisPanel::refresh` |
| Sidebar reads 9.9% while the graph reads `Black 90.1%` | Not a double conversion. The sidebar is the side to move (`winrate_for`). The graph is always Black | `AnalysisPanel::refresh`; `WinrateGraph` cursor text and tooltip |
| Editing tools are missing | They start collapsed. `win.toggle-editor`, or the nav button. A non-Play tool expands them. Active play forces them shut and disables the toggle | `set_editor_visible`; `window.blp` `editor_revealer` |
| Loss and Prior columns are gone | Hidden until `win.toggle-candidate-details`. Hiding the column that is the current sort returns the sort to `#` first. The objects still hold the values | `AnalysisPanel::set_detailed_columns` |
| Overlay shading in the wrong place | Someone remapped indices. INV-1: ownership and policy index identically to the board | `decode.rs`, then recipe (b) |
| Win rates inverted for one side, on *both* the sidebar and the graph | INV-2 violated: a conversion applied somewhere other than display | `winrate_for` / `score_lead_for` are the sanctioned sites |
| Territory totals low by the prisoner count, margin right | The "territory minus prisoners you lost" formula. Each side scores territory **plus the prisoners it holds** | `score.rs` |
| Engine will not start, no useful message in the GUI | KataGo's own log | `$XDG_DATA_HOME/mirai/katago-logs` (GUI, `Config::data_dir`; also `mirai-server` when an `[[engine]]` has no `log_dir`), `$TMPDIR/mirai-katago-logs` (`LocalEngineConfig` default), or `log_dir` in `server.toml`. `PermissionDenied … refusing` means the directory for a generated config, or a directory on its path, is another user's or writable by others — group write counts unless the directory is yours and in your own primary group (`atomic::private_dir`) |
| `Startup(…)` mentioning `logDir` or a config key | A comma in a path. KataGo splits `-override-config` on commas | `local.rs` `override_config` |
| Client cannot connect though the server is up | Fingerprint mismatch after a regenerated cert, wrong token, or wrong `[[engine]]` name | `--print-fingerprint` versus `cert_sha256`. §6 |
| Cancellation looks like 30 s | The client was `kill -9`'d. UDP has no FIN. Ctrl-C through `probe`'s `ctrl_c` arm is the measurement | §6 |
| A `queue_draw` from inside `snapshot()` does nothing | GTK clears `draw_needed` *after* the vfunc returns. The next frame is a tick callback, not a GLib idle — an idle runs between frames, at a priority the frame clock outranks, and its repaint can land mid-animation | `BoardView::redraw_next_frame`. [`RENDERING.md`](RENDERING.md) §6 |
| `size_allocate` goes quiet on the plateau frames of a spring | `gtk_widget_allocate` returns early when the pixel size, baseline and `alloc_needed` are unchanged. Do not use it as "something is still animating", and do not key text deferral on it | `board.rs` `snapshot`; [`RENDERING.md`](RENDERING.md) §6 |
| Board text stutters in one direction of a sidebar fold, and not the other | Glyphs are cached per `PangoFont`, so moving the board is free and a `cell` change is not. `Layout::cell` tracks the sidebar only while the board is width-limited. Defer text while `cell` is moving, and only then | [`RENDERING.md`](RENDERING.md) §6 |
| A geometry value repeating for one frame is treated as the animation ending; the next frame flickers | Width is an integer. A spring's last frames move under a pixel. One repeat paints a cold glyph pass and throws it away. `BoardView` waits for `CELL_SETTLED` (2) | `board.rs`; [`RENDERING.md`](RENDERING.md) §6 |
| A frame-timing bug that reproduces only on an idle machine | Load coarsens the fold — about 13–16 animation frames instead of 22–31 — and a coarse animation never lands on a repeated value. Check `/proc/loadavg` and frames per fold before trusting a clean run. A `shot:` cannot see this: capture re-enters `snapshot()` at the current `cell`, so the PNG always has its text | [`RENDERING.md`](RENDERING.md) §6 |
| Frames drop while the engine is searching, and the GPU sits at 99% | Not the GPU. Another process pinning the same card costs this one nothing. Replacing the `GtkColumnView` model per report rebuilds every row widget: a full window relayout (measured 7–17 ms) and a hovered row that flickers. Mutate `CandidateObject`s in place and bind cells with expressions. Blunder rows are kept and updated in place for the same reason | `AnalysisPanel::refresh`, `Row::apply`, `set_blunders`. [`RENDERING.md`](RENDERING.md) §7 |
| Whole-game analysis sits at 0/N and then completes in one jump | A loop that awaits a permit per position dispatches the *whole* plan before it joins anything, so the first result arrives only once all but `concurrency` searches are done. Refill the `JoinSet` inside the join loop. `running.len() < concurrency` is the whole cap | `mirai_client::batch::sweep` |
| `harness: action … -> MISSING` | No such action on the active window, or a typo. `app.*` goes to the application | `harness::activate`, `window::install_actions` |
| `harness: screenshot failed: nothing was drawn` | The window never mapped, or a modal grabbed before `present()` | Lengthen the preceding `wait:` |
| The harness does nothing at all | Release build (`#[cfg(debug_assertions)]`), `MIRAI_HARNESS` unset, or every step malformed | `harness::install` |

`RUST_LOG` (both binaries use `EnvFilter`, defaulting to `info`):

| Value | Use |
|---|---|
| `info,mirai=debug` | GUI internals: engine activation, the superseded-activation guard, autosave warnings |
| `info,mirai_engine::local=trace` | Every line exchanged with the KataGo process |
| `info,mirai_engine::remote=debug` | Reconnect backoff, TOFU pinning, stream lifecycle |
| `info,mirai_server=debug` | Adds `cancel for an unknown subscription`, `subscription stream closed early` |
| `debug` | Everything, including quinn and rustls. Loud — scope it |

## 8. Before declaring work done

The workspace commands are §1. Do not paste them again. What else the change
owes:

| Change touches | Evidence |
|---|---|
| `mirai-core` rules, scoring, SGF | `cargo test -p mirai-core` is enough; the workspace run is §1 |
| `mirai-proto` types, scales, framing | the `wire_size` command in §1, and read the printed numbers |
| `mirai-engine` query or decode | `probe` locally, and diff a `[final]` against raw `katago analysis` on the identical query (§4) |
| `remote.rs` or `mirai-server` | both probe modes on the same position, the subscription log, and the cancellation rate in §6 |
| Anything drawn | at least one harness recipe, and look at the PNG. Recipe (b) for anything touching `Point`, ownership or policy |
| Anything drawn *per frame* — a new pass in `snapshot()`, or text on the board | `MIRAI_FRAMES=1` over a sidebar fold with the search **finished**, then `tools/perf/frame-stats.py`. No animation frame past the refresh interval. A fold *during* a search misses frames on the report's own layout whatever you drew — that is not your regression ([`RENDERING.md`](RENDERING.md) §6–§8) |
| Signals, properties, `Rc` capture, teardown | Recipe (e): `exit=0`, the autosave gone, no orphaned `katago` |
| A new `GAction` or accelerator | Drive it once through `action:` and confirm `ok`, not `MISSING` |
| Engine config generation (`tuning.rs`, `engines.rs`) or the local-engine page | Recipe (g) in both modes, and look at both PNGs |

- [ ] Every new `.rs` file carries the `SPDX-License-Identifier: GPL-3.0-or-later` header.
- [ ] No `GDK_BACKEND` anywhere in the tree.
- [ ] No test-only branch inside feature code.
- [ ] §1 is clean on what you touched, and on the workspace before merge.
- [ ] Invariants you touched still read true in [`ARCHITECTURE.md`](ARCHITECTURE.md) and
      [`PROTOCOL.md`](PROTOCOL.md).

## 9. Known gaps

These are ability boundaries, not a backlog. Do not file them as missing tests.

| Boundary | What it means |
|---|---|
| The harness does not deliver physical Wayland input | `action:`, `press:`, `page:`, `set:`, `board:` and the rest call production handlers. They do not synthesize a key, a double-click, or a compositor event. Say so if that is what was checked |
| No image golden | A person reads the PNG. That is the check. A pixel oracle fails on the next margin and is not a gap to fill |
| GUI claims need a real run | `cargo test` stays headless. A green suite does not show the window, and it does not start KataGo |
| A `shot:` is not a frame time | Capture re-enters `snapshot()` after geometry has settled, so the PNG always has the text a moving board deferred. Per-frame cost is `MIRAI_FRAMES` and [`RENDERING.md`](RENDERING.md) |
