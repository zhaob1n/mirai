#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Run a debug mirai under MIRAI_HARNESS, isolated from the developer's config and data
# (docs/dev/TESTING.md §5), optionally at an exact logical window size.
#
#   tools/ui/sized-shot.sh WIDTH HEIGHT "wait:3000,shot:/tmp/x.png,quit" [mirai args…]
#
# niri tiles new windows and ignores the default size unless a window rule floats mirai. With
# such a rule, 0 0 shows exactly the size mirai asked for. A nonzero size floats the window if
# no rule already has, then sets that size from outside, to check the layout at a size mirai
# did not choose. Copies ~/.config/mirai/config.toml when present, so the engine profile
# survives; set MIRAI_NO_CONFIG=1 for the empty case.
set -eu
[ $# -ge 3 ] || { sed -n '6,14p' "$0"; exit 2; }
width=$1 height=$2 script=$3
shift 3

root=$(cd "$(dirname "$0")/../.." && pwd)
bin=$root/target/debug/mirai
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/config/mirai" "$scratch/data"
if [ -z "${MIRAI_NO_CONFIG:-}" ] && [ -f "$HOME/.config/mirai/config.toml" ]; then
    cp "$HOME/.config/mirai/config.toml" "$scratch/config/mirai/"
fi

XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
    MIRAI_HARNESS="$script" "$bin" "$@" &
pid=$!

if [ "$width" -gt 0 ] && command -v niri >/dev/null; then
    window=
    for _ in $(seq 50); do
        window=$(niri msg --json windows | jq -c ".[] | select(.pid == $pid)" | sed -n 1p)
        [ -n "$window" ] && break
        sleep 0.1
    done
    if [ -n "$window" ]; then
        id=$(printf '%s' "$window" | jq -r .id)
        [ "$(printf '%s' "$window" | jq -r .is_floating)" = true ] \
            || niri msg action toggle-window-floating --id "$id"
        niri msg action set-window-width --id "$id" "$width"
        niri msg action set-window-height --id "$id" "$height"
    else
        echo "sized-shot: no niri window for pid $pid; size left to the compositor" >&2
    fi
fi
wait "$pid"
