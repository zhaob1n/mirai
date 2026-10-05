<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Testing and verification

How to prove a change to mirai works. Contributor entry is
[`AGENTS.md`](../../AGENTS.md). Frame cost is [`RENDERING.md`](RENDERING.md); the
candidate-colour ramp is [`CANDIDATE_COLOUR.md`](CANDIDATE_COLOUR.md).

| To prove | Use |
|---|---|
| what is drawn | the in-process harness's `shot:` (§5); Wayland root grabs are black |
| frame time, or observed stutter | `MIRAI_FRAMES=1` and `tools/perf/` as in [`RENDERING.md`](RENDERING.md). `tools/perf/ui-survey.sh` runs the `dialogs`, `fox` and `main` scenarios by default (request live analysis with `--engine analysis`) and prints frames over the display's budget, step by step |
| a glitch that costs no time | `tools/perf/dialog-settle.py` records a dialog opening with gpu-screen-recorder and prints how far its last frames are from rest |

## 1. Quick reference

```sh
cargo test --workspace                                   # must be green
cargo clippy --workspace --all-targets -- -D warnings    # must exit 0
cargo build --release --workspace
cargo test -p mirai-proto --test wire_size -- --nocapture   # prints the measured byte budget
```

`just check` runs formatting, all-target Clippy with warnings denied, and the full
workspace suite with the lockfile — the checks every commit owes.

| Changed | Check |
|---|---|
| Rust | `cargo fmt --all --check`. Avoid `cargo clippy --fix`: it rewrites files you have not reviewed |
| Blueprint | `blueprint-compiler lint crates/mirai/src/*.blp crates/mirai/src/panels/*.blp` |
| Documentation | `python tools/docs/check-links.py`: local files and heading anchors, and it refuses a source file cited with a line number — cite a symbol or a section, which survives the next edit. The optional adjacent `mirai-ohos` link is reported separately |
| Translations | the GUI build script runs `msgfmt --check` over the catalogues in `po/LINGUAS` (`crates/mirai/build.rs`) |
| Desktop entry, metainfo | run `desktop-file-validate` and `appstreamcli validate` on what `just install` wrote |

`harness.rs` and `probe.rs` have prose examples, not doc-tests.

## 2. What is covered where

Six crates. The suite defends behaviour. It is not a name index: when a test name is
needed, `cargo test -p <crate> -- --list`.

### `mirai-core`

`cargo test -p mirai-core`. Parser fixtures: `crates/mirai-core/tests/data/katago-selfplay.sgf` (mirai's writer; regenerate in §4) and `lizzieyzy-autoGame1.sgf` (another writer's bytes, vendored).

- INV-1 geometry
- Each ruleset's KataGo string
- Ko, suicide, captures, Zobrist
- Positional versus situational superko, and that edits keep `NodeId`s
- Scoring totals (territory **plus the prisoners that side holds**), not the margin alone
- SGF escaping, collections, branches, compressed `ul:lr` point lists, and a corrupt `MRAI` ignored rather than fatal

### `mirai-proto`

`cargo test -p mirai-proto`. The byte budget is the `wire_size` command in §1 — read the printed numbers, do not copy them here.

- INV-6 quantisation and the INV-2 flip
- The frame length cap enforced *before* allocate
- A stream cut anywhere inside a frame, header included, is an error rather than a clean end
- A control frame's zstd plaintext stops inflating at the read's limit
- Trailing bytes and a flag the stream does not allow refused
- A subscription stream decodes frame by frame, compresses a repeat to a back-reference, refuses a bomb and an oversized window, and restores ownership sent as changes
- A stream cannot run more than one window ahead of its reader
- Cert reuse
- A mismatched pin refused as a mismatch, not as a generic failure
- An openssl-style pin of the real certificate connects
- A probe returns the fingerprint and opens no stream

### `mirai-engine`

`cargo test -p mirai-engine`. These tests never start KataGo. A real engine is §4.

- The JSON handed to KataGo (empty `avoidMoves` dropped, never `analyzeTurns`, `terminate` behind INV-3)
- Decode order and Black-perspective values
- A candidate cap keeps the engine's best, not KataGo's first
- Reporting perspective forced on the command line
- A comma in an override path is an error
- A user-supplied config is overridden only for the two thread counts
- The generated analysis config carries every key KataGo demands and is not rewritten when unchanged
- Calibration selects the measured winner and covers the thread product
- Dropping a subscription forgets it before the tail is decoded
- A decode failure still terminates the search
- A dead remote address fails instead of hanging
- A subscription stream without its preamble fails the connection instead of hanging
- The MRP session rules against a scripted server
- A probe returns the fingerprint and sends no Hello, and a wrong pin fails before Hello

### `mirai-client`

`cargo test -p mirai-client`.

- A request built from the last setup/`PL` boundary, with territory komi taken from that boundary
- A sweep hands back a result before the rest of the plan is dispatched
- Blunder drop measured from the mover, above the 2% noise floor
- An illegal move changes nothing
- Dirty is a document token
- An unpinned connect probes and waits, and Trust is what sends the pin
- Temperature 0 is deterministic and one bad report does not resign
- Fox dialect normalised before `sgf::parse`
- eWeiqi's GIB read top-down with commentary on the move it follows and its variation diagrams kept off the main line
- Yike's Chinese results, exact-name candidates and paged lists that stop where they should
- Malformed eWeiqi dates and extreme Fox rank / Yike result numbers do not panic
- Only a single step onto a placed stone sounds, and a capture is told by the stones it removed

### `mirai-server`

`cargo test -p mirai-server`. Example: `crates/mirai-server/server.example.toml`.

- Token shape and placeholder rejection
- Misspelled keys, duplicate or overlong tokens and a zero `max_subs` refused
- Relative paths resolved against the config directory
- An authenticated client cannot exceed its token's cross-connection subscription quota or raise priority
- Off-board, oversized or unbounded requests constrained before KataGo, with only allow-listed overrides forwarded
- Silent pre-auth and blocked error writes release session slots
- Cancellation stops the search even when its stream is not read, and `STOP_SENDING` alone stops a search that is not reporting
- A pipelined burst is answered in full, while a peer that stops reading its control stream loses its searches
- Oversized/compressed Hello refused without logging secrets

### `mirai`

`cargo test -p mirai`. Display-free projections and dispatch only; nothing is drawn, and what the window looks like is §5.

- A comment or mark does not run the `Tree` refresh and one move projects once
- A returning remote link re-requests analysis, only for the window's current engine
- A score overlay is drawn on the real position, never a pinned variation
- A pick is never faded
- Config merge and discovery order
- A missing Human model does not overwrite the saved strength
- Handicap only where the board has points
- Cache power 0 or at least 2^14
- Clear Board keeps size/rules/komi
- The slider spans the line through the cursor
- Widget geometry lifted out of `snapshot()`
- Harness grammar
- A server's reply is taken only from a 2xx, within its size cap (a loopback libsoup server)
- The record search history keeps one search per server and query, most recent first, drops the oldest past its limit, and round-trips through its file
- `EnginePool` sharing, stranding and teardown

## 3. Testing philosophy

Test expectations are in [AGENTS.md](../../AGENTS.md#testing-expectations).
`cargo test` starts neither GTK nor KataGo. Check rendered output with a harness
PNG (§5); check timing-sensitive engine behaviour with a real engine and logs
(§§4–6). The suite cannot prove either.

## 4. Verifying against a real engine

`cargo test` never starts KataGo. `crates/mirai-engine/examples/probe.rs` does: it
drives a `LocalEngine` or a `RemoteEngine` behind the same `Engine` trait, so the
two paths can be diffed line for line. `probe --help` is the authoritative flag
list. The ones that matter are `--katago/--model/--config` (local),
`--remote/--token/--engine/--fingerprint` (remote), and
`--visits/--size/--moves/--komi/--rules/--report-every-ms` (the position).

```sh
# Use the same KataGo binary, model and generated config as the GUI.
# The model below was used for the example numbers in this section; other nets differ.
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

`--config` is optional; without it built-in tuning writes a temporary config.
Re-measure ramp changes against real games; the fitted samples and visit floor
are in [`CANDIDATE_COLOUR.md`](CANDIDATE_COLOUR.md).

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

The second fixture must come from another writer. Do not substitute a downloaded
human game; see [`FOX_KIFU_API_SPEC.md` §9](FOX_KIFU_API_SPEC.md).
Fox normalisation is already covered in `mirai-client/src/fox.rs`.

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

The harness lives in `crates/mirai/src/harness.rs`; use a **debug** build. Tune
`wait:` for the local engine. Stderr records completed steps; a failed, missing,
timed-out or malformed step makes the process exit 1 after normal shutdown.
Successful scripts exit 0. Window layout is mapped in
[`ARCHITECTURE.md` §7](ARCHITECTURE.md#7-gui-architecture).

Action names are whatever `window::install_actions` registers. A typo logs
`MISSING`; an action that exists but is disabled logs `DISABLED` and fails the
script, since GTK would accept the activation and drop it. The engine actions
are disabled without an engine: `win.toggle-analysis` until one is starting,
`win.analyse-game` and `win.score` until it is ready.
The recipes below name the actions they need. `win.open` and
`win.save-as` open file choosers the harness cannot fill — pass the SGF on the
command line.

### Why the process screenshots itself

The compositor owns Wayland window contents; an XWayland root grab is black.
Do not force `GDK_BACKEND=x11`: test the shipped backend. `harness::shot` uses
the window's `WidgetPaintable` and GSK renderer, including custom widgets'
`snapshot()` (INV-9).

### Real code paths, not a test-only path

`action:` activates the window's production `GAction` (or the application for
`app.`); `action:win.toggle-analysis` reaches the same handler as <kbd>space</kbd>.
`press:` searches the visible dialog if present, otherwise the window. It
matches mapped, sensitive controls by label/title, nested label or tooltip;
use `press:Edit This Profile` for an icon button. Menu buttons open their
popover; wait for it to map before choosing an item:
`stack:Moves,tree:secondary:2:1,wait:400,press:Set as Main Line`. Action rows and expanders
activate by title or label (`press:Blunders`).

`page:` needs an open Preferences dialog (`action:win.preferences`); switch to
the desired page before `press:Restore Defaults` because several pages have
that button. `set:` targets numeric rows (`set:Maximum Visits=2000`), which
have no steppers for `press:`. `stack:Moves` selects the sidebar page;
`press:Moves` cannot activate its switcher. `sort:` targets column titles;
show Loss and Prior with `win.toggle-candidate-details` before sorting them.
Hiding the sorted column resets sorting to `#`.

`board:` and `tree:` enter the production hit-test handlers, not physical Wayland
input. In review, board secondary-click deletes a branch in Play, toggles the
opposite colour in Setup, and does nothing with mark tools.

Traps:

- Mnemonic underscores are stripped (`press:Save` matches `_Save`).
- Needles match the text on screen, which follows the locale. Run English scripts with
  `LANGUAGE=en`, or write the needles in the language you run.
- Substring match is depth-first. Choose a needle unique to the control, or
  switch the page first when titles repeat.
- `wait-status:`, `set:`, `select:` and `fill:` match mapped widgets, not a widget's
  own `visible` flag: a hidden Preferences page keeps that flag on its children.
- A popover lives on its own surface, so its contents never appear in a `shot`.
  Verify a chooser by what picking an entry *does*.
- `press:` walks visible dialog controls, including mapped response buttons
  when libadwaita renders them as `gtk::Button`; response ids alone are not
  matching labels. If no matching button is mapped, capture with `shot:` and
  end with `quit`.
- The record picker's search entry debounces. Use
  `fill:Exact Fox nickname=…,wait:500,press:Search`, not an immediate press
  while Search is disabled. The server toggles are buttons: `press:eWeiqi`,
  `press:Yike`, and each server's entry has its own placeholder.

### Step grammar

`MIRAI_HARNESS` is one comma-separated script, parsed by `harness::parse`.

| Step | Meaning | Delay after |
|---|---|---|
| `wait:<ms>` | Sleep. A non-numeric argument fails the script | — |
| `wait-status:<substring>` | Wait up to 10 s for a visible label on the active window to contain the text | — |
| `action:<prefix.name>` | Activate an action with no parameter | 120 ms |
| `action:<prefix.name>=<string>` | Activate with a string parameter (`action:win.set-engine=workstation`, `action:win.edit-tool=triangle`) | 120 ms |
| `press:<label substring>` | Activate the first matching mapped, sensitive button, action row or menu item; toggle an expander. Scoped to the visible dialog when one is up | 250 ms |
| `page:<preferences page title>` | Open that `adw::PreferencesPage` (`page:Analysis`). The dialog must already be presented | 250 ms |
| `set:<row title>=<number>` | Set the first visible matching `adw::SpinRow` (`set:Maximum Visits=2000`). A non-numeric argument fails the script | 250 ms |
| `select:<row title>=<index>` | Set the first visible matching `adw::ComboRow`. Index 0 is its prompt or default | 250 ms |
| `stack:<view stack page title>` | Show that `adw::ViewStack` page — `stack:Moves` for the branch graph | 250 ms |
| `sort:<column title>` | Sort the first `GtkColumnView` by that column, and flip direction if it is already primary | 250 ms |
| `fill:<placeholder>=<text>` | Fill the first visible `gtk::SearchEntry` or `gtk::Entry` whose placeholder matches | 120 ms |
| `board:<primary\|secondary\|hover>:<GTP>` | Click the mapped board through its production handler, or with `hover` move the pointer there through the motion handler (ghost stone, candidate preview). Invalid, pass and off-board coordinates fail without editing | 120 ms |
| `tree:<primary\|secondary>:<depth>:<lane>` | Press the move-tree cell at that grid position (root is `0:0`, the main line is lane 0) through its production handler: `primary` navigates, `secondary` opens the node menu. `no mapped move tree` unless `stack:Moves` is showing | 120 ms |
| `focus:<widget id>` | Give keyboard focus to the widget with that Blueprint id or `GtkWidget:name` (`focus:comment`, `focus:editor_toggle`). `NOT FOCUSABLE` when missing or refused. Pair with `tools/ui/type-keys.sh` to see where a real key goes | — |
| `shot:<path.png>` | Render the active window to PNG | see below |
| `shot:<path.png>=<widget id>` | Same render, cropped to one widget. Ids are Blueprint's (`blunder_expander`, `nav`, …) | see below |
| `divider:<px>` | Move the board/graph divider so the graph is that tall, through the `set_position` a drag ends in; the paned clamps it at the graph's minimum (80 once laid out). `NOT LAID OUT` while the graph is hidden or unallocated | 250 ms |
| `scroll:<px>` | Scroll the first mapped `ScrolledWindow` that has room to scroll by that many pixels through its vertical adjustment, where the wheel and scrollbar end. Clamped to the range. `NOTHING TO SCROLL` when none is mapped or the content fits | 250 ms |
| `size:<w>x<h>` | Ask for that window size with `set_default_size` (a floating window on niri honours it), then log what it got: `size 902x900 -> 902x900 collapsed=true sidebar=false board=902x603 side=603`. Sweep a width across the sidebar fold and `side` must not change | 500 ms |
| `close-dialog` | Close the dialog presented over the active window through `adw::Dialog::close`, as its close button and Escape do; `NO DIALOG` when none is up | 250 ms |
| `close-window` | Close only the active window through its normal shutdown path, force-closing any dialog presented over it first (libadwaita would otherwise close the dialog and keep the window). `STILL OPEN` when the window survived; `NO WINDOW` when none remained | 250 ms |
| `quit` | `app.quit()`, ending the script | — |

Unknown or malformed steps fail the script, rather than silently skipping the
step. Targets must be nonempty: `press:`, `wait-status:`, and `select:=2` are
invalid, not requests to match the first control. Only `fill:` and a string
action's value may be empty. Bare commands (`close-dialog`, `close-window`,
`quit`) take no colon or argument. Whitespace around steps is trimmed, so a
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
harness: close-dialog -> ok                 # NO DIALOG = nothing presented
harness: close-window -> ok                 # NO WINDOW = none remained; STILL OPEN = it did not close
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
`no widget id "x"` means the id is wrong; `"x" is not mapped` means the widget
or an ancestor is hidden — the blunder expander is invisible until a sweep finds
something, and the candidate list goes with a folded sidebar.

**Menus need focus.** A context popover is an xdg_popup with a grab, and the
compositor refuses the grab for a window without keyboard focus: the popover
stays unmapped and the `press:` that follows reports `NOT FOUND`. The
`open-focused false` rule below therefore rules out `tree:secondary` →
`press:`; focus the harness window for those runs. A popover is its own
surface, so `shot:` never shows it — shoot the result after `press:` instead.

### Recipes

Engine recipes need a debug build and a profile in isolated `config.toml`.
If the file is missing, `Config::seeded()` may discover KataGo and a network
asynchronously; allow time for it. Explicit `engine_profile = []` prevents
discovery (recipe (h)). KataGo takes seconds to load; toggling analysis before
it is ready is safe because `AppState::set_engine` restarts it on arrival.

```sh
export SGF=$PWD/crates/mirai-core/tests/data/katago-selfplay.sgf
export RUST_LOG=info,mirai=debug
```

#### Isolation

A harness run must not touch the developer's session.
The unique `GApplication` otherwise forwards a second launch to the existing
window. Config writes and autosaves use the active XDG directories; a scripted
window can also steal keyboard focus.

With `MIRAI_HARNESS` set, `harness::application_flags` adds `NON_UNIQUE` (debug
builds only), so a harnessed run is its own primary instance, and
`harness::application_id` makes it `io.github.zhaob1n.Mirai.Harness` — the Wayland
`app_id` a compositor matches. Keep focus with a rule on that id; on niri:

```kdl
window-rule {
    match app-id="io.github.zhaob1n.Mirai.Harness"
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

Do not use `dbus-run-session` for a second instance: it can leave the login
session's accessibility bus socket without a listener. Debug harness runs
already set `NON_UNIQUE`.

#### Sound

`tools/ui/record-sound.sh "<script>" [args…]` uses isolated XDG directories, but writes
`engine_profile = []` instead of copying your profiles and routes mirai's audio to a private
null sink. It prints one line per sound onset, so a missing or extra stone sound shows up
as a line, without playing anything aloud. It proves timing, not level:
the null sink did not show a replayed stream losing its first 10–25 ms, which the real device
did (see `crates/mirai/src/sound.rs`). Compare loudness on `$(pactl get-default-sink).monitor` instead.

Tuning the clips themselves needs no GUI. Edit the constants in
`crates/mirai-client/src/sound.rs` — the HarmonyOS client plays the same clips — then:

```sh
cargo test -p mirai-client render_clips -- --ignored   # writes /tmp/mirai-sounds/clip0..7.wav
tools/ui/sound-levels.py                                # peaks, drop loudness, brightness
pw-play /tmp/mirai-sounds/clip0.wav                     # placement; clip2 = 1-stone capture
```

Keep every peak under 1.0 and the first drop 1.5–4 dB under the placement; if a timbre
change moves the drops' loudness, bring it back with `CLINK`.

#### (a) Load an SGF, navigate, live analysis, screenshot

A positional path is opened through `connect_open`.

```sh
MIRAI_HARNESS="wait:2000,action:win.next10,action:win.next10,action:win.toggle-analysis,wait:12000,shot:/tmp/mirai-a.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Expect `harness: 7 steps`, the actions `ok`, and `wrote /tmp/mirai-a.png`. The
PNG is the board at move 20, editor toolbar **above it**, position label under the
board, graph filled and its cursor labelled `…%` (Black's), sidebar on Analysis with five
columns. Candidate colour is utility loss against the pick (cyan at the cool
end, grey below ten visits) — not a visit heatmap;
[`CANDIDATE_COLOUR.md`](CANDIDATE_COLOUR.md). The sidebar percentage is the side
to move. Insert `action:win.toggle-candidate-details` to show Loss and Prior,
and `stack:Moves` for the branch graph: main line down from the root, variations
to the right, cursor wearing an accent ring.

#### (b) Ownership overlay — the INV-1 canary

Live analysis always requests `Want::OWNERSHIP`. `win.toggle-ownership` only switches
drawing on.

```sh
MIRAI_HARNESS="wait:2000,action:win.last,action:win.toggle-analysis,wait:15000,action:win.toggle-ownership,wait:1500,shot:/tmp/mirai-own.png,quit" \
  cargo run -p mirai -- "$SGF"
```

Correct: dark shading covers Black's stones and the bottom-left territory;
light covers White's. A transpose reflects this asymmetric region across the
diagonal.

A vertical flip is subtler: right shape, reflected top-to-bottom. Either way the
giveaway is shading that does not touch the stones it belongs to.

#### (c) New game, engine reply, undo, score estimate

```sh
MIRAI_HARNESS="wait:8000,action:win.new-game,wait:800,press:Start Game,wait:1200,action:win.pass,wait:12000,shot:/tmp/mirai-play1.png,action:win.undo,wait:1200,shot:/tmp/mirai-play2.png,action:win.score,wait:20000,shot:/tmp/mirai-score.png,quit" \
  cargo run -p mirai
```

The first wait lets KataGo load before `new_game::present` offers engine
strength. Expect a White reply to Black's pass in the first PNG; Undo returns
to the human's turn. `win.score` opens the ownership-based result dialog.

`press:Start Game` logging `NOT FOUND` means the dialog was not up yet — raise
the preceding `wait:`. `win.score` logging `DISABLED` means KataGo was not
ready yet, or never came up; check the log directory (§7).

#### (d) Whole-game analysis

`win.analyse-game` is `BatchAnalysis::start`. Each node is analysed to
`analysis.batch_visits` (100 by default), with twice `numAnalysisThreads` queries in flight,
at most sixteen (`mirai_client::batch::in_flight`).

```sh
MIRAI_HARNESS="wait:2000,action:win.analyse-game,wait:90000,shot:/tmp/mirai-batch.png,shot:/tmp/mirai-blunders.png=blunder_expander,quit" \
  cargo run -p mirai -- "$SGF"
```

Expect a filled graph, blunder marks and rows; the second capture isolates the
list. If progress is still running, lengthen the wait.

#### (e) Clean shutdown

`close-window` calls `gtk::Window::close` on the active window, the same path as the
title-bar button. `dispose` is the backstop.

```sh
rm -f "$XDG_DATA_HOME/mirai"/autosave-*.sgf

MIRAI_HARNESS="wait:2000,action:win.next10,action:win.toggle-analysis,wait:15000,shot:/tmp/mirai-before-close.png,close-window" \
  ./target/debug/mirai "$SGF"
echo "exit=$?"

ls "$XDG_DATA_HOME/mirai"       # no autosave-*.sgf
pgrep -a katago                 # nothing left from this run
```

Expect `exit=0`, no autosave and no orphaned KataGo. `MiraiWindow::shutdown`
drops `Ui` once; its `Drop` handles release (INV-8).

`kill -9` bypasses both close and dispose. A loaded record leaves its autosave,
and the next start offers it. That asymmetry is intentional.

#### (f) Two windows, one KataGo

Do **not** set `MIRAI_HARNESS` here: `NON_UNIQUE` would start a second engine, which is
the opposite of the proof. Match `pgrep` to the binary this profile actually starts.

```sh
./target/debug/mirai "$SGF" &                       # scratch XDG dirs, as above
sleep 10; pgrep -af 'katago analysis' | wc -l       # 1
./target/debug/mirai "$OTHER_SGF"; echo "exit=$?"   # 0: adopted by the running instance
sleep 10; pgrep -af 'katago analysis' | wc -l       # still 1
sleep 30; ls "$XDG_DATA_HOME/mirai"                 # two autosave files: two live windows
```

#### (g) Local-engine editor — managed and custom analysis config

The icon button on a profile row is matched by its tooltip.

```sh
s=/tmp/mirai-ui; mkdir -p "$s/config/mirai" "$s/data"
# one local profile, katago and model paths only
XDG_CONFIG_HOME="$s/config" XDG_DATA_HOME="$s/data" \
  MIRAI_HARNESS="wait:4000,action:win.preferences,wait:1000,press:Edit This Profile,wait:1200,shot:/tmp/ui-managed.png,quit" \
  ./target/debug/mirai
```

| Profile in `config.toml` | What the PNG must show |
|---|---|
| No `config` key | Analysis Config shows `Managed by mirai`. Search subtitles read `0 uses mirai's default (4)` and `(16)`. Batching and Memory is visible |
| `config = "…"` | Custom mode shows the config row. Batching and Memory is hidden. Search subtitles read `0 keeps the value from your analysis config` |

`page:Analysis,set:Maximum Visits=2000,press:Restore Defaults` is how a script
changes a number and puts it back. Undo on the toast restores the previous
value; profiles are not reset. Use short paths, or the rows grow wider than the
captured window.

#### (h) No engine, and cached analysis without one

Write the empty list. Do not omit the file.

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
Black's `…%`. The sidebar percentage is the side to move.

## 6. Verifying the remote path

This is the loopback and cancellation runbook. §4 only diffs a `[final]` block
once the server is already up.

### Bring up a server

```sh
cargo run -q -p mirai-server -- --generate-token        # 64 hex chars; needs no config file
mkdir -p ~/.config/mirai && cp crates/mirai-server/server.example.toml ~/.config/mirai/server.toml
chmod 600 ~/.config/mirai/server.toml                   # bearer token stays private
$EDITOR ~/.config/mirai/server.toml                     # paste the token, set the engine paths
cargo run -q -p mirai-server -- --print-fingerprint     # creates the cert if missing
RUST_LOG=info,mirai_server=debug cargo run --release -p mirai-server
```

`server.example.toml` is the annotated reference; its all-zero token is
deliberately invalid until replaced. `the_documented_minimal_example_requires_a_real_token`
checks the minimal config's placeholder and its replacement. `--config` and
`--listen` override the file. With no `[[token]]` block the server starts and
warns that every client will be rejected. Lines to look for — version and
thread count are whatever this process started:

```text
INFO engine ready engine=default katago_version=<version> analysis_threads=<n> human_model=false
INFO mirai-server listening listen=127.0.0.1:9678 engines=1 tokens=1
INFO certificate fingerprint (compare this with the client before trusting) sha256=<colon-grouped hex>
```

### Point a client at it

`probe --remote` is the fastest check (§4). For the GUI, add a remote profile
(`url`, `token`, optional `engine`) to the isolated `config.toml`; the fields are
[GUIDE §8](../user/GUIDE.md#the-settings-file). Leave `cert_sha256` out: selecting the profile probes and shows
`dialogs::confirm_fingerprint` with the colon-grouped digits before any token is
sent. Trust writes the pin via `Config::set_pin` and then connects; Cancel
persists nothing. Compare it with the server's startup log (the same grouping;
`--print-fingerprint` is the raw hex) before accepting. Switch engines with
`action:win.set-engine=<profile>`.

To check that editing does not discard a pin just trusted through the profile list, start
with an unpinned profile and no `active_engine` in the isolated config. With the server up:

```sh
MIRAI_HARNESS="wait:3000,action:win.preferences,wait:1000,press:desktop,wait:2500,press:Trust,wait:2500,press:Edit This Profile,wait:1200,shot:/tmp/mirai-pinned-editor.png,press:Save Profile,wait:600,quit" \
  ./target/debug/mirai
```

Use the profile's name instead of `desktop`, and compare the prompted fingerprint with the
server before trusting it. The editor must show that fingerprint, not **Not pinned yet**,
and `cert_sha256` must remain in `config.toml` after Save. The row's Edit handler reads the
current saved profile: activating it pins the certificate without rebuilding the list.

### Confirm a subscription opened

```text
INFO connection open session=1 peer=127.0.0.1:53412
INFO authenticated session=1 token=laptop max_subs=64
INFO open subscription session=1 sub=1 engine=default moves=20 max_visits=Some(1000000) max_candidates=Some(10) priority=4
INFO subscription done session=1 sub=1 visits=6500
```

`moves=20` is INV-4 on display: the whole position travelled with the request.
Priority identifies the caller — live analysis 4, whole-game analysis 0, score
estimate 8 — each clamped into the served band.

### Prove cancellation

```sh
# terminal 1
RUST_LOG=info cargo run --release -p mirai-server
# terminal 2, then Ctrl-C
cargo run -p mirai-engine --example probe -- \
    --remote mirai://127.0.0.1:9678 --token "$TOKEN" \
    --fingerprint "$(cargo run -q -p mirai-server -- --print-fingerprint)" \
    --visits 1000000
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

Measure `/proc` deltas, not `ps %CPU` (a lifetime average). This also checks
that a dropped local subscription stopped KataGo.

## 7. Debugging playbook

Symptom, cause or guard, and location. Rendering mechanics live in
[`RENDERING.md`](RENDERING.md); black external grabs are covered in §5.

### Window and widgets

| Symptom | Cause / protection | Where |
|---|---|---|
| A `notify::` handler or `bind_property` target silently stopped firing | `explicit_notify` on a **derive-generated** setter. It disables automatic `notify::`. It belongs only on a property whose hand-written setter emits the signal itself. Three properties name a custom setter: `live_analysis`, `ownership_overlay`, `policy_overlay` | `app.rs`, the `#[properties]` block on `imp::AppState` |
| A widget will not shrink, or one pane eats the window | `gtk::Paned` resize/shrink flags, or a hardcoded position. The graph wants `resize-end-child: false` and no fixed split. An unset paned sizes the graph by its *minimum* request and clips a shrinkable child instead of shrinking it, which is why the graph pins its remembered height as the minimum until the first layout | `window.blp` content paned (`resize-end-child: false`, `shrink-end-child: false`); `WinrateGraph::pin` / `release` |
| The first window leaves bare background beside or under the board | `fit_default_size` measured the chrome wrong, or the window is tiled: niri ignores the default size unless a window rule floats mirai | `window::fit_default_size`; `mirai::window` debug log `default window size` |
| The sidebar page switcher is missing, and a stray `✕` sits in its place | `adw::HeaderBar::show_title(false)` hides the *title widget*. That widget **is** the `InlineViewSwitcher`. The `✕` is a second set of window controls | `window.blp` sidebar header: `show-start-title-buttons` / `show-end-title-buttons` false, `show-title` left on |
| Editing tools are missing | They start revealed, but a game closes them and they stay closed afterwards; so does `win.toggle-editor` or the nav button. A non-Play tool expands them. Active play forces them shut and disables the toggle | `set_editor_visible`; `window.blp` `editor_revealer` |
| Loss and Prior columns are gone | Hidden until `win.toggle-candidate-details`. Hiding the column that is the current sort returns the sort to `#` first. The objects still hold the values | `AnalysisPanel::set_detailed_columns` |

### Engine and analysis

| Symptom | Cause / protection | Where |
|---|---|---|
| An engine connects, then vanishes seconds later; the server logs a connection with no subscription | Activation race. `activate_profile` is async and a local KataGo takes seconds, so an older activation can finish last | The activation counter in `AppState::activate_profile`. `discarding a superseded engine activation` at debug means the guard worked |
| Live analysis restarts, but reports keep arriving for the old position | `generation`, bumped by `restart_analysis` and checked before a report is applied | `app.rs` `restart_analysis` |
| Sidebar says **No Engine Configured**, and that is treated as a failed open | No profile, no live report, and the current node has no cached analysis. The window is up. The StatusPage button is `win.preferences`; `present` does not open the dialog. A *missing* file may still seed a discovered engine once `Config::seeded` finishes on the blocking pool. An explicit `engine_profile = []` is the empty case | `update_analysis_page`, the empty page of `analysis_stack` in `window.blp`. Recipe (h) |
| Cached numbers missing on an SGF that has them, or a fake live speed with no engine | The panel is shown when the *current* node has analysis, even with an empty profile list. Speed is attached only to a live report | `update_analysis_page`, `AnalysisPanel::refresh` |
| Sidebar reads 9.9% while the graph reads `90.1%` | Not a double conversion. The sidebar is the side to move (`winrate_for`). The graph is always Black | `AnalysisPanel::refresh`; `WinrateGraph` cursor text and tooltip |
| Overlay shading in the wrong place | Someone remapped indices. INV-1: ownership and policy index identically to the board | `decode.rs`, then recipe (b) |
| Win rates inverted for one side, on *both* the sidebar and the graph | INV-2 violated: a conversion applied somewhere other than display | `winrate_for` / `score_lead_for` are the sanctioned sites |
| Territory totals low by the prisoner count, margin right | The "territory minus prisoners you lost" formula. Each side scores territory **plus the prisoners it holds** | `score.rs` |
| Engine will not start, no useful message in the GUI | KataGo's own log | `$XDG_DATA_HOME/mirai/katago-logs` (GUI, `Config::data_dir`; also `mirai-server` when an `[[engine]]` has no `log_dir`), `$TMPDIR/mirai-katago-logs` (`LocalEngineConfig` default), or `log_dir` in `server.toml`. `PermissionDenied … refusing` means the directory for a generated config, or a directory on its path, is another user's or writable by others — group write counts unless the directory is yours and in your own primary group (`atomic::private_dir`) |
| `Startup(…)` mentioning `logDir` or a config key | A comma in a path. KataGo splits `-override-config` on commas | `local.rs` `override_config` |
| Whole-game analysis sits at 0/N and then completes in one jump | A loop that awaits a permit per position dispatches the *whole* plan before it joins anything, so the first result arrives only once all but `concurrency` searches are done. Refill the `JoinSet` inside the join loop. `running.len() < concurrency` is the whole cap | `mirai_client::batch::sweep` |

### Shutdown and autosave

| Symptom | Cause / protection | Where |
|---|---|---|
| "mirai did not shut down cleanly" on every start | The autosave is deleted by dropping the window's `Ui`. Either the process was killed, or a leftover from an earlier crash has not been answered | `Drop for Ui`, `window::collect_stale_autosaves` |
| The restore prompt offers an empty board | `GameTree::has_content` regressed. An autosave with no move, setup, mark, to-play override or comment is neither written nor offered | `tree.rs` |
| Engine and runtime survive window close | A long-lived callback owns the window, or shutdown never took the `Ui`. There is no `Ui::shutdown`. `close-request`, `dispose` and `ApplicationImpl::shutdown` all call `MiraiWindow::shutdown`, which `take_ui`s once. `Drop for Ui` aborts tasks, flushes the comment, cancels batch, play and analysis, saves config, clears the engine, and drops the autosave | `MiraiWindow::shutdown`, `Drop for Ui`, weak-window callbacks in `window.rs`, `play.rs`, `batch.rs` |

### Remote engine

| Symptom | Cause / protection | Where |
|---|---|---|
| Client cannot connect though the server is up | Fingerprint mismatch after a regenerated cert, wrong token, or wrong `[[engine]]` name | `--print-fingerprint` versus `cert_sha256`. §6 |
| Selecting a profile asks to trust a fingerprint the editor's Test Connection just trusted | Save compared the pinned URL as text, so `box` and `mirai://box` dropped the pin. `pin_for` compares parsed endpoints | `prefs.rs` |
| Cancellation looks like 30 s | The client was `kill -9`'d. UDP has no FIN. Ctrl-C through `probe`'s `ctrl_c` arm is the measurement | §6 |

### Rendering

| Symptom | Cause / protection | Where |
|---|---|---|
| A `queue_draw` inside `snapshot()` does nothing | Use a tick callback for the next frame | [`RENDERING.md` §6](RENDERING.md#6-candidate-labels) |
| `size_allocate` goes quiet during a fold | A plateau needs no new allocation; do not use allocation as the text-deferral signal | [`RENDERING.md` §6](RENDERING.md#6-candidate-labels) |
| Board text stutters only in one fold direction | Defer text only while `Layout::cell` changes | [`RENDERING.md` §6](RENDERING.md#6-candidate-labels) |
| A one-frame geometry repeat makes text flicker | Wait for two repeats (`CELL_SETTLED`) | [`RENDERING.md` §6](RENDERING.md#6-candidate-labels) |
| The frame bug appears only on an idle machine | Check `/proc/loadavg` and frames per fold; a `shot:` cannot measure this | [`RENDERING.md` §6](RENDERING.md#6-candidate-labels) |
| Frames drop during search; GPU is at 99% | Keep list objects stable and mutate in place, not a model splice per report | [`RENDERING.md` §7](RENDERING.md#7-a-report-cost-a-layout-and-it-was-never-the-gpu) |

### Input, translation and the harness

| Symptom | Cause / protection | Where |
|---|---|---|
| Typing in a comment passes, deletes a branch or toggles analysis; Ctrl+Z in a field undoes the record | The key was made an application accelerator. Those run in the window's capture phase, before the focused field. Only combinations no text field uses may be `KeyScope::Global`; check with `focus:comment` and `tools/ui/type-keys.sh` | `window::SHORTCUTS`, `view_shortcuts` |
| `harness: action … -> MISSING` | No such action on the active window, or a typo. `app.*` goes to the application | `harness::activate`, `window::install_actions` |
| `harness: screenshot failed: nothing was drawn` | The window never mapped, or a modal grabbed before `present()` | Lengthen the preceding `wait:` |
| A string stays English in a translated window | Not passed to a gettext function as a literal, so `xgettext` never saw it; or `po/` not updated since. A string from `mirai-core`/`mirai-client` is English by design and must be worded in `crates/mirai` | `tools/i18n/update-po.sh`, then `msgfmt --statistics`; [`TRANSLATING.md`](TRANSLATING.md) |
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

Run §1; choose evidence for the surface changed:

| Change touches | Evidence |
|---|---|
| Core rules, scoring, SGF | `cargo test -p mirai-core` |
| Wire types or framing | Run `wire_size` (§1) and read the numbers |
| Engine query or decode | Diff local `probe` `[final]` against raw KataGo on the identical query (§4) |
| Remote engine or server | Compare both probe modes and check subscription/cancellation (§6) |
| Drawn output | Inspect a harness PNG; use recipe (b) for point, ownership or policy changes |
| Per-frame drawing | Measure `MIRAI_FRAMES=1` with a finished search during a sidebar fold; use `tools/perf/frame-stats.py` and [`RENDERING.md` §§6–9](RENDERING.md) |
| Opening or closing a dialog, or anything that should hold 144 Hz | `tools/perf/ui-survey.sh` on the fast output for the scenarios above; for an uncovered surface, use its own `MIRAI_HARNESS` script with `MIRAI_FRAMES=1` and `tools/perf/frame-stats.py`. Read `over` per step, `action:<name>` for work between frames, and `tools/perf/frame-profile.py` for a slow frame's clock and stack; `tools/perf/dialog-settle.py` for how the text settles as the open ends ([`RENDERING.md` §8](RENDERING.md#8-dialogs-lists-and-a-160-hz-budget)) |
| Signals, properties, capture or teardown | Recipe (e): exit 0, no autosave or orphaned KataGo |
| New action | Drive `action:` and confirm `ok`, not `MISSING` |
| User-visible text | `tools/i18n/update-po.sh`, translate the new messages, and look at the window with `LANGUAGE=zh_CN` ([`TRANSLATING.md`](TRANSLATING.md)) |
| Engine config generation or editor | Recipe (g), both modes and PNGs |

Check new `.rs` files for the SPDX header, never set `GDK_BACKEND` in test or
production code, avoid test-only branches in production, and recheck affected invariants
in [`AGENTS.md` §2](../../AGENTS.md#2-non-negotiable-invariants).

## 9. Known gaps

These are ability boundaries, not a backlog. Do not file them as missing tests.

| Boundary | What it means |
|---|---|
| The harness does not deliver physical Wayland input | `action:`, `press:`, `page:`, `set:`, `board:` and the rest call production handlers. They do not synthesise a key, a double-click or a compositor event. Say so if that is what was checked |
| No image golden | Inspect the PNG; a pixel oracle breaks with the next margin change |
