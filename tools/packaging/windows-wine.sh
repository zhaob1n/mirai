#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Runs a staged Windows build (tools/packaging/windows-cross.sh) under Wine, in a prefix of
# its own under target/windows/wine so the developer's prefix is never touched.
#
#   tools/packaging/windows-wine.sh [STAGE_DIR] [EXE] [ARGS...]
#
# STAGE_DIR defaults to the newest target/windows/dist/*-debug, EXE to mirai. The debug
# build carries the harness; Wine sees Unix paths through drive Z:, and passes the
# environment through:
#
#   MIRAI_HARNESS='wait:6000,shot:Z:\tmp\wine.png,quit' tools/packaging/windows-wine.sh
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
stage=${1:-$(ls -dt "$repo"/target/windows/dist/*-debug 2>/dev/null | head -n1 || true)}
[ -n "$stage" ] && [ -d "$stage/bin" ] || { echo "no staged build: run tools/packaging/windows-cross.sh debug" >&2; exit 1; }
exe=${2:-mirai}
shift $(($# > 2 ? 2 : $#))

export WINEPREFIX=$repo/target/windows/wine
# No Mono or Gecko install prompt: mirai needs neither.
export WINEDLLOVERRIDES=${WINEDLLOVERRIDES:-mscoree,mshtml=}
export WINEDEBUG=${WINEDEBUG:--all}
# Wine hands the host environment through; on Windows there is no session bus address, and
# GLib starts its own with gdbus.exe instead of reaching for the host's.
unset DBUS_SESSION_BUS_ADDRESS
[ -d "$WINEPREFIX" ] || wineboot -i
exec wine "$stage/bin/$exe.exe" "$@"
