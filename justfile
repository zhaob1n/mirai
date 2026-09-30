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
# (packaging/aur/PKGBUILD). The install recipes never build: under sudo they would leave a
# root-owned target/.

prefix := env("PREFIX", "/usr/local")
destdir := env("DESTDIR", "")
target := env("CARGO_TARGET_DIR", "target")
appid := "io.github.zhaob1n.Mirai"
root := destdir + prefix
icons := "crates/mirai/resources/icons/hicolor"

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
        test -x "{{ target }}/release/$1" || {
            echo "{{ target }}/release/$1 is missing: run 'just build' first" >&2
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
        msgfmt --desktop --template "data/{{ appid }}.desktop.in" -d po -o "$gen/{{ appid }}.desktop"
        msgfmt --xml --template "data/{{ appid }}.metainfo.xml.in" -d po -o "$gen/{{ appid }}.metainfo.xml"
        install -Dm755 "{{ target }}/release/mirai" -t "{{ root }}/bin/"
        install -Dm644 "$gen/{{ appid }}.desktop" -t "{{ root }}/share/applications/"
        install -Dm644 "$gen/{{ appid }}.metainfo.xml" -t "{{ root }}/share/metainfo/"
        for lang in $(sed 's/#.*//' po/LINGUAS); do
            msgfmt -o "$gen/$lang.mo" "po/$lang.po"
            install -Dm644 "$gen/$lang.mo" "{{ root }}/share/locale/$lang/LC_MESSAGES/mirai.mo"
        done
        install -Dm644 "{{ icons }}/scalable/apps/{{ appid }}.svg" -t "{{ root }}/share/icons/hicolor/scalable/apps/"
        install -Dm644 "{{ icons }}/symbolic/apps/{{ appid }}-symbolic.svg" -t "{{ root }}/share/icons/hicolor/symbolic/apps/"
        install -Dm644 README.md README.zh-CN.md docs/user/GUIDE.md docs/user/GUIDE.zh-CN.md -t "{{ root }}/share/doc/mirai/"
        { set +x; } 2>/dev/null
    fi
    if [ "{{ part }}" != mirai ]; then
        need mirai-server
        set -x
        install -Dm755 "{{ target }}/release/mirai-server" -t "{{ root }}/bin/"
        install -Dm644 crates/mirai-server/server.example.toml -t "{{ root }}/share/doc/mirai-server/"
    fi

# Remove what `install` put under PREFIX.
[arg("part", pattern="mirai|mirai-server|all", help="`mirai` (the GUI), `mirai-server`, or omitted for both")]
[script]
uninstall part="all": && refresh
    set -x
    if [ "{{ part }}" != mirai-server ]; then
        rm -f "{{ root }}/bin/mirai"
        rm -f "{{ root }}/share/applications/{{ appid }}.desktop"
        rm -f "{{ root }}/share/metainfo/{{ appid }}.metainfo.xml"
        for lang in $(sed 's/#.*//' po/LINGUAS); do
            rm -f "{{ root }}/share/locale/$lang/LC_MESSAGES/mirai.mo"
        done
        rm -f "{{ root }}/share/icons/hicolor/scalable/apps/{{ appid }}.svg"
        rm -f "{{ root }}/share/icons/hicolor/symbolic/apps/{{ appid }}-symbolic.svg"
        rm -f "{{ root }}/share/doc/mirai/README.md" "{{ root }}/share/doc/mirai/README.zh-CN.md"
        rm -f "{{ root }}/share/doc/mirai/GUIDE.md" "{{ root }}/share/doc/mirai/GUIDE.zh-CN.md"
        rmdir --ignore-fail-on-non-empty "{{ root }}/share/doc/mirai" 2>/dev/null || true
    fi
    if [ "{{ part }}" != mirai ]; then
        rm -f "{{ root }}/bin/mirai-server"
        rm -f "{{ root }}/share/doc/mirai-server/server.example.toml"
        rmdir --ignore-fail-on-non-empty "{{ root }}/share/doc/mirai-server" 2>/dev/null || true
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
