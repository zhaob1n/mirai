#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Builds mirai for x86_64-pc-windows-gnu and stages a self-contained tree:
#
#     bin/   mirai.exe, mirai-server.exe, every DLL they load, gdbus.exe
#     lib/   gdk-pixbuf loaders, the GStreamer plugins stone sounds need, GIO's TLS module
#     share/ Adwaita and hicolor icons, compiled GSettings schemas, translations
#     doc/   licence, README, guide, example server configuration
#
# GTK finds its data relative to the directory holding its DLLs, so the layout is the
# MinGW prefix layout. Runs inside the image from packaging/windows/Containerfile, via
# tools/packaging/windows-cross.sh; see docs/dev/PACKAGING.md.
#
#     stage.sh release    build and stage, then a portable zip and an installer in
#                         $CARGO_TARGET_DIR/dist/
#     stage.sh debug      build and stage the debug build (harness included) only

set -euo pipefail

profile=${1:-release}
case $profile in
release | debug) ;;
*)
    echo "usage: $0 [release|debug]" >&2
    exit 2
    ;;
esac

target=x86_64-pc-windows-gnu
sysroot=${MINGW_SYSROOT:?not inside the Windows build image}
out=${CARGO_TARGET_DIR:?}
semver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
version=${MIRAI_VERSION:-$semver}
name=mirai-$version-windows-x86_64
[ "$profile" = debug ] && name=$name-debug
stage=$out/dist/$name
res=$out/resources

if [ -n "${MIRAI_CRATES_INDEX:-}" ]; then
    printf '[source.crates-io]\nreplace-with = "mirror"\n[source.mirror]\nregistry = "%s"\n' \
        "$MIRAI_CRATES_INDEX" >"$CARGO_HOME/config.toml"
fi

# The icon and version information Explorer, the taskbar, "Open with" and Task Manager show
# for each executable, linked in as a COFF resource object. Only the final binary takes the
# link argument, so switching it never rebuilds a dependency.
mkdir -p "$res"
svg=crates/mirai/resources/icons/hicolor/scalable/apps/io.github.zhaob1n.Mirai.svg
pngs=()
for size in 16 24 32 48 64 128 256; do
    rsvg-convert -w "$size" -h "$size" -o "$res/icon-$size.png" "$svg"
    pngs+=("$res/icon-$size.png")
done
icotool -c -o "$res/mirai.ico" "${pngs[@]}"

IFS=. read -r major minor patch <<<"${semver%%[-+]*}"
resource() { # binary description
    cat >"$res/$1.rc" <<EOF
#include <winver.h>
1 ICON "mirai.ico"
1 VERSIONINFO
FILEVERSION $major,$minor,$patch,0
PRODUCTVERSION $major,$minor,$patch,0
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "$2"
      VALUE "FileVersion", "$version"
      VALUE "InternalName", "$1"
      VALUE "LegalCopyright", "Copyright (C) 2026 Huang Zhaobin. GPL-3.0-or-later."
      VALUE "OriginalFilename", "$1.exe"
      VALUE "ProductName", "mirai"
      VALUE "ProductVersion", "$version"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
EOF
    x86_64-w64-mingw32-windres -I "$res" -O coff -o "$res/$1.o" "$res/$1.rc"
}
resource mirai "mirai"
resource mirai-server "mirai-server"

flags=(--locked --target "$target")
[ "$profile" = release ] && flags+=(--release)
for bin in mirai mirai-server; do
    cargo rustc "${flags[@]}" -p "$bin" --bin "$bin" -- -C "link-arg=$res/$bin.o"
done

rm -rf "$stage"
mkdir -p "$stage/bin"

# The DLLs an image imports, resolved against the sysroot. A name the sysroot does not
# carry is a system DLL (KERNEL32, USER32, msvcrt, ...) and stays out of the tree.
imports() {
    x86_64-w64-mingw32-objdump -p "$1" | sed -n 's/^\s*DLL Name: //p'
}

declare -A seen=()
bundle() {
    local dll
    for dll in $(imports "$1"); do
        local key=${dll,,}
        [ -n "${seen[$key]:-}" ] && continue
        seen[$key]=1
        local path
        path=$(find "$sysroot/bin" -maxdepth 1 -iname "$dll" -print -quit)
        [ -n "$path" ] || continue
        cp "$path" "$stage/bin/"
        bundle "$path"
    done
}

for exe in mirai mirai-server; do
    cp "$out/$target/$profile/$exe.exe" "$stage/bin/"
done

# GLib starts a private session bus with gdbus.exe, found beside libgio, so that
# GApplication can keep one process per user and hand a second launch's files to it.
cp "$sysroot/bin/gdbus.exe" "$stage/bin/"

loaders=lib/gdk-pixbuf-2.0/2.10.0
mkdir -p "$stage/$loaders/loaders"
cp "$sysroot/$loaders"/loaders/*.dll "$stage/$loaders/loaders/"
cp "$sysroot/$loaders/loaders.cache" "$stage/$loaders/"

# libsoup reaches `https://` through GIO's TLS backend, a module GLib loads by name from
# lib/gio/modules beside its own DLL. Only the GnuTLS one: glib-networking's proxy modules
# read GNOME's settings or proxy environment variables, not Windows' proxy setting.
mkdir -p "$stage/lib/gio/modules"
cp "$sysroot/lib/gio/modules/libgiognutls.dll" "$stage/lib/gio/modules/"

# Stone sounds play through GtkMediaFile, whose GStreamer backend builds a playbin3 fed by
# a giostreamsrc; GstPlay aborts the process when playbin3 is missing. These are the
# elements an in-memory WAV needs on its way to a Windows audio sink. mirai points
# GStreamer at this directory itself (crates/mirai/src/windows_package.rs); the plugin scanner helper is left out because this GStreamer cannot locate it
# either, and falls back to scanning in-process.
plugins=(coreelements typefindfunctions playback gio audioconvert audioresample audiofx volume
    wavparse autodetect wasapi directsound)
mkdir -p "$stage/lib/gstreamer-1.0"
for plugin in "${plugins[@]}"; do
    cp "$sysroot/lib/gstreamer-1.0/libgst$plugin.dll" "$stage/lib/gstreamer-1.0/"
done

for image in "$stage"/bin/*.exe "$stage/$loaders"/loaders/*.dll "$stage"/lib/gio/modules/*.dll \
    "$stage"/lib/gstreamer-1.0/*.dll; do
    bundle "$image"
done

mkdir -p "$stage/share/icons" "$stage/share/glib-2.0/schemas"
cp -r "$sysroot/share/icons/Adwaita" "$sysroot/share/icons/hicolor" "$stage/share/icons/"
glib-compile-schemas --targetdir="$stage/share/glib-2.0/schemas" "$sysroot/share/glib-2.0/schemas"

# mirai's catalogues, compiled as `just install` compiles them, and GTK's, GLib's and
# libadwaita's for the same languages. GLib translates GTK's own strings only for a
# language the application has a catalogue for (crates/mirai/src/i18n.rs), so these are the
# only ones that can ever show.
while read -r lang; do
    lang=${lang%%#*}
    lang=${lang//[[:space:]]/}
    [ -n "$lang" ] || continue
    messages=$stage/share/locale/$lang/LC_MESSAGES
    mkdir -p "$messages"
    msgfmt --check -o "$messages/mirai.mo" "po/$lang.po"
    for domain in gtk40 glib20 libadwaita; do
        if [ -f "$sysroot/share/locale/$lang/LC_MESSAGES/$domain.mo" ]; then
            cp "$sysroot/share/locale/$lang/LC_MESSAGES/$domain.mo" "$messages/"
        fi
    done
done <po/LINGUAS

if [ "$profile" = debug ]; then
    echo "$stage"
    exit 0
fi

# Fedora's MinGW DLLs keep their symbols; libstdc++ alone is 29 MB before this.
find "$stage" \( -iname '*.dll' -o -iname '*.exe' \) -print0 |
    xargs -0 x86_64-w64-mingw32-strip --strip-unneeded
mkdir -p "$stage/doc"
cp LICENSE README.md docs/user/GUIDE.md crates/mirai-server/server.example.toml "$stage/doc/"

rm -f "$out/dist/$name.zip" "$out/dist/$name-setup.exe"
(cd "$out/dist" && zip -qr9 "$name.zip" "$name")
makensis -V2 -INPUTCHARSET UTF8 \
    -DVERSION="$version" -DSTAGE="$stage" -DICON="$res/mirai.ico" \
    -DOUTFILE="$out/dist/$name-setup.exe" packaging/windows/installer.nsi
echo "$out/dist/$name.zip"
echo "$out/dist/$name-setup.exe"
