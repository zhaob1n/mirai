#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Builds packaging/aur/PKGBUILD from this checkout instead of GitHub. The PKGBUILD is
# copied with only its source= line pointing at the local repository, pinned to HEAD:
# the last commit, not uncommitted changes (`just install` covers those).
#
#   tools/packaging/makepkg-local.sh [MAKEPKG_ARGS...]     builds in target/archpkg
#   tools/packaging/makepkg-local.sh --prepare DIR         writes DIR/PKGBUILD only
#
# With cargo from rustup rather than a package, pass -d: makepkg then skips its dependency
# check, and `sudo pacman -U` on the result still resolves the runtime depends.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
commit=$(git -C "$repo" rev-parse HEAD)

prepare_only=0
dir=$repo/target/archpkg
if [[ ${1:-} == --prepare ]]; then
    prepare_only=1 dir=$2
fi

mkdir -p "$dir"
# The PKGBUILD is bash: %q quotes a checkout path holding spaces or quotes, and awk takes
# the line from the environment, so nothing in the path is read as a sed/awk pattern.
SOURCE_LINE="source=(\"\$_repo::\"$(printf %q "git+file://$repo#commit=$commit"))" \
    awk '/^source=/ { print ENVIRON["SOURCE_LINE"]; n++; next } { print } END { exit n != 1 }' \
    "$repo/packaging/aur/PKGBUILD" >"$dir/PKGBUILD" ||
    { echo "PKGBUILD needs exactly one source= line to rewrite" >&2; exit 1; }

(( prepare_only )) && exit 0
cd "$dir"
makepkg "$@"
