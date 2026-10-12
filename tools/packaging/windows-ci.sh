#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# GitHub Actions driver for the native MSYS2 UCRT64 Windows installer.
# From an UCRT64 shell at the repository root:
#
#   tools/packaging/windows-ci.sh                 test, smoke, and stage the installer
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
    for extra in python nsis icoutils librsvg gst-plugins-good gst-plugins-bad; do
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

use_prefix_toolchain() {
    # Native UCRT64 Cargo must win over any rustup or MSVC rustc that leaked onto PATH.
    export MSYS2_PATH_TYPE=minimal
    export PATH="$(cygpath -u "$MINGW_PREFIX")/bin:$PATH"
    hash -r
    export CARGO_TARGET_DIR=target
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
    host=$(rustc -vV | awk '/^host:/{print $2; exit}')
    [[ $host == x86_64-pc-windows-gnu ]] ||
        die "rustc host is ${host:-unknown}, want x86_64-pc-windows-gnu"
    # Match windows-release.py, or its explicit tool paths dirty C build scripts
    # again when the next cached run starts cargo test.
    export RUSTC="$(cygpath -m "$bin/rustc.exe")"
    export CC="$(cygpath -m "$bin/gcc.exe")"
    export CXX="$(cygpath -m "$bin/g++.exe")"
    export AR="$(cygpath -m "$bin/ar.exe")"
    export PKG_CONFIG="$(cygpath -m "$bin/pkg-config.exe")"
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

smoke_installer() {
    local version=$1 installer installed output status
    installer=$PWD/target/dist/mirai-${version}-windows-x86_64-setup.exe
    installed=$(cygpath -u "$LOCALAPPDATA")/Programs/mirai
    [[ ! -e $installed ]] || die "installer smoke requires an unused per-user installation"
    # This is a runtime probe, not another application build. Run it from the
    # installed bin/ so module lookup and DLL resolution cannot borrow UCRT64.
    glib-compile-resources crates/mirai/resources/mirai.gresource.xml \
        --sourcedir=crates/mirai/resources --generate-source \
        --target=target/windows-smoke-resources.c --c-name=windows_smoke
    # shellcheck disable=SC2046
    gcc -Wall -Wextra -Werror tools/packaging/windows-runtime-smoke.c \
        target/windows-smoke-resources.c -o target/windows-runtime-smoke.exe \
        $(pkg-config --cflags --libs gtk4 libsoup-3.0 gstreamer-1.0)
    # Otherwise MSYS rewrites NSIS's /S switch as a path and the wizard waits for input.
    env MSYS2_ARG_CONV_EXCL='*' PATH='/usr/bin:/bin' \
        /usr/bin/timeout --foreground 120 "$installer" /S
    [[ -f $installed/bin/mirai.exe && -f $installed/uninstall.exe ]] ||
        die "installer did not install the application"
    output=$(env PATH='/usr/bin:/bin' "$installed/bin/mirai-server.exe" --version)
    [[ $output == "mirai-server $(workspace_version)" ]] ||
        die "installed server returned an unexpected version: $output"
    env -u DBUS_SESSION_BUS_ADDRESS -u GIO_EXTRA_MODULES -u GSETTINGS_SCHEMA_DIR \
        -u GDK_PIXBUF_MODULE_FILE -u GDK_PIXBUF_MODULEDIR -u GI_TYPELIB_PATH \
        -u GST_PLUGIN_PATH -u GST_PLUGIN_SYSTEM_PATH -u GST_PLUGIN_SCANNER \
        -u GST_PLUGIN_PATH_1_0 -u GST_PLUGIN_SYSTEM_PATH_1_0 -u GST_PLUGIN_SCANNER_1_0 \
        -u SSL_CERT_FILE -u SSL_CERT_DIR \
        PATH='/usr/bin:/bin' LANGUAGE=en \
        /usr/bin/timeout --foreground 60 "$(cygpath -u "$MINGW_PREFIX")/bin/python.exe" \
        tools/packaging/windows-gui-smoke.py "$(cygpath -m "$installed/bin/mirai.exe")" \
        target/dist/release-smoke.png
    cp target/windows-runtime-smoke.exe "$installed/bin/windows-runtime-smoke.exe"
    rm -f target/runtime-gst-registry.bin
    status=0
    env -u DBUS_SESSION_BUS_ADDRESS -u GIO_EXTRA_MODULES -u GSETTINGS_SCHEMA_DIR \
        -u GDK_PIXBUF_MODULE_FILE -u GDK_PIXBUF_MODULEDIR -u GI_TYPELIB_PATH \
        -u GST_PLUGIN_PATH -u GST_PLUGIN_SYSTEM_PATH -u GST_PLUGIN_SCANNER \
        -u GST_PLUGIN_PATH_1_0 -u GST_PLUGIN_SYSTEM_PATH_1_0 -u GST_PLUGIN_SCANNER_1_0 \
        -u SSL_CERT_FILE -u SSL_CERT_DIR \
        PATH='/usr/bin:/bin' \
        GST_REGISTRY="$(cygpath -m "$PWD/target/runtime-gst-registry.bin")" \
        /usr/bin/timeout --foreground 60 "$installed/bin/windows-runtime-smoke.exe" \
        >target/dist/runtime-smoke.log 2>&1 || status=$?
    rm -f "$installed/bin/windows-runtime-smoke.exe" target/runtime-gst-registry.bin
    cat target/dist/runtime-smoke.log
    [[ $status -eq 0 ]] || die "installed proxy/image/audio smoke exited $status"
    # NSIS copies the uninstaller to TEMP and its launcher exits before that child.
    # Start-Process -Wait waits for the whole process tree, not just the launcher.
    env MSYS2_ARG_CONV_EXCL='*' PATH='/usr/bin:/bin' \
        MIRAI_UNINSTALLER="$(cygpath -w "$installed/uninstall.exe")" \
        /usr/bin/timeout --foreground 120 \
        "$(cygpath -u "$WINDIR")/System32/WindowsPowerShell/v1.0/powershell.exe" \
        -NoProfile -NonInteractive -Command \
        '$ErrorActionPreference = "Stop"; $p = Start-Process -FilePath $env:MIRAI_UNINSTALLER -ArgumentList "/S" -Wait -PassThru; exit $p.ExitCode'
    if [[ -e $installed ]]; then
        find "$installed" -maxdepth 3 -print >&2
        die "uninstaller left the clean installation behind"
    fi
    printf 'windows-ci: installed runtime launched and clean uninstall passed\n'
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
    use_prefix_toolchain
    # Wine rejects IPV6_V6ONLY (WSAEOPNOTSUPP 10045), so QUIC endpoint creation fails
    # there. The hosted Windows runner is the proof; do not narrow this away.
    cargo test --release --locked --workspace --target x86_64-pc-windows-gnu
    stage "$version"
    assert_installer "$version"
    smoke_installer "$version"
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
