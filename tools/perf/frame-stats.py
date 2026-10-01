#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Aggregate the stderr of a `MIRAI_FRAMES=1` run into per-action frame statistics.

    MIRAI_FRAMES=1 MIRAI_HARNESS="wait:2000,action:win.toggle-sidebar,wait:700,quit" \
        ./target/debug/mirai game.sgf 2>frames.log
    tools/perf/frame-stats.py frames.log

One block is printed per harness step, because the interesting frames are the ones that
follow an action. The refresh period is the shortest frame interval the run produced at
least ten times: an animating window ticks at the output's rate, while an idle one may fall
back to a 60 Hz timer even on a 160 Hz output, so the median would misjudge a fast display.
Columns:

    n            frames the clock produced in that block
    over         frames whose work exceeded one refresh period, so missed their deadline
                 whatever the compositor did
    dt           median / worst frame interval, in ms; a long one with little work behind
                 it is time spent between frames — see `action:` below
    work         worst time a frame spent in GTK's phases, in ms
    paint        median / worst `paint` phase of the frames that painted at all, in ms
    layout       worst `layout` phase, in ms — measure and allocate for the whole window
    widgets      our own snapshot() timers, summed per label; `action:<name>` is the
                 synchronous cost of activating that window action, which runs between
                 frames — a large one shows up only as a long `dt`

`paint` covers GTK snapshotting the widget tree *and* GSK rasterising the render node, so a
large paint with tiny `widgets` numbers means the cost is in GSK, not in our code. Other
lines — `GDK_DEBUG=frames` output, logging — are ignored, so one run can carry both.
"""

import statistics
import sys
from pathlib import Path

PAINTED = 0.3  # ms; below this the frame produced no real render


def blocks(lines):
    # The harness logs an action step once it has run, so the action's own timer line comes
    # just before the step that caused it: carry it into that step's block.
    label, cur, carried = "startup", [], []
    for line in lines:
        line = line.strip()
        if line.startswith("harness:"):
            yield label, cur
            label, cur, carried = line[len("harness: ") :], carried, []
        elif line.startswith("action:"):
            carried.append(line)
        else:
            cur.append(line)
    yield label, cur + carried


def stat(values):
    if not values:
        return "—"
    return f"{statistics.median(values):6.1f} /{max(values):7.1f}"


def main(path):
    rows, all_dts = [], []
    for label, body in blocks(Path(path).read_text().splitlines()):
        dts, works, paints, layouts, widgets = [], [], [], [], {}
        for line in body:
            if line.startswith("frame-dt"):
                dts.append(float(line.split()[1]))
            elif line.startswith("frame-phases"):
                fields = dict(f.split("=", 1) for f in line.split()[1:])
                works.append(float(fields.get("total", 0)))
                paint = float(fields.get("paint", 0))
                if paint > PAINTED:
                    paints.append(paint)
                layouts.append(float(fields.get("layout", 0)))
            elif line and line[0].isalpha() and " " in line:
                name, _, value = line.partition(" ")
                try:
                    ms = float(value)
                except ValueError:
                    continue
                widgets.setdefault(name, []).append(ms)
        if not dts:
            continue
        all_dts.extend(dts)
        rows.append((label, dts, works, paints, layouts, widgets))
    if not rows:
        sys.exit(f"{path}: no frame-dt lines; was MIRAI_FRAMES=1 set on a debug build?")

    period = refresh_period(all_dts)
    print(f"refresh period {period:.2f} ms ({1000 / period:.0f} Hz)")
    print(
        f"{'step':34s} {'n':>4s} {'over':>4s} {'dt med/max':>16s} {'work max':>8s} "
        f"{'paint med/max':>16s} {'layout max':>10s}  widgets"
    )
    for label, dts, works, paints, layouts, widgets in rows:
        over = sum(1 for w in works if w > period)
        detail = " ".join(
            f"{name}={statistics.median(v):.2f}×{len(v)}" for name, v in sorted(widgets.items())
        )
        print(
            f"{label[:34]:34s} {len(dts):4d} {over:4d} {stat(dts):>16s} "
            f"{max(works) if works else 0:8.1f} {stat(paints):>16s} "
            f"{max(layouts) if layouts else 0:10.2f}  {detail}"
        )


def refresh_period(dts):
    counts = {}
    for dt in dts:
        counts[round(dt * 4) / 4] = counts.get(round(dt * 4) / 4, 0) + 1
    frequent = [dt for dt, n in counts.items() if n >= 10 and dt > 1]
    return min(frequent) if frequent else statistics.median(dts)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
