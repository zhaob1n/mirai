#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Write an SGF of ~180 real moves, as a stress case for the board's move numbers.

    tools/perf/numbered-game.py /tmp/numbered.sgf

`dense-board.py` places *setup* stones, which carry no move numbers, so it cannot exercise
the text passes at all. This plays on every other row instead: neighbours within a row share
a chain, the rows between them stay empty, and nothing is ever captured — so the position
survives a load and every stone on the board has a number over it.
"""

import sys


def main(path):
    moves = []
    colour = "B"
    for y in range(0, 19, 2):
        for x in range(19):
            moves.append(f";{colour}[{chr(97 + x)}{chr(97 + y)}]")
            colour = "W" if colour == "B" else "B"
    sgf = "(;FF[4]GM[1]SZ[19]KM[7.5]RU[Chinese]" + "".join(moves) + ")"
    with open(path, "w", encoding="utf-8") as out:
        out.write(sgf)
    print(f"{path}: {len(moves)} moves")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
