#!/bin/sh
# meson install script: move GStreamer plugins from <libdir>/gstreamer-1.0/ into <libdir>/,
# because an APK only packages the shared libraries directly in lib/<abi>/.
set -eu
dir="${MESON_INSTALL_DESTDIR_PREFIX}/$1"
[ -d "$dir/gstreamer-1.0" ] || exit 0
for f in "$dir"/gstreamer-1.0/libgst*.so; do
  [ -e "$f" ] && mv -f "$f" "$dir/"
done
rmdir "$dir/gstreamer-1.0" 2>/dev/null || true
# gst-plugins-base/-bad always build these helper libraries; nothing in the APK links them
# (checked with llvm-readelf over libmirai.so, libgtk-4.so and the plugins main.c loads).
for l in adaptivedemux analytics app badaudio basecamerabinsrc codecparsers codecs controller \
         fft insertbin isoff mpegts net photography player rtp rtsp sctp sdp transcoder \
         uridownloader webrtc; do
  rm -f "$dir/libgst$l-1.0.so"
done
rm -f "$dir/libgstopengl.so"
