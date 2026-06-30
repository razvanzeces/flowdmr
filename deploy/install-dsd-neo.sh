#!/usr/bin/env bash
#
# Build and install dsd-neo (the DMR decoder FlowDMR drives) and its mbelib-neo
# vocoder dependency. Run with sudo on Raspberry Pi OS / Debian (aarch64).
#
# dsd-neo:    https://github.com/arancormonk/dsd-neo
# mbelib-neo: https://github.com/arancormonk/mbelib-neo   (AMBE/IMBE primitives)
#
# AMBE+2 PATENT NOTE: mbelib implements patent-encumbered vocoder math. It is
# widely used for personal, RECEIVE-ONLY experimentation. Shipping it in a
# product, or re-transmitting decoded audio, may have legal implications in your
# jurisdiction. You are responsible for lawful use.
#
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

# Install the RTL-SDR Blog driver first (install-rtlsdr-v4.sh) so librtlsdr is present.

echo "==> Build dependencies for dsd-neo"
apt-get update
apt-get install -y \
  git cmake build-essential pkg-config \
  libsndfile1-dev libssl-dev libncursesw5-dev \
  libpulse-dev libusb-1.0-0-dev

echo "==> mbelib-neo"
SRC=/usr/local/src
rm -rf "$SRC/mbelib-neo"
git clone --depth 1 https://github.com/arancormonk/mbelib-neo "$SRC/mbelib-neo"
cmake -S "$SRC/mbelib-neo" -B "$SRC/mbelib-neo/build" -DCMAKE_BUILD_TYPE=Release
cmake --build "$SRC/mbelib-neo/build" -j"$(nproc)"
cmake --install "$SRC/mbelib-neo/build"
ldconfig

echo "==> dsd-neo"
rm -rf "$SRC/dsd-neo"
git clone --depth 1 https://github.com/arancormonk/dsd-neo "$SRC/dsd-neo"
# BUILD_TESTING=OFF skips the test targets (some trip -Werror=unused-parameter on
# this toolchain); DSD_WARNINGS_AS_ERRORS=OFF keeps a stray warning from failing
# the real build. We only need the dsd-neo binary.
cmake -S "$SRC/dsd-neo" -B "$SRC/dsd-neo/build" -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_TESTING=OFF -DDSD_WARNINGS_AS_ERRORS=OFF
cmake --build "$SRC/dsd-neo/build" -j"$(nproc)"
cmake --install "$SRC/dsd-neo/build"
ldconfig

cat <<EOF

Done. Verify dsd-neo runs and lists RTL input + UDP output support:
  dsd-neo -h

Smoke-test a DMR channel (mono audio out to UDP, console metadata):
  dsd-neo -fs -nm -i rtl:0:439.0000M:0:0:12:0:2 -o udp:127.0.0.1:23470

  -fs  DMR BS/MS      -nm  MONO audio (required: UDP audio is stereo otherwise)
  -i rtl:dev:freq:gain:ppm:bw:squelch:volume     (freq in MHz with M suffix)
  -o udp:host:port    8 kHz s16le PCM

If your build prints source/talkgroup differently, adjust the re_source /
re_talkgroup / re_call_end regexes in the sidecar config.
EOF
