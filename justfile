# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Build and install without a distribution package:
#
#     just build
#     sudo just install            # both; or `install mirai` / `install mirai-server`
#     sudo just uninstall          # likewise
#
# `cargo install` can place only binaries, not the desktop entry, metainfo and icons, so
# installation lives here. Packages call the same recipes with DESTDIR and PREFIX set
# (packaging/aur/*/PKGBUILD, packaging/msys2/*/PKGBUILD). The install recipes never build:
# under sudo they would leave a root-owned target/.
#
# Staging a build that is not this host's `target/release` (a Windows .exe, an explicit
# `--target`, a debug tree) is environment, not a second install list:
#
#     MIRAI_BIN_DIR=<dir of the binaries>   # wins; do not look under target/
#     MIRAI_EXE_SUFFIX=.exe                  # empty on Unix
#     MIRAI_PROFILE=release                  # used only when MIRAI_BIN_DIR is empty
#     CARGO_BUILD_TARGET=<triple>            # cargo's --target subdirectory, if that was passed
#     CARGO_TARGET_DIR=target
#     MIRAI_DESKTOP=1                        # 0 omits the desktop entry and AppStream metainfo
#
# Translations, icons, docs and the license always install. Linux with no variables set is
# unchanged: `target/release/mirai`, no suffix, desktop metadata included.

prefix := env("PREFIX", "/usr/local")
destdir := env("DESTDIR", "")
target := env("CARGO_TARGET_DIR", "target")
appid := "io.github.zhaob1n.Mirai"
root := destdir + prefix
icons := "crates/mirai/resources/icons/hicolor"
mirai_bin_dir := env("MIRAI_BIN_DIR", "")
mirai_exe_suffix := env("MIRAI_EXE_SUFFIX", "")
mirai_profile := env("MIRAI_PROFILE", "release")
cargo_build_target := env("CARGO_BUILD_TARGET", "")
# 1 installs the desktop entry and AppStream metainfo. 0 omits that Linux metadata.
mirai_desktop := env("MIRAI_DESKTOP", "1")

# Already-built binaries. MIRAI_BIN_DIR wins. Otherwise cargo's layout: an explicit
# --target (including native UCRT64 passing its host tuple) writes
# target/<triple>/<profile>/; a host build writes target/<profile>/.
bin_dir := if mirai_bin_dir != "" {
    mirai_bin_dir
} else if cargo_build_target != "" {
    target / cargo_build_target / mirai_profile
} else {
    target / mirai_profile
}

# The checks every commit owes. Run from the repository root.
check:
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo test --locked --workspace

# Build both binaries in release mode.
build:
    cargo build --locked --release -p mirai -p mirai-server

# Install under PREFIX (default /usr/local), staged under DESTDIR.
[arg("part", pattern="mirai|mirai-server|all", help="`mirai` (the GUI), `mirai-server`, or omitted for both")]
[script]
install part="all": && refresh
    need() {
        test -f "{{ bin_dir }}/$1{{ mirai_exe_suffix }}" || {
            echo "{{ bin_dir }}/$1{{ mirai_exe_suffix }} is missing: run 'just build' first" >&2
            exit 1
        }
    }
    if [ "{{ part }}" != mirai-server ]; then
        need mirai
        # The desktop entry, metainfo and catalogues are merged from po/ into a scratch
        # directory, so installing still writes nothing into the tree.
        gen=$(mktemp -d)
        trap 'rm -rf "$gen"' EXIT
        set -x
        if [ "{{ mirai_desktop }}" != 0 ]; then
            msgfmt --desktop --template "data/{{ appid }}.desktop.in" -d po -o "$gen/{{ appid }}.desktop"
            msgfmt --xml --template "data/{{ appid }}.metainfo.xml.in" -d po -o "$gen/{{ appid }}.metainfo.xml"
        fi
        install -Dm755 "{{ bin_dir }}/mirai{{ mirai_exe_suffix }}" "{{ root }}/bin/mirai{{ mirai_exe_suffix }}"
        if [ "{{ mirai_desktop }}" != 0 ]; then
            install -Dm644 "$gen/{{ appid }}.desktop" -t "{{ root }}/share/applications/"
            install -Dm644 "$gen/{{ appid }}.metainfo.xml" -t "{{ root }}/share/metainfo/"
        fi
        for lang in $(sed 's/#.*//' po/LINGUAS); do
            msgfmt -o "$gen/$lang.mo" "po/$lang.po"
            install -Dm644 "$gen/$lang.mo" "{{ root }}/share/locale/$lang/LC_MESSAGES/mirai.mo"
        done
        install -Dm644 "{{ icons }}/scalable/apps/{{ appid }}.svg" -t "{{ root }}/share/icons/hicolor/scalable/apps/"
        install -Dm644 "{{ icons }}/symbolic/apps/{{ appid }}-symbolic.svg" -t "{{ root }}/share/icons/hicolor/symbolic/apps/"
        install -Dm644 README.md README.zh-CN.md docs/user/GUIDE.md docs/user/GUIDE.zh-CN.md -t "{{ root }}/share/doc/mirai/"
        install -Dm644 LICENSE "{{ root }}/share/licenses/mirai/LICENSE"
        { set +x; } 2>/dev/null
    fi
    if [ "{{ part }}" != mirai ]; then
        need mirai-server
        set -x
        install -Dm755 "{{ bin_dir }}/mirai-server{{ mirai_exe_suffix }}" "{{ root }}/bin/mirai-server{{ mirai_exe_suffix }}"
        install -Dm644 crates/mirai-server/server.example.toml -t "{{ root }}/share/doc/mirai-server/"
        install -Dm644 LICENSE "{{ root }}/share/licenses/mirai-server/LICENSE"
    fi

# Remove what `install` put under PREFIX.
[arg("part", pattern="mirai|mirai-server|all", help="`mirai` (the GUI), `mirai-server`, or omitted for both")]
[script]
uninstall part="all": && refresh
    set -x
    if [ "{{ part }}" != mirai-server ]; then
        rm -f "{{ root }}/bin/mirai{{ mirai_exe_suffix }}"
        rm -f "{{ root }}/share/applications/{{ appid }}.desktop"
        rm -f "{{ root }}/share/metainfo/{{ appid }}.metainfo.xml"
        for lang in $(sed 's/#.*//' po/LINGUAS); do
            rm -f "{{ root }}/share/locale/$lang/LC_MESSAGES/mirai.mo"
        done
        rm -f "{{ root }}/share/icons/hicolor/scalable/apps/{{ appid }}.svg"
        rm -f "{{ root }}/share/icons/hicolor/symbolic/apps/{{ appid }}-symbolic.svg"
        rm -f "{{ root }}/share/doc/mirai/README.md" "{{ root }}/share/doc/mirai/README.zh-CN.md"
        rm -f "{{ root }}/share/doc/mirai/GUIDE.md" "{{ root }}/share/doc/mirai/GUIDE.zh-CN.md"
        rm -f "{{ root }}/share/licenses/mirai/LICENSE"
        rmdir --ignore-fail-on-non-empty "{{ root }}/share/doc/mirai" "{{ root }}/share/licenses/mirai" 2>/dev/null || true
    fi
    if [ "{{ part }}" != mirai ]; then
        rm -f "{{ root }}/bin/mirai-server{{ mirai_exe_suffix }}"
        rm -f "{{ root }}/share/doc/mirai-server/server.example.toml"
        rm -f "{{ root }}/share/licenses/mirai-server/LICENSE"
        rmdir --ignore-fail-on-non-empty "{{ root }}/share/doc/mirai-server" "{{ root }}/share/licenses/mirai-server" 2>/dev/null || true
    fi

# A stale icon cache hides a new icon, so refresh one that exists. Packages leave this to
# the package manager's hooks.
[private]
[script]
refresh:
    [ -z "{{ destdir }}" ] || exit 0
    if [ -f "{{ root }}/share/icons/hicolor/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache -qtf "{{ root }}/share/icons/hicolor"
    fi
    if [ -d "{{ root }}/share/applications" ] && command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "{{ root }}/share/applications"
    fi
