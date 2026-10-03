#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Drives each UI surface through the harness with MIRAI_FRAMES=1 and prints per-step frame
# statistics (tools/perf/frame-stats.py), so a dialog that drops frames shows up as a row
# rather than as a feeling.
#
#     tools/perf/ui-survey.sh [--engine] [scenario...]
#
# Scenarios: dialogs fox main analysis (default: dialogs fox main). `analysis` needs
# --engine, which copies your config.toml (engine profiles) into the isolated run with
# Analyse on Open turned off; without it the run has no engine.
#
# The binary is an optimised build that keeps debug assertions, because the harness and the
# frame probes are compiled only with them: target/perf/release/mirai, built on first use.
# Override with MIRAI_BIN. Logs go to ${MIRAI_SURVEY_OUT:-/tmp/mirai-survey}/<scenario>.log.
# MIRAI_WRAP prefixes the command — `MIRAI_WRAP="perf record -k CLOCK_MONOTONIC
# --call-graph fp -e cycles/period=2000000/ -e task-clock/period=200000/ -o /tmp/p.data --"`
# profiles the same run, and tools/perf/frame-profile.py reads it frame by frame.
# Every scenario runs with LANGUAGE=en, as its steps match on-screen labels; for `fox` that
# also makes the records the first CJK text the process draws, as for anyone with a
# Latin-script interface.
#
# The window opens on the focused output, and its refresh rate is the budget: the stats
# print the period they judged against. Keep the window on screen — niri throttles frame
# callbacks for windows no output shows.

set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
bin=${MIRAI_BIN:-$root/target/perf/release/mirai}
out=${MIRAI_SURVEY_OUT:-/tmp/mirai-survey}
sgf=$root/crates/mirai-core/tests/data/katago-selfplay.sgf

engine=0
scenarios=()
for arg in "$@"; do
    case $arg in
        --engine) engine=1 ;;
        dialogs | fox | main | analysis) scenarios+=("$arg") ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done
[[ ${#scenarios[@]} -gt 0 ]] || scenarios=(dialogs fox main)

if [[ -z ${MIRAI_BIN:-} ]]; then
    (cd "$root" && CARGO_TARGET_DIR=target/perf CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
        cargo build --release -p mirai --quiet)
fi

repeat() { # <steps> <times>
    local steps=""
    for _ in $(seq "$2"); do
        steps+=$1
    done
    printf '%s' "$steps"
}

open_close() { # <action> <times>: present a dialog and close it again
    repeat "action:$1,wait:1000,close-dialog,wait:800," "$2"
}

declare -A script
script[dialogs]="wait:3000,$(open_close win.preferences 2)\
action:win.preferences,wait:1000,page:Analysis,wait:600,page:Play,wait:600,page:General,wait:600,page:Engines,wait:600,close-dialog,wait:800,\
$(open_close win.new-game 2)$(open_close win.about 2)quit"
script[fox]="wait:3000,$(open_close win.download-fox 2)action:win.download-fox,wait:1000,\
$(repeat 'press:Next Page,wait:500,' 3)press:Previous Page,wait:500,\
fill:Exact Fox nickname=,wait:700,fill:Exact Fox nickname=申,wait:700,press:申真谞,wait:800,\
close-dialog,wait:800,quit"
script[main]="wait:3000,\
action:win.toggle-sidebar,wait:700,action:win.toggle-sidebar,wait:700,\
action:win.toggle-graph,wait:700,action:win.toggle-graph,wait:700,\
stack:Moves,wait:700,stack:Comment,wait:700,stack:Analysis,wait:700,\
action:win.next10,wait:400,action:win.next10,wait:400,action:win.next10,wait:400,\
action:win.last,wait:400,action:win.first,wait:400,\
action:win.toggle-coords,wait:500,action:win.toggle-coords,wait:500,\
action:win.toggle-move-numbers,wait:500,action:win.toggle-move-numbers,wait:500,\
action:win.toggle-editor,wait:700,action:win.toggle-editor,wait:700,quit"
script[analysis]="wait:8000,action:win.next10,wait:300,action:win.toggle-analysis,wait:4000,\
$(repeat 'action:win.next,wait:500,' 8)action:win.next10,wait:1500,\
$(repeat 'action:win.prev,wait:500,' 4)action:win.toggle-analysis,wait:1000,quit"

fox_cache() { # <path>: saved searches in the shape Fox answers with, CJK names and titles
    python3 - "$1" <<'EOF'
import json, sys
names = ["柯洁", "申真谞", "党毅飞", "朴廷桓", "芈昱廷", "一力辽", "Shin Jinseo", "辜梓豪"]
def rows(count, seed):
    return [{
        "chessid": str(1785337045010001403 - i - seed * 1000), "blacknick": names[(i + seed) % 8],
        "blackenname": "", "whitenick": names[(i * 3 + 1 + seed) % 8], "whiteenname": "",
        "blackdan": 108, "whitedan": 100 + i % 9, "blackocc": 2, "whiteocc": 2,
        "winner": 1 + i % 2, "point": -1 if i % 3 else 75, "movenum": 120 + i % 180,
        "boardsize": 19, "starttime": f"2026-07-{1 + i % 28:02d} 22:{i % 60:02d}:25",
        "title": "第6届中国围棋王中王争霸赛总决赛" if i % 4 == 0 else "",
    } for i in range(count)]
searches = [
    {"query": "柯洁", "account": "柯洁", "saved": 1785337045, "rows": rows(200, 0)},
    {"query": "申真谞", "account": "申真谞", "saved": 1785250645, "rows": rows(37, 1)},
    {"query": "6757425", "account": "UID 6757425", "saved": 1785164245, "rows": rows(8, 2)},
]
# A full history: the recent list at its longest.
searches += [{"query": f"{names[i % 8]}{i}", "account": f"{names[i % 8]}{i}",
              "saved": 1785000000 - i * 3600, "rows": rows(60, i)} for i in range(3, 20)]
json.dump({"searches": searches}, open(sys.argv[1], "w"), ensure_ascii=False)
EOF
}

mkdir -p "$out"
for name in "${scenarios[@]}"; do
    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT
    mkdir -p "$scratch/config/mirai" "$scratch/data/mirai"
    if [[ $engine == 1 ]]; then
        sed 's/^auto_analyse_on_open *=.*/auto_analyse_on_open = false/' \
            "${XDG_CONFIG_HOME:-$HOME/.config}/mirai/config.toml" > "$scratch/config/mirai/config.toml"
    else
        [[ $name != analysis ]] || { echo "analysis needs --engine" >&2; exit 2; }
        printf 'engine_profile = []\n' > "$scratch/config/mirai/config.toml"
    fi
    [[ $name != fox ]] || fox_cache "$scratch/data/mirai/fox-searches.json"
    log=$out/$name.log
    LANGUAGE=en XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" \
        MIRAI_FRAMES=1 RUST_LOG=warn MIRAI_HARNESS="${script[$name]}" \
        ${MIRAI_WRAP:-} "$bin" "$sgf" 2>"$log" || echo "($name: a harness step failed; see $log)" >&2
    rm -rf "$scratch"
    trap - EXIT
    echo "== $name"
    python3 "$root/tools/perf/frame-stats.py" "$log"
done
