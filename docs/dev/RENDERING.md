<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Rendering performance

Measurements and rationale for drawing mirai's custom widgets with quads instead
of paths. [architecture](ARCHITECTURE.md) · [testing](TESTING.md) ·
[AGENTS.md](../../AGENTS.md).

**Rule (INV-9):** in `snapshot()`, use colour, border or rounded-clip nodes
instead of fill/stroke paths for shapes they can draw. Keep necessary paths
(curves and diagonal marks) small or cached. Use the helpers in
[`widgets/paint.rs`](../../crates/mirai/src/widgets/paint.rs).

---

## 1. The symptom

Folding the sidebar visibly stuttered: its ~300 ms animation drew 3–7 frames
instead of ~18 at 60 Hz. Stepping through a late-game position repainted the
board in 90–120 ms (8–11 fps). Both exposed the same rendering cost.

## 2. How it was measured

The in-app instrument, [`render_probe.rs`](../../crates/mirai/src/render_probe.rs),
logs frame cadence (`frame-dt`) and GTK update, layout and paint phases
(`frame-phases`). `Timer` measures the drawing passes, and every window action's
synchronous cost (`action:<name>`), which runs between frames where no phase sees it. The
probes need debug assertions, not an unoptimised build:
[`ui-survey.sh`](../../tools/perf/ui-survey.sh) builds `target/perf` (release, assertions
on) and drives each surface — dialogs, the Fox picker, the main window, live analysis —
through the harness. [`frame-stats.py`](../../tools/perf/frame-stats.py) aggregates the
logs and judges each frame against the output's refresh period. [`dense-board.py`](../../tools/perf/dense-board.py) produces the 285-stone
worst case; [`numbered-game.py`](../../tools/perf/numbered-game.py) exercises
190 numbered moves, which setup stones cannot test. `MIRAI_NO_BOARD`,
`MIRAI_NO_GRAPH`, `MIRAI_NO_LABEL_DEFER`, `MIRAI_SPIN` and `MIRAI_DUMP_NODE`
are available for ablation and render-node capture in debug builds. For a stack,
`MIRAI_WRAP="perf record --call-graph dwarf -o p.data --"` profiles the same survey run;
Arch's GTK has frame pointers, but unwinding through it from Rust needs DWARF.

Measurements in §§3–5 used no engine, a ~1486×1634 window on a
6016×3384@60 Hz output at scale 2. Reset persisted `[ui]` toggles between
runs in the isolated profile. Pin the output: a 160 Hz display allows 6.2 ms
rather than 16.7 ms. Check `/proc/loadavg` and frames per fold; a busy machine
can hide the plateau defect in §6.

```sh
tools/perf/dense-board.py /tmp/dense.sgf
MIRAI_FRAMES=1 MIRAI_HARNESS="wait:2000,action:win.toggle-sidebar,wait:800,\
action:win.toggle-sidebar,wait:800,quit" ./target/debug/mirai /tmp/dense.sgf 2>frames.log
tools/perf/frame-stats.py frames.log
```

## 3. What the numbers said

**The time is in `paint`, not in layout or in our own code.** Per animation frame, before the
fix: `layout` 0.1–0.3 ms, our `snapshot()` implementations 0.04 ms (board) and 0.3 ms (graph),
`paint` **10–36 ms** on an empty board and **89–125 ms** on a full one. So the cost sat between
handing GSK a render node and the frame being done.

**It is not the renderer, and not the pixels.** Every renderer was equally slow, and a size
sweep (285 stones, before the fix) shows the cost is flat against window area — so it is not
fill rate:

| renderer (empty board) | paint median | | window | area | paint median |
|---|---|---|---|---|---|
| `vulkan` (default) | 29.1 ms | | 560×480 | 0.27 Mpx | 100.3 ms |
| `ngl` | 49.6 ms | | 800×680 | 0.54 Mpx | 83.6 ms |
| `cairo` | 27.8 ms | | 1100×940 | 1.03 Mpx | 98.8 ms |
| | | | 1400×1200 | 1.68 Mpx | 72.7 ms |

**Historical ablations** (`MIRAI_NO_WOOD`/`MIRAI_NO_GRID` were temporary):
sidebar fold on an empty board before the fix.

| build | slow frames per animation | paint median |
|---|---|---|
| unchanged | 19 | 29.6 ms |
| `MIRAI_NO_WOOD` | 28 | 20.1 ms |
| `MIRAI_NO_GRID` | 31 | 17.5 ms |
| both | **0** | 4.9 ms |
| `MIRAI_NO_BOARD` (graph only) | 4 | 9.2 ms |
| `MIRAI_NO_BOARD` + `MIRAI_NO_GRAPH` | 0 | — (max 1.5 ms) |

The board's historical ablations isolated wood (~10 ms), grid and border
(~12 ms), and per-stone paths (90–120 ms on a full board). Header, list and
other GTK controls did not account for this paint cost.

**The last experiment names the mechanism.** `MIRAI_SPIN` redraws an *unchanging* scene every
frame:

| scene, before the fix | paint median | frame interval |
|---|---|---|
| empty board, same nodes re-rendered | **0.26 ms** | 16.7 ms |
| 285 stones, nodes rebuilt each snapshot | **58.0 ms** | 68.1 ms |

An unchanged fill node is cheap to re-render: GSK caches its rasterisation by
the `GskPath` **pointer**, scale and subpixel offset
(`gsk/gpu/gskgpucachedfill.c`). Fresh paths in every `snapshot()` miss the
cache; the static board layer is likewise rebuilt when allocation changes.
A sidebar fold changes allocation every frame. Replaying an identical render
node with `gtk4-rendernode-tool benchmark` took 13 ms for the old full board
against 4 ms for the new one.

## 4. The fix

[`widgets/paint.rs`](../../crates/mirai/src/widgets/paint.rs) provides discs
as rounded clips and colour nodes, outlines as border nodes, lines as colour
rectangles, and `over()` for pre-blending. Wood, grid, stones, marks, tree
elbows and graph guides now use these primitives rather than freshly built
fill/stroke paths.

Two pixel constraints matter: opaque grid ink must be pre-blended against wood
to prevent double-blending where rectangles overlap; the tree uses one trunk
per parent for the same reason. Stone shadow discs do not overlap because
stones are 0.96 cells across.

The win-rate and score-lead curves, dashed 50 % line, triangle and cross marks
remain paths: they need curves, dashes or diagonals. Marks cover one stone;
the plot-wide curve is cached and rebuilt only when its data changes (INV-9).

## 5. After

Same machine, same scenes, same instrument:

| scene | before | after |
|---|---|---|
| sidebar fold, empty board | paint 29.6 ms, 6–8 frames missed per animation | paint 1.8–4.1 ms, **0 missed** |
| sidebar fold, 285 stones | paint 89–125 ms, 3 frames drawn per animation | paint 7.7 ms, **15 frames, 0 missed** |
| `MIRAI_SPIN`, 285 stones | paint 58.0 ms, clock at 68 ms | paint 3.5 ms, clock at 16.7 ms |
| stepping a real game record (`win.next`) | — | paint 0.9–2.6 ms |

Before/after in-app screenshots of the empty and dense boards, numbered
stones and move tree showed shape differences limited to edge antialiasing;
header text also varied with run timing.

## 6. Candidate labels

Candidate labels are Pango glyphs sized to `Layout::cell`, unlike the paths
measured with no engine in §§3–5. Their different cache key explains the cost:

| node | cache key (GTK `gsk/gpu/`) | consequence for us |
|---|---|---|
| fill / stroke | `GskPath` pointer + scale + subpixel offset | a path rebuilt per frame always misses (§3) |
| text | `PangoFont` + glyph + subpixel flags + render scale | **position is not in the key**; the font size is |
| colour / border / rounded clip | none — one shader each | nothing to miss |

So a board that only *moves* keeps every digit for free, and a board whose `cell` *changed*
re-rasterises all of them. Measured on one fold, in the same run: the candidate pass costs
**0.68 ms** on a frame whose `cell` was unchanged and **13.6 ms** (worst 27.8) on a frame whose
`cell` moved.

`cell` is `min(width/units_x, height/units_y)`: it follows sidebar width only
while the board is width-limited, so the two fold directions differ.

| fold, ~15 animation frames | frames where `cell` changed |
|---|---|
| hiding the sidebar (board grows, becomes height-limited) | 2–5 |
| showing it (board shrinks, stays width-limited) | 11–13 |

**Defer text only while `cell` moves.** Blobs, stones and marks remain visible.
After two consecutive repeats at the same `cell`, a one-shot tick callback
repaints text in the next frame.

`size_allocate` is not a settling signal: GTK can skip it when size and
baseline are unchanged, precisely on a spring's plateau, but may call it when
`cell` is unchanged. `queue_draw` in `snapshot()` is lost because GTK clears
`draw_needed` after the vfunc returns. An idle can repaint mid-animation; a
tick callback runs in the next frame's update phase before paint. A timeout
cannot detect settling in libadwaita's spring animation.

Measured with `tools/perf/dense-board.py`, live analysis on (20 candidates, 10 Hz), six folds
per run, debug build, 6016×3384@60 Hz at scale 2. `over` counts animation frames past the
16.7 ms deadline:

| rule | text on … of 89–93 animation frames | frame median | over |
|---|---|---|---|
| never defer (`MIRAI_NO_LABEL_DEFER=1`) | 100 % | 7.4 ms | **29 %** |
| defer whenever `size_allocate` ran | 12 % | 4.3 ms | 1 % |
| defer while `cell` moves | **44 %** | 4.0 ms | 2 % |

Deferral cuts missed frames from 29 % to 2 %. Unlike allocation-based deferral,
tracking `cell` retains text on 44 % rather than 12 % of animation frames at
nearly the same cost (a one-frame difference in ~90, on a busy machine).

### One repeat is not a stop

Width is integer and a spring can repeat one `cell` mid-animation. Painting
text on that plateau incurs a cold glyph pass, then discards it at the next
size: one measured false settle cost 19.3 ms and made a 30 ms frame. A repeat
is cheap only if the preceding frame already painted that size.

So text waits for `CELL_SETTLED = 2` repeats. Same binary, six folds each, 60 Hz, move numbers
on a 190-move record and no engine:

| repeats required | frames where text came back and then vanished | frames over 1.6 × the interval |
|---|---|---|
| 1 | 3, in 2 of 6 folds | 3 |
| **2** | **0** | 1 |

The remaining slow frame is the first paint at a new, settled font size:
16.5–21.8 ms versus 2.6 ms at a cached size. Two repeats remove mid-fold
flicker without hiding the text after the visible animation ends.

Load matters: a busy machine produced only 13–16 frames per fold, with no
plateau; idle folds had 22–31 frames and exposed the flicker. Verified on the
190-move record with no engine: numbers return two frames after `cell` settles.
`shot:` cannot verify this timing because capture re-enters `snapshot()` at
the current size; use the frame timeline.

Font-size quantisation would preserve text throughout the fold but visibly
step its type; 2 % missed frames did not justify it. Coordinate labels remain
in the static layer: hiding the letters would make the board unreadable, and
the unchanged-size text pass is only 0.68 ms.

## 7. A report cost a layout, and it was never the GPU

Sidebar folds dropped frames during engine searches even though drawing was
already cheap. The following isolates report-driven layout from GPU load.

Same dense board (285 stones, 20 candidates), same ~1486×1634 window, six folds per run, debug
build. `over` counts frames past the 16.7 ms deadline in the 900 ms after each toggle; `layout`
is the frame clock's layout→paint span, which is GTK measuring and allocating the window:

| condition | GPU | reports | frames per fold | over | layout max |
|---|---|---|---|---|---|
| engine searching (3 runs) | 99 % | 10 Hz | 42–57 | **25–31 %** | 15.7–17.6 ms |
| its search finished, same scene (2 runs) | 7 % | — | 60–62 | 2 % | 0.74 ms |
| **another process** pinning the GPU, own search finished | **99 %** | — | 61–62 | **1 %** | 1.55 ms |
| engine searching, `report_interval_ms = 1000` | 99 % | 1 Hz | 57–60 | 5 % | 16.0 ms |

The independent GPU-saturating process did not slow the app, whereas reducing
reports to 1 Hz reduced missed frames: this is report-rate work, not GPU
contention. KataGo used ~2.2 of 16 CPU cores and load averaged 4 in both
slow and smooth runs.

Previously `AnalysisPanel::refresh` replaced the entire `GtkColumnView` model
per report. Recreating rows forced a 7–17 ms full-window layout and visibly
reset hover shading at 10 Hz. Ablating just `store.splice` cut missed frames
from 26–29 % to 2–10 % and layouts over 5 ms from 8–10 to zero per fold.
Hiding the sidebar did not help: its child still participates in layout.

The fix is stable `CandidateObject`s updated in place. `Row::apply` only
notifies changed properties; expression-bound cells update individually and
fixed numeric widths avoid re-measuring columns. In a 20-row, six-column
list, 120 row widgets were created once over ~100 reports. With the engine
searching, folds missed 2–7 % of frames, with no layout over 5 ms — the
idle baseline. A steady, unanimated view had missed 22 % before the fix:
any new report-rate list or readout must avoid per-report widget churn.

## 8. Dialogs, lists, and a 160 Hz budget

The goal moved from 60 Hz to at least 144 Hz: 6.9 ms a frame, 6.25 ms on the 160 Hz
output. Measured with `ui-survey.sh` on a 3840×2160@160 Hz output at scale 1.5 unless
noted; release with assertions, no engine but for live analysis.

| surface | before | after |
|---|---|---|
| Fox picker, 200 records, reopen | a 116–133 ms frame (60 Hz output) | action ~1 ms, layout 1–2.5 ms |
| Fox picker, first open | layout 98–223 ms | 13 ms with Latin names; CJK below |
| Preferences | 38–130 ms action, every open | first open unchanged; then 2–9 ms |
| New Game | 21–55 ms action, every open | first open unchanged; then 2.6 ms |
| editor reveal (board resize) | board 7.4 ms a frame, 11 frames over | 0.13 ms, 0 over |
| live analysis, candidate figures | 0.75 ms a report | 0.26 ms |
| live analysis, 8 steps forward | 73–75 candidate rows built; panel refresh p90 2.1–4.7 ms, max 5.1–6.6 | 2–3 rows; p90 1.2–1.4 ms, max 1.9–2.6 |

**A list view keeps 200 rows.** `GtkListView` keeps `GTK_LIST_VIEW_MAX_LIST_ITEMS`
(200) rows alive around its anchor whatever its height (`gtklistview.c`), so a list of
Fox's 200 records was 200 bound, measured, styled rows. A one-column `GtkGridView` keeps
`GTK_GRID_VIEW_MAX_VISIBLE_ROWS` (30) plus three. Both are *inert* while unrooted: they
drop their factory, and rebind every live row synchronously when presented again. The
picker therefore empties its store before each presentation and refills it four rows a
frame (`fox_picker.rs`).

**`adw_dialog_present` measures the whole dialog, synchronously** — every page of
Preferences, every row's text. Building the template and that measure took 38–130 ms
per open; a dialog kept for the window and presented again costs 2–9 ms. Preferences,
New Game and the Fox picker are kept and reset or reloaded on each presentation.
Setting a spin row to the value it shows still formats and relays it out, so reloads
compare first.

**A resize re-shaped the coordinates every frame.** The static layer is rebuilt at each
new allocation, and each fractional font size meant Pango matching a font through
fontconfig and shaping 38 strings: 7 ms a frame. Coordinates are now set in whole
pixels and kept shaped across rebuilds. Candidate figures went through one layout whose
text and font were switched twice per line, so every figure was shaped twice per report;
one layout per line shapes it once.

**The candidate list rebuilt its rows on every step.** Under live analysis a step
emptied the store until the position's first report, which refilled it a few moves a
report. `GtkColumnView` destroys the row of each object that leaves its model and builds
one for each that arrives, synchronously inside `items-changed`, so the panel's refresh
between frames carried the churn. A row the report has no move for is now blanked
instead: its labels hidden, its widgets kept. Not activatable, it loses the class the
stylesheet keys on to drop its cells' padding, so it has no height, and it sorts last.
The list looks as it did; only a lowered suggestion limit removes rows. `MIRAI_FRAMES`
counts rows built (`candidate-row`) and times the refresh (`candidates`).

What remains is GTK's or libadwaita's:

- **Dialog animations, 5–15 ms a frame.** libadwaita's floating sheet animates its scale
  from 0.8 (`adw-floating-sheet.c`), and GSK keys glyphs by scale, so every frame of an
  open or close re-rasterises the dialog's text. A text-heavy dialog costs more; the
  picker's CJK records most (up to 30 ms closing at 160 Hz).
- **The first CJK text in a process**, 20–110 ms: font fallback and loading. With an
  English UI, the picker's records are usually that text.
- **A Preferences page's first visit**, 30–40 ms of layout: wrapped labels are shaped
  again at their allocated width.
- **A report relays out the window**: label text changes queue a resize up to the
  toplevel, 1–2 ms a report; GSK then renders ~3 ms.

## 9. Keeping it

- Use `paint.rs` primitives; keep unavoidable paths' bounds and segment
  counts small, then measure them with `MIRAI_FRAMES=1`.
- For per-intersection text, defer only while `cell` changes, wait two repeats
  and repaint via a tick callback (§6).
- `MIRAI_SPIN` on a dense board should paint in single-digit milliseconds;
  `MIRAI_NO_LABEL_DEFER=1` checks the text cost while folding with live analysis.
- Measure drawing with the search **finished**: report-driven layouts during
  search mask changes to `snapshot()` (§7). The [testing guide](TESTING.md)
  has the GUI verification recipes.
