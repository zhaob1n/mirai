#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Aggregate the stderr of a `MIRAI_FRAMES=1` run into per-action frame statistics.

    MIRAI_FRAMES=1 MIRAI_HARNESS="wait:2000,action:win.toggle-sidebar,wait:700,quit" \
        ./target/debug/mirai game.sgf 2>frames.log
    tools/perf/frame-stats.py frames.log

One block is printed per harness step, because the interesting frames are the ones that
follow an action. Columns:

    n            frames the clock produced in that block
    slow         frames whose interval exceeded 20 ms (a missed 60 Hz deadline)
    dt           median / worst frame interval, in ms
    paint        median / worst `paint` phase of the frames that painted at all, in ms
    layout       worst `layout` phase, in ms — measure and allocate for the whole window
    widgets      our own snapshot() timers, summed per label

`paint` covers GTK snapshotting the widget tree *and* GSK rasterising the render node, so a
large paint with tiny `widgets` numbers means the cost is in GSK, not in our code.
"""

import statistics
import sys
from pathlib import Path

PAINTED = 0.3  # ms; below this the frame produced no real render


def blocks(lines):
    label, cur = "startup", []
    for line in lines:
        line = line.strip()
        if line.startswith("harness:"):
            yield label, cur
            label, cur = line[len("harness: ") :], []
        else:
            cur.append(line)
    yield label, cur


def stat(values):
    if not values:
        return "—"
    return f"{statistics.median(values):6.1f} /{max(values):7.1f}"


def main(path):
    rows = []
    for label, body in blocks(Path(path).read_text().splitlines()):
        dts, paints, layouts, widgets = [], [], [], {}
        for line in body:
            if line.startswith("frame-dt"):
                dts.append(float(line.split()[1]))
            elif line.startswith("frame-phases"):
                fields = dict(f.split("=", 1) for f in line.split()[1:])
                paint = float(fields.get("paint", 0))
                if paint > PAINTED:
                    paints.append(paint)
                layouts.append(float(fields.get("layout", 0)))
            elif line and line[0].isalpha() and " " in line:
                name, _, value = line.partition(" ")
                try:
                    widgets.setdefault(name, []).append(float(value))
                except ValueError:
                    continue
        if not dts:
            continue
        rows.append((label, dts, paints, layouts, widgets))

    print(
        f"{'step':34s} {'n':>4s} {'slow':>4s} "
        f"{'dt med/max':>16s} {'paint med/max':>16s} {'layout max':>10s}  widgets"
    )
    for label, dts, paints, layouts, widgets in rows:
        slow = sum(1 for d in dts if d > 20)
        detail = " ".join(
            f"{name}={statistics.median(v):.2f}×{len(v)}" for name, v in sorted(widgets.items())
        )
        print(
            f"{label[:34]:34s} {len(dts):4d} {slow:4d} "
            f"{stat(dts):>16s} {stat(paints):>16s} "
            f"{max(layouts) if layouts else 0:10.2f}  {detail}"
        )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
