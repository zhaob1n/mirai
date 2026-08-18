<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Rendering performance

Why mirai's custom widgets draw with quads and not with paths, and the measurements that
settled it.
[architecture](ARCHITECTURE.md) · [testing](TESTING.md) ·
[retrospective](../archive/RETROSPECTIVE.md) · [AGENTS.md](../../AGENTS.md).

**The rule, if you read nothing else:** in `snapshot()`, never hand GSK a `fill` or `stroke`
node for a shape a colour node, a border node or a rounded clip can draw. Paths are for ink
that is genuinely curved or diagonal — the win-rate curve, the triangle and cross marks — and
only where the node's bounds stay small. The helpers in
[`crates/mirai/src/widgets/paint.rs`](../../crates/mirai/src/widgets/paint.rs) exist so this
costs nothing to obey.

---

## 1. The symptom

Folding the sidebar (<kbd>F9</kbd>) visibly stuttered. The animation is time-based and about
300 ms long, so it should be ~18 frames at 60 Hz; it was drawing 3–7.

The same cost was there all along in a much worse place: stepping through a *late-game*
position repainted the board in 90–120 ms, which is 8–11 fps for ordinary review work. The
sidebar animation only made it obvious, because it is the one interaction that needs many
consecutive frames.

## 2. How it was measured

`perf` is not installed on the development machine and external screen capture is black under
Wayland, so the application measures itself. Branch `perf-probe` carries the instrument:

- **`crates/mirai/src/render_probe.rs`** — `frame-dt` lines (the frame clock's own cadence,
  quantised to the 60 Hz refresh: `16.67` is a hit, `50.00` means two frames were missed) and
  `frame-phases` lines, which split each frame into GTK's phases: `update` (animations),
  `layout` (measure and allocate), `paint` (snapshot the widget tree, then GSK renders it).
- **Ablation flags** — `MIRAI_NO_BOARD`, `MIRAI_NO_GRAPH`, `MIRAI_NO_WOOD`, `MIRAI_NO_GRID`,
  `MIRAI_NO_STARS`, `MIRAI_NO_STONES`, `MIRAI_NO_SHADOW`, `MIRAI_COLLAPSED`, `MIRAI_SPIN`.
- **`tools/perf/frame-stats.py`** — aggregates the stderr into a per-action table.
- **`tools/perf/dense-board.py`** — writes the 285-stone position used as the worst case.
- **`MIRAI_DUMP_NODE`** — writes the window's render node for `gtk4-rendernode-tool`.

Runs use an isolated `XDG_CONFIG_HOME`/`XDG_DATA_HOME` and no engine, so nothing in the
numbers is KataGo's — and, it later turned out, nothing in them is analysis labels either
(§6). The window is whatever niri gives it, ~1486×1634; `niri msg action
set-window-width/-height` was used for the size sweep.

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

**Removing one drawing pass at a time finds the cost.** Sidebar animation, empty board, before
the fix:

| build | slow frames per animation | paint median |
|---|---|---|
| unchanged | 19 | 29.6 ms |
| `MIRAI_NO_WOOD` | 28 | 20.1 ms |
| `MIRAI_NO_GRID` | 31 | 17.5 ms |
| both | **0** | 4.9 ms |
| `MIRAI_NO_BOARD` (graph only) | 4 | 9.2 ms |
| `MIRAI_NO_BOARD` + `MIRAI_NO_GRAPH` | 0 | — (max 1.5 ms) |

Every expensive item is an `append_fill` or an `append_stroke`: the wood rounded-rect fill
(~10 ms), the grid and border strokes (~12 ms), the stone shadow path and one circle fill plus
one circle stroke per stone (the 90–120 ms of a full board). The rest of the window — header
bar, view switcher, `ColumnView`, text — never showed up at all.

**The last experiment names the mechanism.** `MIRAI_SPIN` redraws an *unchanging* scene every
frame:

| scene, before the fix | paint median | frame interval |
|---|---|---|
| empty board, same nodes re-rendered | **0.26 ms** | 16.7 ms |
| 285 stones, nodes rebuilt each snapshot | **58.0 ms** | 68.1 ms |

Re-rendering the *same* fill node is free, so GSK caches a path rasterisation and keys that
cache on the node. Rebuilding the node is what costs: the stones are built fresh in every
`snapshot()`, and the board's cached static layer is rebuilt whenever the allocation changes —
which is exactly what a sidebar animation does, frame after frame. `gtk4-rendernode-tool
benchmark` agrees from outside the process, and shows the same asymmetry, because it replays
one identical node: 13 ms per render for the old full board against 4 ms for the new one.

That is the root cause: **GSK rasterises a fill or stroke node when it first sees it, and mirai
handed it new ones every frame.**

## 4. The fix

`crates/mirai/src/widgets/paint.rs` — `fill_disc` (rounded clip + colour node), `stroke_disc`
and `stroke_rect` (border nodes), `hline`/`vline` (colour nodes), and `over` for pre-blending
translucent ink. Then:

| was | is now |
|---|---|
| wood: rounded-rect path fill | colour node in a rounded clip |
| grid: one stroked path, 38 segments across the board | one colour-node rectangle per line |
| board border: stroked rectangle path | border node |
| star points, stones, shadows, last-move dot, candidate blobs, label discs, tree nodes | `fill_disc` |
| stone rims, candidate ring, circle marks, territory outlines, tree outlines | `stroke_disc` / `stroke_rect` |
| move tree: one stroked path of right-angle elbows | one rectangle per run, one trunk per parent |
| graph: horizontal guides as a stroked path | `hline` per guide |

Two details that keep the pixels honest:

- Translucent ink drawn as overlapping rectangles blends twice. The grid ink is therefore
  pre-blended against the wood with `over()` and drawn opaque, and the move tree draws one
  trunk rectangle per parent instead of one per child.
- Stones are 0.96 cells across, so per-stone shadow discs never overlap and blend exactly like
  the single combined path they replaced.

Still paths, deliberately: the win-rate and score-lead curves, the dashed 50 % line, and the
triangle, square and cross marks. Curves are not quads, and the marks each cover one stone, so
the bounds GSK has to rasterise are a cell rather than a board.

## 5. After

Same machine, same scenes, same instrument:

| scene | before | after |
|---|---|---|
| sidebar fold, empty board | paint 29.6 ms, 6–8 frames missed per animation | paint 1.8–4.1 ms, **0 missed** |
| sidebar fold, 285 stones | paint 89–125 ms, 3 frames drawn per animation | paint 7.7 ms, **15 frames, 0 missed** |
| `MIRAI_SPIN`, 285 stones | paint 58.0 ms, clock at 68 ms | paint 3.5 ms, clock at 16.7 ms |
| stepping a real game record (`win.next`) | — | paint 0.9–2.6 ms |

Visual verification: the app screenshots itself (`shot:` steps) before and after, with the same
scripts, on an empty board with coordinates, the 285-stone position, a real record at move 10
with and without move numbers, and the move tree. Differences are confined to antialiasing on
the edges of stones and grid lines — at 8× magnification the two are indistinguishable, and
outside the board the only differing pixels are header text that depends on run timing.

## 6. Candidate labels

The probe above ran with no engine, so the quad rewrite never saw analysis numbers. Those
are pango glyphs whose size tracks `Layout::cell`. Folding the sidebar (when it is not an
overlay) still changes the board's **width** every frame. `cell` is
`min(width/units_x, height/units_y)`, so it follows the width only while the board is
width-limited; once height is tighter, `cell` freezes and only the horizontal origin
moves. Either way each snapshot is new. A new `cell` makes every label a brand-new GSK
text node — the same miss §3 named, just not a path. Stones-only stays smooth (quads); a
live report stutters the fold.

Blobs stay (`fill_disc`). Labels paint only on a snapshot whose whole allocation matches
the previous one, not on `cell` alone: even a recentre rebuilds the snapshot. The first
paint, and any redraw at a stable size (cursor, a new report), still has numbers. A
changed allocation queues one idle redraw; the next snapshot at that size brings the text
back. There is no timeout: libadwaita's split-view animation is not a fixed duration, and
`show-sidebar` flips when F9 is pressed, not when the pixels settle. The allocation *is*
the signal.

## 7. Keeping it

- `paint.rs` is the only place these primitives are defined; use them.
- If a new widget needs a path, keep the node's bounds small and its segment count low, then
  measure it with the probe branch before assuming it is fine.
- Pango on the board is sized to `cell`. Anything that reallocates every frame (sidebar
  fold, a live window resize) must not emit those glyphs until the allocation repeats.
  `BoardView` already does this; a new overlay that draws per-intersection text has to
  do the same.
- `MIRAI_SPIN` on a dense board is the cheapest regression check: it should stay a
  single-digit millisecond paint. There is deliberately no unit test — a headless test cannot
  see a frame, and asserting on node types would pin the implementation rather than the
  behaviour.
