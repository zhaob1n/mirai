#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Cut a perf profile of the GTK thread frame by frame, against a `MIRAI_FRAMES=1` log.

    MIRAI_FRAMES=1 MIRAI_HARNESS="…" perf record -k CLOCK_MONOTONIC --call-graph fp \\
        -e cycles/period=2000000/ -e task-clock/period=200000/ -o run.perf -- \\
        target/perf/release/mirai game.sgf 2>run.log
    tools/perf/frame-profile.py run.log run.perf            # every slow frame
    tools/perf/frame-profile.py run.log run.perf 'page "Play"' 1   # what one frame ran

Each `frame-phases` line carries `at=`, when the frame began on CLOCK_MONOTONIC, the clock
`-k CLOCK_MONOTONIC` stamps samples with. For every frame over --min ms, the first form
prints the frame's wall time beside the GTK thread's on-CPU time (task-clock samples) and
cycles, and so the clock it ran at: a slow frame on a core held at its lowest frequency
reads as a few Mcycles at under 1 GHz, not as more work. Given a harness step and n, the
second form prints the nth slow frame after that step by inclusive time per function.
Each sample counts for the period perf names its event with, so any `period=` will do.
With `kernel.perf_event_paranoid` at 2, the default, perf samples user space only: on-CPU
time then leaves out the kernel, and a frame whose wall time exceeds it waited or ran
there.

Use frame pointers (`--call-graph fp`): GTK's widget recursion runs deeper than a DWARF
stack copy reaches. Distribution libraries have them; a GTK built with symbols for
attribution needs `-Dc_args=-fno-omit-frame-pointer`, and LD_LIBRARY_PATH to load it.
"""

import argparse
import collections
import re
import subprocess

# The recipe's periods, for an event whose name does not carry its own.
CYCLES_PERIOD = 2_000_000
TASK_CLOCK_PERIOD_NS = 200_000
THREAD = "mirai"


def frames(log, min_ms):
    """(step, start, end, phases) for every frame over min_ms."""
    step = "startup"
    for line in open(log, encoding="utf-8"):
        line = line.strip()
        if line.startswith("harness:"):
            step = line[len("harness: "):]
        elif line.startswith("frame-phases") and " at=" in line:
            fields = dict(f.split("=", 1) for f in line.split()[1:])
            total = float(fields["total"])
            if total >= min_ms:
                start = float(fields["at"])
                yield step, start, start + total / 1000, line


def samples(perf, start=None, end=None, stacks=False):
    """(event, [functions, leaf first]) for the GTK thread's samples in the window."""
    fields = "comm,tid,time,event" + (",ip,sym" if stacks else "")
    args = ["perf", "script", "-i", perf, "--no-inline", "-F", fields]
    if start is not None:
        args += ["--time", f"{start:.6f},{end:.6f}"]
    out = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    cur = None
    for line in out.splitlines():
        if not line.strip():
            continue
        if not line[0].isspace():
            # A sample's header: `comm tid time: event:`, the comm possibly with spaces, and
            # with the leaf frame on the same line when the stack did not unwind.
            if cur is not None:
                yield cur
            head = re.match(r"(.*?)\s+\d+\s+[\d.]+:\s+(\S+?):?(?:\s+[0-9a-f]+\s+(.*))?\s*$", line)
            cur = (head[2], [head[3]] if head[3] else []) if head and head[1] == THREAD else None
        elif cur is not None and stacks:
            frame = re.match(r"\s*[0-9a-f]+\s+(.*?)(\+0x[0-9a-f]+)?\s*$", line)
            if frame:
                cur[1].append(frame[1])
    if cur is not None:
        yield cur


def period(event, default):
    """The sampling period perf names the event with (`task-clock/period=200000/u`)."""
    found = re.search(r"period=(\d+)", event)
    return int(found[1]) if found else default


def on_cpu_ms(event):
    return period(event, TASK_CLOCK_PERIOD_NS) / 1e6 if "task-clock" in event else 0.0


def clocks(args):
    events = [(e, t) for e, t in timed(args.perf)]
    print(f"{'step':34s} {'wall':>6s} {'on-cpu':>7s} {'Mcycles':>8s} {'GHz':>5s}  phases")
    for step, start, end, phases in frames(args.log, args.min):
        window = [e for e, t in events if start <= t <= end]
        cpu = sum(on_cpu_ms(e) for e in window)
        cycles = sum(period(e, CYCLES_PERIOD) for e in window if "cycles" in e) / 1e6
        ghz = cycles / cpu if cpu else 0.0
        detail = phases.split(" ", 2)[2].rsplit(" at=", 1)[0]
        print(f"{step[:34]:34s} {(end - start) * 1000:6.1f} {cpu:7.1f} {cycles:8.0f} {ghz:5.2f}  {detail}")


def timed(perf):
    out = subprocess.run(["perf", "script", "-i", perf, "-F", "comm,tid,time,event"],
                         capture_output=True, text=True, check=True).stdout
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[0] == THREAD:
            yield parts[3], float(parts[2].rstrip(":"))


def functions(args):
    picked = [f for f in frames(args.log, args.min) if args.step in f[0]]
    if len(picked) < args.nth:
        raise SystemExit(f"only {len(picked)} frames over {args.min} ms after {args.step!r}")
    step, start, end, phases = picked[args.nth - 1]
    print(phases)
    inclusive = collections.Counter()
    cpu = 0.0
    for event, stack in samples(args.perf, start, end, stacks=True):
        weight = on_cpu_ms(event)
        if not weight:
            continue
        cpu += weight
        for fn in set(stack):
            if not fn.startswith("[unknown]"):
                inclusive[fn] += weight
    print(f"on-CPU {cpu:.1f} ms of {(end - start) * 1000:.1f} ms")
    for fn, ms in inclusive.most_common(args.top):
        print(f"{ms:7.1f} ms  {fn}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("log")
    parser.add_argument("perf")
    parser.add_argument("step", nargs="?", help="harness step whose frames to attribute")
    parser.add_argument("nth", nargs="?", type=int, default=1)
    parser.add_argument("--min", type=float, default=6.0, help="slow frame, in ms")
    parser.add_argument("--top", type=int, default=40)
    args = parser.parse_args()
    functions(args) if args.step else clocks(args)


if __name__ == "__main__":
    main()
