#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# GitHub Actions driver for the native MSYS2 UCRT64 Windows installer.
# From an UCRT64 shell at the repository root:
#
#   tools/packaging/windows-ci.sh                 build and stage the installer
#   tools/packaging/windows-ci.sh version         print the validated version and exit
#   tools/packaging/windows-ci.sh packages        print UCRT64 dependencies without MSYS2
#   tools/packaging/windows-ci.sh publish DIR     attach DIR's setup.exe to the tag release
#
# The build does not clone GitHub and does not run a VCS PKGBUILD. Dependencies are the
# union of top-level depends and makedepends from these two files, each sourced in its own
# subshell with MINGW_PACKAGE_PREFIX=mingw-w64-ucrt-x86_64 (a second source in the same
# shell would overwrite the arrays):
#
#   packaging/msys2/mirai-git/PKGBUILD
#   packaging/msys2/mirai-server-git/PKGBUILD
#
# optdepends, provides, conflicts, mingw_arch, source, sha256sums, options and license are
# not install lists. Release-only tools the bundler calls (python, nsis, icoutils, librsvg)
# are added after that union. rust, just, gcc (via cc) and gettext are already in the arrays
# and are not duplicated. No zip.
#
# The installer runtime then replaces GTK with the D3D12 build pinned in
# packaging/windows/gtk4-d3d12.sha256 and cairo with the build pinned in
# packaging/windows/cairo.sha256, and adds the GStreamer plugin packages without their
# dependencies: only a few plugins ship, and bundle_imports checks their imports.
#
# Tag pushes must be named v<workspace semver> or v<workspace semver>-<prerelease>.
# workflow_dispatch and a local run with no event use the Cargo workspace version and
# do not publish. Untrusted ref names arrive only in MIRAI_GITHUB_REF_NAME / RELEASE_TAG
# and are rejected unless they match that grammar; they are never eval'd.
#
# The stage tool builds. This script does not cargo-build the installer itself:
#
#   python tools/packaging/windows-release.py \
#     --prefix "$(cygpath -m "$MINGW_PREFIX")" --target-dir target --version "$VERSION"
#
# Installer: target/dist/mirai-$VERSION-windows-x86_64-setup.exe
set -euo pipefail

die() {
    printf 'windows-ci: %s\n' "$*" >&2
    exit 1
}

invoke_dir=$PWD
cd "$(dirname "$0")/../.."
[[ -f Cargo.toml ]] || die "Cargo.toml not found; run from a checkout"

# Drop the Actions token before any build command. The publish subcommand is a
# separate process and sets GH_TOKEN itself; this build must not see a credential.
if [[ ${1:-build} != publish ]]; then
    unset GITHUB_TOKEN GH_TOKEN ACTIONS_RUNTIME_TOKEN \
        ACTIONS_ID_TOKEN_REQUEST_TOKEN ACTIONS_ID_TOKEN_REQUEST_URL
fi

workspace_version() {
    awk '
        /^\[workspace\.package\]/ { in_ws = 1; next }
        /^\[/ { in_ws = 0 }
        in_ws && /^version[[:space:]]*=[[:space:]]*"/ {
            if (match($0, /"[^"]+"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                exit
            }
        }
    ' Cargo.toml
}

# A semver prerelease: dot-separated identifiers, numeric ones without leading zeros.
valid_prerelease() {
    local pre=$1 id
    [[ $pre =~ ^[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*$ ]] || return 1
    local IFS=.
    # shellcheck disable=SC2086
    for id in $pre; do
        if [[ $id =~ ^[0-9]+$ && $id == 0[0-9]* ]]; then
            return 1
        fi
    done
}

valid_semver() {
    local v=$1 core pre part
    [[ $v =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]] || return 1
    core=${BASH_REMATCH[1]}
    pre=${BASH_REMATCH[2]#-}
    local IFS=.
    # shellcheck disable=SC2086
    for part in $core; do
        [[ $part == 0[0-9]* ]] && return 1
    done
    [[ -z ${BASH_REMATCH[2]} ]] || valid_prerelease "$pre"
}

# Print the version the installer is named with. Reads MIRAI_GITHUB_* only, never
# GITHUB_REF / GITHUB_REF_NAME, so an automatic Actions variable cannot skip the check.
resolve_version() {
    local event=${MIRAI_GITHUB_EVENT_NAME-} ws
    ws=$(workspace_version)
    [[ -n $ws ]] || die "Cargo.toml has no [workspace.package] version"
    valid_semver "$ws" || die "workspace version is not a semver: $ws"
    if [[ -z $event ]]; then
        if [[ -n ${GITHUB_ACTIONS-} ]]; then
            die "MIRAI_GITHUB_EVENT_NAME is unset"
        fi
        printf '%s\n' "$ws"
        return
    fi
    case $event in
        workflow_dispatch)
            printf '%s\n' "$ws"
            ;;
        push)
            [[ ${MIRAI_GITHUB_REF_TYPE-} == tag ]] || die "refusing a non-tag push"
            version_from_tag "$ws"
            ;;
        *)
            die "refusing event"
            ;;
    esac
}

version_from_tag() {
    local ws=$1 tag=${MIRAI_GITHUB_REF_NAME-} rest pre
    # Fail closed before the raw value is printed or passed to a command.
    [[ $tag =~ ^v[0-9A-Za-z.+-]+$ ]] || die "tag name is not a safe v-semver token"
    [[ ${#tag} -le 80 ]] || die "tag name is too long"
    rest=${tag#v}
    if [[ $rest == "$ws" ]]; then
        printf '%s\n' "$rest"
        return
    fi
    if [[ $rest == "$ws"-* ]]; then
        pre=${rest#"$ws"-}
        valid_prerelease "$pre" || die "tag prerelease is not a semver suffix of $ws"
        printf '%s\n' "$rest"
        return
    fi
    die "tag does not match workspace version $ws"
}

require_ucrt64() {
    [[ ${MSYSTEM-} == UCRT64 ]] || die "need an MSYS2 UCRT64 shell (MSYSTEM=${MSYSTEM-unset})"
    [[ ${MINGW_PREFIX-} == /ucrt64 ]] || die "MINGW_PREFIX=${MINGW_PREFIX-unset}, want /ucrt64"
    command -v cygpath >/dev/null || die "cygpath missing; not an MSYS2 shell"
    command -v pacman >/dev/null || die "pacman missing"
    export MINGW_PACKAGE_PREFIX=mingw-w64-ucrt-x86_64
}

# Top-level depends and makedepends only. package() bodies are not executed.
collect_one() {
    local file=$1
    (
        set +u
        export MINGW_PACKAGE_PREFIX=mingw-w64-ucrt-x86_64
        # shellcheck disable=SC1090
        source "$file" || exit 1
        printf '%s\n' "${depends[@]}" "${makedepends[@]}"
    )
}

package_list() {
    local MINGW_PACKAGE_PREFIX=mingw-w64-ucrt-x86_64
    # Explicit files, each in its own subshell. Do not source both in one shell: the
    # second PKGBUILD overwrites depends and makedepends.
    local -a files=(
        packaging/msys2/mirai-git/PKGBUILD
        packaging/msys2/mirai-server-git/PKGBUILD
    )
    local -a specs=() unique=()
    local file spec extra output contributed=0
    local -A seen=()
    for file in "${files[@]}"; do
        [[ -f $file ]] || die "missing $file; refusing to guess packages"
        printf 'windows-ci: sourcing %s\n' "$file" >&2
        output=$(collect_one "$file") || die "failed to source $file"
        while IFS= read -r spec || [[ -n $spec ]]; do
            [[ -n $spec ]] || continue
            specs+=("$spec")
            contributed=1
        done <<<"$output"
    done
    [[ $contributed -eq 1 ]] ||
        die "PKGBUILD depends and makedepends are empty; keep those arrays at top level"
    # Release-only tools the bundler invokes. Not a second GUI library list.
    for extra in python nsis icoutils librsvg; do
        specs+=("${MINGW_PACKAGE_PREFIX}-${extra}")
    done
    for spec in "${specs[@]}"; do
        local pattern='^[[:alnum:]._+:=<>~-]+$'
        [[ $spec =~ $pattern ]] || die "refusing unsafe package spec from PKGBUILD"
        [[ -n ${seen[$spec]:-} ]] && continue
        seen[$spec]=1
        unique+=("$spec")
    done
    [[ ${#unique[@]} -gt 0 ]] || die "dependency list is empty"
    printf '%s\n' "${unique[@]}"
}

install_packages() {
    local -a pkgs=()
    local spec output
    output=$(package_list) || die "could not read the package list"
    while IFS= read -r spec || [[ -n $spec ]]; do
        [[ -n $spec ]] || continue
        pkgs+=("$spec")
    done <<<"$output"
    [[ ${#pkgs[@]} -gt 0 ]] || die "dependency list is empty"
    printf 'windows-ci: installing %s packages\n' "${#pkgs[@]}"
    printf 'windows-ci: %s\n' "${pkgs[@]}"
    pacman -S --needed --noconfirm "${pkgs[@]}"
}

# Runtime packages outside setup-msys2's cache: the pinned GTK and cairo builds and the
# GStreamer plugin packages. The workflow caches this directory.
RUNTIME_PACKAGES=target/windows-packages

# install_pinned NAME RELEASES [OFFICIAL...]
# Installs mingw-w64-ucrt-x86_64-NAME as pinned by packaging/windows/NAME.sha256, from
# RELEASES/NAME-VERSION/, first removing the OFFICIAL packages it replaces (without their
# dependents: what depends on them is satisfied by the package's provides).
install_pinned() {
    local name=$1 releases=$2 pin=$PWD/packaging/windows/$1.sha256 cache=$PWD/$RUNTIME_PACKAGES
    local file version installed
    shift 2
    file=$(awk 'NR == 1 { print $2 }' "$pin")
    [[ $file =~ ^mingw-w64-ucrt-x86_64-$name-([0-9A-Za-z.+_-]+)-any\.pkg\.tar\.zst$ ]] ||
        die "unexpected package name in $pin"
    version=${BASH_REMATCH[1]}
    if ! (cd "$cache" && sha256sum --check --status "$pin" 2>/dev/null); then
        curl -fsSL --retry 3 -o "$cache/$file" "$releases/$name-$version/$file"
        (cd "$cache" && sha256sum --check "$pin") || die "$file checksum mismatch"
    fi
    installed=$(pacman -Q "mingw-w64-ucrt-x86_64-$name" 2>/dev/null | awk '{ print $2 }' || true)
    [[ $installed != "$version" ]] || return 0
    # Replacing the official package in place, the pin must stay ahead of it, or -U
    # downgrades it under packages built against the newer one.
    [[ $# -ne 0 || -z $installed || $(vercmp "$installed" "$version") -lt 0 ]] ||
        die "MSYS2's $name $installed is not older than the pinned $version; rebase the fork and republish"
    local -a official=()
    [[ $# -eq 0 ]] || mapfile -t official < <(pacman -Qq "$@" 2>/dev/null || true)
    [[ ${#official[@]} -eq 0 ]] || pacman -Rdd --noconfirm "${official[@]}"
    pacman -U --noconfirm --cachedir "$cache" "$cache/$file"
}

install_runtime_packages() {
    local cache=$PWD/$RUNTIME_PACKAGES
    mkdir -p "$cache"
    install_pinned gtk4-d3d12 https://github.com/zhaob1n/gtk-d3d12/releases/download \
        mingw-w64-ucrt-x86_64-gtk4 mingw-w64-ucrt-x86_64-gtk4-media-gstreamer
    # Same package name as the official cairo, so -U replaces it in place.
    install_pinned cairo https://github.com/zhaob1n/cairo/releases/download
    pacman -Sdd --needed --noconfirm --cachedir "$cache" \
        mingw-w64-ucrt-x86_64-gst-plugins-good mingw-w64-ucrt-x86_64-gst-plugins-bad
    paccache -rq -k1 -c "$cache"
}

use_prefix_toolchain() {
    # Native UCRT64 Cargo must win over any rustup or MSVC rustc that leaked onto PATH.
    export MSYS2_PATH_TYPE=minimal
    export PATH="$(cygpath -u "$MINGW_PREFIX")/bin:$PATH"
    hash -r
    unset CARGO_BUILD_TARGET RUSTUP_TOOLCHAIN
    unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_SYSROOT_DIR_x86_64_pc_windows_gnu \
        PKG_CONFIG_LIBDIR_x86_64_pc_windows_gnu PKG_CONFIG_PATH_x86_64_pc_windows_gnu
    local bin tool path host
    bin=$(cygpath -u "$MINGW_PREFIX")/bin
    for tool in python cargo rustc gcc just msgfmt makensis icotool rsvg-convert; do
        path=$(command -v "$tool" 2>/dev/null || true)
        [[ -n $path ]] || die "$tool is not installed (PKGBUILD arrays or release-only tools)"
        case $path in
            "$bin"/*) ;;
            *) die "$tool resolved to $path, want $bin" ;;
        esac
    done
    # Read to the end: an early-exiting reader closes the pipe, and rustc panics on the
    # write with exit code 101, which pipefail turns into a failed run.
    host=$(rustc -vV | awk '/^host:/ { host = $2 } END { print host }')
    [[ $host == x86_64-pc-windows-gnu ]] ||
        die "rustc host is ${host:-unknown}, want x86_64-pc-windows-gnu"
}

stage() {
    local version=$1
    local -a args=(
        --prefix "$(cygpath -m "$MINGW_PREFIX")"
        --target-dir target
        --version "$version"
    )
    python tools/packaging/windows-release.py "${args[@]}"
}

assert_installer() {
    local version=$1
    local installer=target/dist/mirai-${version}-windows-x86_64-setup.exe
    local magic
    [[ -f $installer ]] || die "installer missing: $installer"
    magic=$(od -An -t x1 -N 2 "$installer" | tr -d ' \n')
    [[ $magic == 4d5a ]] || die "installer is not a PE executable"
    if command -v sha256sum >/dev/null; then
        sha256sum "$installer"
    fi
    printf 'windows-ci: installer %s\n' "$installer"
}

build_pipeline() {
    local version sha=${GITHUB_SHA-}
    version=$(resolve_version)
    # The raw ref has been checked. Do not leave it around for cargo or the bundler.
    unset MIRAI_GITHUB_REF_NAME MIRAI_GITHUB_REF_TYPE
    printf 'windows-ci: version %s\n' "$version"
    if [[ $sha =~ ^[0-9a-fA-F]{7,64}$ ]]; then
        printf 'windows-ci: revision %s\n' "$sha"
    fi
    require_ucrt64
    install_packages
    install_runtime_packages
    use_prefix_toolchain
    stage "$version"
    assert_installer "$version"
}

publish_release() {
    local dir=${1:-} repo=${MIRAI_GITHUB_REPOSITORY-} ws version tag asset magic
    [[ -n ${GITHUB_ACTIONS-} ]] || die "publish only runs on GitHub Actions"
    [[ $repo == zhaob1n/mirai ]] || die "refusing to publish outside zhaob1n/mirai"
    [[ -n ${GH_TOKEN:-}${GITHUB_TOKEN:-} ]] || die "no publish token"
    command -v gh >/dev/null || die "gh is not installed"
    [[ -n $dir && -d $dir ]] || die "usage: windows-ci.sh publish DIR"
    ws=$(workspace_version)
    valid_semver "$ws" || die "workspace version is not a semver: $ws"
    MIRAI_GITHUB_EVENT_NAME=push \
        MIRAI_GITHUB_REF_TYPE=tag \
        MIRAI_GITHUB_REF_NAME=${RELEASE_TAG-} \
        version=$(resolve_version)
    tag=v$version
    [[ ${RELEASE_TAG-} == "$tag" ]] || die "RELEASE_TAG does not match the validated tag"
    local -a found=()
    local file
    while IFS= read -r -d '' file; do
        found+=("$file")
    done < <(find "$dir" -type f -name "mirai-${version}-windows-x86_64-setup.exe" -print0)
    [[ ${#found[@]} -eq 1 ]] || die "expected one mirai-${version}-windows-x86_64-setup.exe in $dir"
    asset=${found[0]}
    [[ $(basename "$asset") == "mirai-${version}-windows-x86_64-setup.exe" ]] ||
        die "installer name does not match the validated version"
    magic=$(od -An -t x1 -N 2 "$asset" | tr -d ' \n')
    [[ $magic == 4d5a ]] || die "installer is not a PE executable"
    # Create only when missing. Never delete the release or its other assets.
    # --clobber replaces this installer's name alone.
    if gh release view "$tag" --repo "$repo" --json tagName --jq .tagName >/dev/null 2>&1; then
        printf 'windows-ci: release %s exists; uploading installer only\n' "$tag"
    else
        local -a pre=()
        [[ $version == *-* ]] && pre=(--prerelease)
        gh release create "$tag" --repo "$repo" --verify-tag --title "$tag" \
            "${pre[@]}" --generate-notes
    fi
    gh release upload "$tag" --repo "$repo" "$asset" --clobber
    printf 'windows-ci: published %s to %s\n' "$(basename "$asset")" "$tag"
}

main() {
    case ${1:-build} in
        build)
            build_pipeline
            ;;
        version)
            resolve_version
            ;;
        packages)
            package_list
            ;;
        publish)
            shift
            dir=${1:-}
            if [[ -n $dir && $dir != /* ]]; then
                dir=$invoke_dir/$dir
            fi
            publish_release "$dir"
            ;;
        *)
            die "usage: windows-ci.sh [build|version|packages|publish DIR]"
            ;;
    esac
}

main "$@"
