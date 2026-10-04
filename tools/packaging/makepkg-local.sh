#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Builds one of the PKGBUILDs in packaging/aur/ from this checkout instead of GitHub. The
# PKGBUILD is copied with only its source= line pointing at the local repository, pinned to
# HEAD: the last commit, not uncommitted changes (`just install` covers those).
#
#   tools/packaging/makepkg-local.sh PKG [MAKEPKG_ARGS...]   builds in target/archpkg/PKG
#   tools/packaging/makepkg-local.sh PKG --prepare DIR       writes DIR/PKGBUILD only
#
# PKG is a directory of packaging/aur/: mirai-git or mirai-server-git.
# With cargo from rustup rather than a package, pass -d: makepkg then skips its dependency
# check, and `sudo pacman -U` on the result still resolves the runtime depends.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
commit=$(git -C "$repo" rev-parse HEAD)

pkg=${1:-}
pkgbuild=$repo/packaging/aur/$pkg/PKGBUILD
if [[ -z $pkg || ! -f $pkgbuild ]]; then
    echo "usage: $0 mirai-git|mirai-server-git [--prepare DIR | MAKEPKG_ARGS...]" >&2
    exit 1
fi
shift

prepare_only=0
dir=$repo/target/archpkg/$pkg
if [[ ${1:-} == --prepare ]]; then
    prepare_only=1 dir=$2
fi

mkdir -p "$dir"
# The PKGBUILD is bash: %q quotes a checkout path holding spaces or quotes, and awk takes
# the line from the environment, so nothing in the path is read as a sed/awk pattern.
SOURCE_LINE="source=(\"\$_repo::\"$(printf %q "git+file://$repo#commit=$commit"))" \
    awk '/^source=/ { print ENVIRON["SOURCE_LINE"]; n++; next } { print } END { exit n != 1 }' \
    "$pkgbuild" >"$dir/PKGBUILD" ||
    { echo "PKGBUILD needs exactly one source= line to rewrite" >&2; exit 1; }

(( prepare_only )) && exit 0
cd "$dir"
makepkg "$@"
