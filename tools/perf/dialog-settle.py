#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Record a dialog opening off the screen and print how its text settles.

    tools/perf/dialog-settle.py DP-2
    tools/perf/dialog-settle.py DP-2 --runs 2 --compare live:MIRAI_NO_SHEET_TEXTURE=1 \\
        --compare texture:

A frame-time log cannot show a glitch that costs no time. The harness opens a dialog over
an empty board and quits, while gpu-screen-recorder captures the output the window is on.
For the open, the last stretch of motion before the window goes away, the script prints
the mean difference in grey levels between each of the animation's last frames and the
dialog at rest, over the middle half of what the open changed, which for a centred dialog
is its content. `last` is that difference on the last frame still off rest by more than
the recording's noise: what the user sees change as the animation ends. Drawn live,
Preferences ends on 0.4-0.5, the spring's own last step; drawn from a texture resampled
to the end, on 0.9, its text sharpening there; with the texture drawn at scale 1 in the
spring's tail, on under 0.2 (RENDERING.md §8).

Needs gpu-screen-recorder able to capture the output (on Wayland, its KMS capture), ffmpeg
and numpy. Keep the output otherwise still: anything else that moves on it is motion too;
the pointer is left out. The window opens on the focused output, and MIRAI_WRAP prefixes
the command as for ui-survey.sh, to put it on the recorded one. Configurations given with
--compare run interleaved, each with its environment added. The binary is ui-survey.sh's;
MIRAI_BIN overrides it.
"""

import argparse
import os
import shlex
import signal
import subprocess
import tempfile
import time

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
# Frames without motion that end a stretch of it.
STILL = 30
# Grey levels a quarter-resolution pixel must change by for its frame to count as moving.
MOVED = 8
# Mean grey levels off rest beyond the recording's noise; a texture laid on the pixel grid
# differs from live drawing by less.
NOISE = 0.15


def binary():
    if "MIRAI_BIN" in os.environ:
        return os.environ["MIRAI_BIN"]
    env = dict(os.environ, CARGO_TARGET_DIR="target/perf",
               CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS="true")
    subprocess.run(["cargo", "build", "--release", "-p", "mirai", "--quiet"],
                   cwd=ROOT, env=env, check=True)
    return os.path.join(ROOT, "target/perf/release/mirai")


def record(args, extra, video):
    """Opens the dialog once with `extra` in the environment, recording to `video`."""
    with tempfile.TemporaryDirectory() as scratch:
        os.makedirs(f"{scratch}/config/mirai")
        os.makedirs(f"{scratch}/data/mirai")
        with open(f"{scratch}/config/mirai/config.toml", "w") as config:
            config.write("engine_profile = []\n")
        env = dict(os.environ, LANGUAGE="en", RUST_LOG="warn",
                   XDG_CONFIG_HOME=f"{scratch}/config", XDG_DATA_HOME=f"{scratch}/data",
                   MIRAI_HARNESS=f"wait:{args.wait},action:{args.action},wait:2000,quit",
                   **extra)
        recorder = subprocess.Popen(
            ["gpu-screen-recorder", "-w", args.output, "-f", str(args.fps), "-q", "ultra",
             "-k", "hevc", "-cursor", "no", "-o", video],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(1)
            wrap = shlex.split(os.environ.get("MIRAI_WRAP", ""))
            run = subprocess.run(wrap + [args.bin], env=env, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.PIPE, text=True)
            if run.returncode:
                raise SystemExit(f"mirai exited with {run.returncode}:\n{run.stderr}")
            time.sleep(0.5)
        finally:
            recorder.send_signal(signal.SIGINT)
            recorder.wait()


def size(video):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v", "-show_entries",
         "stream=width,height", "-of", "csv=p=0", video],
        capture_output=True, text=True, check=True).stdout
    return tuple(map(int, out.split(",")))


def frames(video, filters, width, height):
    """Every frame of `video` through ffmpeg `filters`, grey, `width` by `height`."""
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", video, "-fps_mode", "passthrough", "-vf",
         f"{filters},format=gray", "-f", "rawvideo", "-"],
        capture_output=True, check=True).stdout
    return np.frombuffer(out, np.uint8).reshape(-1, height, width)


def diff(a, b):
    return np.abs(a.astype(np.int16) - b.astype(np.int16))


def settle(video):
    """Mean distance to the dialog at rest over the open's last frames, ending on the
    first frame at rest."""
    width, height = size(video)
    small = frames(video, "scale=iw/4:ih/4", width // 4, height // 4)
    stretches = []
    for i in range(1, len(small)):
        if (diff(small[i], small[i - 1]) > MOVED).sum() <= 4:
            continue
        if stretches and i - stretches[-1][1] <= STILL:
            stretches[-1][1] = i
        else:
            stretches.append([i, i])
    if len(stretches) < 2:
        raise SystemExit(f"{video}: expected the open and the window going away, "
                         f"saw {len(stretches)} stretches of motion")
    start, end = stretches[-2]
    after = min(end + STILL // 2, len(small) - 1)
    ys, xs = np.nonzero(diff(small[after], small[max(start - 2, 0)]) > MOVED)
    x0, x1, y0, y1 = (4 * int(v) for v in (xs.min(), xs.max(), ys.min(), ys.max()))
    w, h = (x1 - x0) // 2, (y1 - y0) // 2
    content = frames(video, f"crop={w}:{h}:{x0 + w // 2}:{y0 + h // 2}", w, h)
    off = [float(diff(content[i], content[after]).mean()) for i in range(start, after)]
    last = max(i for i, d in enumerate(off) if d > NOISE)
    return off[max(last - 5, 0) : last + 2]


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("output", help="the output to record, as gpu-screen-recorder names it")
    parser.add_argument("--action", default="win.preferences")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--fps", type=int, default=160, help="the output's refresh rate")
    parser.add_argument("--wait", type=int, default=2500, help="ms before the action")
    parser.add_argument("--compare", action="append", metavar="NAME:VAR=VALUE,...",
                        help="a configuration to run, with its environment")
    args = parser.parse_args()
    configs = []
    for spec in args.compare or ["default:"]:
        name, _, assignments = spec.partition(":")
        configs.append((name, dict(a.split("=", 1) for a in assignments.split(",") if a)))
    args.bin = binary()
    print(f"{'':12s} {'distance to rest, last frames of the open':>41s} {'last':>5s}")
    with tempfile.TemporaryDirectory() as videos:
        for run in range(args.runs):
            for name, extra in configs:
                video = f"{videos}/{name}-{run}.mp4"
                record(args, extra, video)
                tail = settle(video)
                print(f"{name[:12]:12s} {' '.join(f'{d:5.2f}' for d in tail):>41s} "
                      f"{tail[-2]:5.2f}")
                os.remove(video)


if __name__ == "__main__":
    main()
