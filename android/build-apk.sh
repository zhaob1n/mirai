#!/usr/bin/env bash
# Builds debug-signed release APKs of mirai for arm64-v8a and x86_64 with pixiewood.
# Run from anywhere; output lands in android/out/.
#
# Environment (defaults match the author's build box):
#   ANDROID_HOME  Android SDK with platforms;android-36, build-tools;36.0.0 and an NDK
#   ANDROID_NDK   NDK (27.2 tested)
#   PIXIEWOOD     pixiewood script (https://github.com/sp1ritCS/gtk-android-builder, master)
#   STUDIO        directory with Android Studio's jars (pixiewood -a), for icon conversion
#   MESON         meson >= 1.11 (GTK 4.24 needs it)
#   KEYSTORE      keystore to sign with (default: ~/.android/debug.keystore, pass "android")
# Host tools: cargo with the *-linux-android targets, blueprint-compiler >= 0.22, msgfmt,
# glib-mkenums >= 2.86 (see the --cross-file / -Dhosttools options in the manifest), python3-gi
# for blueprint-compiler. Meson downloads the wraps (GStreamer 1.28.7, glib-networking, OpenSSL,
# libsoup, ...) on the first build.
# Regenerating checked-in files (not needed for a build):
#   update-icons.sh <adwaita-icon-theme checkout>   -> icons/, icons.gresource.xml (needs rg)
#   gstreamer/gen-options.py <dir with the unpacked gstreamer, gst-plugins-{base,good,bad}
#                             tarballs>              -> gstreamer/meson.build
set -euo pipefail
W=/workspace/gtk-android
: "${ANDROID_HOME:=$W/sdk}" "${ANDROID_NDK:=$ANDROID_HOME/ndk/27.2.12479018}"
: "${PIXIEWOOD:=$W/pw-master/pixiewood}" "${STUDIO:=$W/mini-studio}" "${MESON:=$W/venv/bin/meson}"
: "${KEYSTORE:=$HOME/.android/debug.keystore}"
export ANDROID_HOME PATH=$(dirname "$MESON"):$HOME/.cargo/bin:$PATH
cd "$(dirname "$0")"
manifest=io.github.zhaob1n.Mirai.android.xml
[ -f .pixiewood/pixiewood.ini ] ||
  "$PIXIEWOOD" -v prepare --release -s "$ANDROID_HOME" -t "$ANDROID_NDK" -a "$STUDIO" --meson "$MESON" "$manifest"
"$PIXIEWOOD" -v generate
"$PIXIEWOOD" -v build
bt=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
mkdir -p out
for f in .pixiewood/android/app/build/outputs/apk/release/*-unsigned.apk; do
  o=out/$(basename "${f/-unsigned/-debugsigned}")
  "$bt/zipalign" -f -p 4 "$f" "$o.tmp"
  "$bt/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android --key-pass pass:android \
      --ks-key-alias androiddebugkey --out "$o" "$o.tmp"
  rm -f "$o.tmp" "$o.idsig"
done
ls -la out
