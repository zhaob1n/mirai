#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Run a debug mirai under MIRAI_HARNESS and type real keys into it, for what the harness
# cannot reach: which widget a key goes to (docs/dev/TESTING.md §5).
#
#   tools/ui/type-keys.sh "wait:2500,focus:comment,wait:6000,shot:/tmp/k.png,quit" p "-k space"
#
# Each key argument is one wtype invocation. Needs niri and wtype (a Wayland virtual keyboard;
# WTYPE overrides the path). The window is focused through niri, each key is sent only while
# niri still reports it focused, and focus returns to the previous window afterwards — the
# keys go to whatever is focused, so do not touch the keyboard while this runs. Start the
# typing within the script's leading waits: typing begins about four seconds after launch.
set -eu
[ $# -ge 1 ] || { sed -n '5,14p' "$0"; exit 2; }
script=$1
shift
wtype=${WTYPE:-wtype}
root=$(cd "$(dirname "$0")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/config" "$scratch/data"

focused() { niri msg --json focused-window | jq -r '.id // empty'; }
previous=$(focused)

XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
    MIRAI_HARNESS="$script" "$root/target/debug/mirai" >"$scratch/log" 2>&1 &
pid=$!

window=
for _ in $(seq 50); do
    window=$(niri msg --json windows | jq -r ".[] | select(.pid == $pid) | .id" | sed -n 1p)
    [ -n "$window" ] && break
    sleep 0.1
done
sleep 3
if [ -n "$window" ]; then
    niri msg action focus-window --id "$window"
    sleep 0.5
    for key in "$@"; do
        if [ "$(focused)" != "$window" ]; then
            echo "type-keys: focus left the harness window; stopped before $key" >&2
            break
        fi
        # shellcheck disable=SC2086 # one argument holds a whole wtype command line
        "$wtype" $key
        sleep 0.3
    done
else
    echo "type-keys: the harness window never appeared" >&2
fi
[ -n "$previous" ] && niri msg action focus-window --id "$previous"
wait "$pid" || true
grep '^harness:' "$scratch/log"
