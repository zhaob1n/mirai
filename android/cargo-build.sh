#!/bin/sh
# Builds crates/mirai as a static library for one Android ABI.
# Usage: cargo-build.sh <rust-target> <ndk-clang> <cargo-target-dir> <output.a>
#   <ndk-clang> is the NDK compiler driver, e.g. .../bin/x86_64-linux-android31-clang
# Needs: cargo with the target installed,
# blueprint-compiler >= 0.22 and msgfmt on PATH (MIRAI_HOSTTOOLS is prepended if set).
set -eu
target=$1 clang=$2 tdir=$3
out=$(cd "$(dirname "$4")" && pwd)/$(basename "$4")
here=$(cd "$(dirname "$0")" && pwd)
tc=$(dirname "$clang")
[ -n "${MIRAI_HOSTTOOLS:-}" ] && PATH=$MIRAI_HOSTTOOLS:$PATH
PATH=$HOME/.cargo/bin:$PATH
export PATH
u=$(echo "$target" | tr 'a-z-' 'A-Z_'); l=$(echo "$target" | tr '-' '_')
export "CC_$l=$clang" "AR_$l=$tc/llvm-ar" "CARGO_TARGET_${u}_LINKER=$clang"
# The -sys crates only declare FFI; nothing is linked into a staticlib, so skip pkg-config
# (meson links the real GTK/libadwaita/libsoup it built when it links libmirai.so).
for d in GLIB_2_0:glib-2.0 GOBJECT_2_0:gobject-2.0 GIO_2_0:gio-2.0 CAIRO:cairo CAIRO_GOBJECT:cairo-gobject \
         GDK_PIXBUF_2_0:gdk_pixbuf-2.0 PANGO:pango-1.0 GRAPHENE_GOBJECT_1_0:graphene-1.0 GTK4:gtk-4 \
         LIBADWAITA_1:adwaita-1 LIBSOUP_3_0:soup-3.0; do
  export "SYSTEM_DEPS_${d%%:*}_NO_PKG_CONFIG=1" "SYSTEM_DEPS_${d%%:*}_LIB=${d#*:}"
done
cd "$here/.."
cargo build --release --locked --target "$target" --target-dir "$tdir" -p mirai --example mirai-android
cp "$tdir/$target/release/examples/libmirai_android.a" "$out"
