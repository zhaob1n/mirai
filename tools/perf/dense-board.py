#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Write an SGF whose root places ~285 setup stones, as a rendering stress case.

    tools/perf/dense-board.py /tmp/dense.sgf

A late-game position is the worst case for the board widget — it is the one a real review
spends most of its time in — and generating it beats shipping a game record just to measure
with. Every fourth row is left empty so no group is captured on load.
"""

import sys


def main(path):
    black, white = [], []
    for y in range(19):
        if y % 4 == 3:
            continue
        for x in range(19):
            point = chr(97 + x) + chr(97 + y)
            (black if (x // 2 + y // 4) % 2 == 0 else white).append(point)
    sgf = (
        "(;FF[4]GM[1]SZ[19]KM[7.5]RU[Chinese]"
        + "AB" + "".join(f"[{p}]" for p in black)
        + "AW" + "".join(f"[{p}]" for p in white)
        + ")"
    )
    with open(path, "w", encoding="utf-8") as out:
        out.write(sgf)
    print(f"{path}: {len(black)} black + {len(white)} white setup stones")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
