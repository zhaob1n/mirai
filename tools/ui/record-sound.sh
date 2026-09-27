#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Run a debug mirai under MIRAI_HARNESS with its audio routed to a private null sink, record
# that sink, and print every sound onset: the proof that a step made, or did not make, a sound.
#
#   tools/ui/record-sound.sh "wait:3000,action:win.next,wait:800,quit" game.sgf
#   tools/ui/record-sound.sh "wait:3000,board:primary:D4,wait:800,quit"
#
# Prints one `<seconds> peak=<level>` line per onset; the file stays at /tmp/mirai-sound.wav.
# A capture is a strike followed ~0.2 s later by quieter drops. Nothing reaches the speakers,
# and other audio playing on the machine does not leak into the recording. Config and data are
# isolated with no engine profile.
set -eu
[ $# -ge 1 ] || { sed -n '5,14p' "$0"; exit 2; }
script=$1
shift

root=$(cd "$(dirname "$0")/../.." && pwd)
out=/tmp/mirai-sound.wav
scratch=$(mktemp -d)
module=$(pactl load-module module-null-sink sink_name=mirai_probe \
    sink_properties=device.description=mirai_probe)
trap 'pactl unload-module "$module"; rm -rf "$scratch"' EXIT
mkdir -p "$scratch/config/mirai" "$scratch/data"
echo 'engine_profile = []' >"$scratch/config/mirai/config.toml"

parecord -d mirai_probe.monitor --channels=1 --rate=44100 --file-format=wav \
    --latency-msec=20 "$out" &
recorder=$!
sleep 0.5
PULSE_SINK=mirai_probe XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
    MIRAI_HARNESS="$script" "$root/target/debug/mirai" "$@"
# parecord buffers; give the tail time to land before stopping it.
sleep 1.5
kill -INT "$recorder"
wait "$recorder" || true

python3 - "$out" <<'EOF'
import sys, wave
import numpy as np

w = wave.open(sys.argv[1])
rate = w.getframerate()
a = np.abs(np.frombuffer(w.readframes(w.getnframes()), dtype="<i2") / 32767)
last = None
for i in np.nonzero(a > 0.02)[0]:
    if last is None or i - last > rate * 0.015:
        print(f"{i / rate:.3f}s peak={a[i:i + rate // 50].max():.2f}")
    last = i
EOF
