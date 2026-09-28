#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Cross-builds the Windows release in a Fedora container (packaging/windows/Containerfile)
# from this working tree, uncommitted changes included.
#
#   tools/packaging/windows-cross.sh [package]    zip into target/windows/dist/
#   tools/packaging/windows-cross.sh debug        stage the debug build, harness included
#   tools/packaging/windows-cross.sh shell        a shell in the build container
#   tools/packaging/windows-cross.sh run CMD...   any command in the build container
#
# The image is rebuilt from the Containerfile first; unchanged layers come from podman's
# cache. Cargo's registry lives in the volume `mirai-windows-cargo` and the build in
# target/windows/, so a rebuild recompiles only what changed.
#
# Optional mirrors, all unset by default:
#   MIRAI_FEDORA_UPDATES_MIRRORLIST   mirrorlist URL for Fedora's `updates` repository
#   RUSTUP_DIST_SERVER, RUSTUP_UPDATE_ROOT   as for rustup
#   MIRAI_CRATES_INDEX                a crates.io replacement, e.g. sparse+https://.../index/
# CONTAINER_ENGINE=docker uses docker instead of podman.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
engine=${CONTAINER_ENGINE:-podman}
image=localhost/mirai-windows-build

mkdir -p "$repo/target/windows"
log=$repo/target/windows/image.log
"$engine" build -t "$image" -f "$repo/packaging/windows/Containerfile" \
    --build-arg "FEDORA_UPDATES_MIRRORLIST=${MIRAI_FEDORA_UPDATES_MIRRORLIST:-}" \
    --build-arg "RUSTUP_DIST_SERVER=${RUSTUP_DIST_SERVER:-}" \
    --build-arg "RUSTUP_UPDATE_ROOT=${RUSTUP_UPDATE_ROOT:-}" \
    "$repo/packaging/windows" >"$log" 2>&1 ||
    { cat "$log" >&2; echo "building the image failed; log in $log" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n1)
version+=+g$(git -C "$repo" rev-parse --short=10 HEAD)
git -C "$repo" diff --quiet HEAD || version+=.dirty

run() {
    local tty=()
    [ -t 0 ] && tty=(-it)
    "$engine" run --rm "${tty[@]}" \
        -v "$repo:/src" -w /src \
        -v mirai-windows-cargo:/opt/cargo/registry \
        -e CARGO_TARGET_DIR=/src/target/windows \
        -e "MIRAI_VERSION=$version" \
        -e MIRAI_CRATES_INDEX -e RUSTUP_DIST_SERVER -e RUSTUP_UPDATE_ROOT \
        "$image" "$@"
}

case ${1:-package} in
package) run packaging/windows/stage.sh release ;;
debug) run packaging/windows/stage.sh debug ;;
shell) run bash ;;
run)
    shift
    run "$@"
    ;;
*)
    sed -n '5,10p' "$0" >&2
    exit 2
    ;;
esac
