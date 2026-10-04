#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Builds each PKGBUILD in packaging/aur/ in its own clean devtools chroot, which holds only
# that package's makedepends (so mirai-server-git building against GTK fails here),
# installs the packages into a fresh copy of that chroot (no makedepends, so a missing
# `depends` shows; mirai-server first, which must not pull GTK in), smoke-runs both
# binaries there, and runs namcap. Builds HEAD of this checkout (makepkg-local.sh), not the
# working tree, and not GitHub.
#
#   tools/packaging/arch-chroot-test.sh [CHROOT_DIR]      default ~/distros/archbuild
#
# Needs devtools and namcap. The chroot steps run as root through $SUDO (default sudo;
# SUDO=run0 works too).
# From a Wayland session the smoke run also opens the mirai window for 60 seconds.
#
# The chroot keeps its own package cache in CHROOT_DIR/pkgcache. extra-x86_64-build would
# share the host's, and on a derivative that rebuilds Arch packages under the same file
# names (CachyOS), the chroot then finds the derivative's build and rejects its signature.
set -euo pipefail

if [[ ${1:-} == --as-root ]]; then
    work=$2 chroots=$3 wayland=$4
    base=$chroots/extra-x86_64
    delete() {
        [[ -e $1 ]] || return 0
        btrfs subvolume delete "$1" >/dev/null 2>&1 || rm -rf "$1"
    }
    # A root without the marker mkarchroot writes last is left from a failed creation.
    [[ -f $base/root/.arch-chroot ]] || delete "$base/root"
    if [[ -d $base/root ]]; then
        arch-nspawn "$base/root" pacman -Syuu --noconfirm
    else
        mkdir -p "$base" "$chroots/pkgcache"
        mkarchroot -C /usr/share/devtools/pacman.conf.d/extra.conf \
            -M /usr/share/devtools/makepkg.conf.d/x86_64.conf \
            -c "$chroots/pkgcache" "$base/root" base-devel
    fi
    for pkg in "$work"/*/; do
        (cd "$pkg" && makechrootpkg -c -r "$base")
    done

    test=$base/pkgtest
    delete "$test"
    btrfs subvolume snapshot "$base/root" "$test" >/dev/null 2>&1 || cp -a "$base/root" "$test"

    binds=(--bind-ro="$work:/pkgs")
    if [[ -n $wayland ]]; then
        binds+=(--bind-ro="$wayland:/run/mirai-test/wayland-0")
    fi
    arch-nspawn "$test" "${binds[@]}" sh -euc '
        pkgs=$(ls /pkgs/*/*.pkg.tar.zst | grep -v -- -debug-)
        pacman -U --noconfirm $(echo "$pkgs" | grep /mirai-server-git/)
        if pacman -Q gtk4 2>/dev/null; then echo "mirai-server-git pulled in GTK"; exit 1; fi
        pacman -U --noconfirm $(echo "$pkgs" | grep /mirai-git/)
        if ldd /usr/bin/mirai /usr/bin/mirai-server | grep "not found"; then exit 1; fi
        mirai-server --help >/dev/null
        echo "mirai-server --help: ok"
        if [ -S /run/mirai-test/wayland-0 ]; then
            echo "mirai window up for 60 s"
            XDG_RUNTIME_DIR=/run/mirai-test WAYLAND_DISPLAY=wayland-0 timeout 60 mirai || [ $? -eq 124 ]
        fi
    '
    exit
fi

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
chroots=$(realpath -m "${1:-$HOME/distros/archbuild}")
work=$(mktemp -d)
wayland=
if [[ -n ${WAYLAND_DISPLAY:-} && -n ${XDG_RUNTIME_DIR:-} ]]; then
    wayland=$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY
fi

for pkg in mirai-git mirai-server-git; do
    "$repo/tools/packaging/makepkg-local.sh" "$pkg" --prepare "$work/$pkg"
done
mkdir -p "$chroots"
${SUDO:-sudo} env SUDO_USER="$USER" "$(realpath "$0")" --as-root "$work" "$chroots" "$wayland"

namcap "$work"/*/PKGBUILD "$work"/*/*.pkg.tar.zst
echo "packages and logs: $work"
