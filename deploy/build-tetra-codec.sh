#!/usr/bin/env bash
#
# Build + install the OPEN-SOURCE TETRA ACELP speech codec that FlowStation (and
# FlowDMR) link for PCM<->ACELP transcoding. This is the ETSI EN 300 395-2
# reference codec, packaged by outerplane:
#
#   https://github.com/outerplane/tetra-codec
#
# It installs libtetra-codec.so + tetra-codec.pc (pkg-config) + tetra-codec.h, so
# `cargo build --release --features flowdmr` links the REAL codec (audible output).
# Run with sudo on the Pi.
#
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

echo "==> Dependencies"
apt-get update
apt-get install -y git cmake build-essential pkg-config

echo "==> Building outerplane/tetra-codec"
SRC=/usr/local/src/tetra-codec
rm -rf "$SRC"
git clone --depth 1 https://github.com/outerplane/tetra-codec "$SRC"
cmake -S "$SRC" -B "$SRC/build" -DCMAKE_BUILD_TYPE=Release
cmake --build "$SRC/build" -j"$(nproc)"
cmake --install "$SRC/build"
ldconfig

echo "==> Verifying"
if pkg-config --exists tetra-codec; then
  echo "OK: pkg-config tetra-codec -> $(pkg-config --libs tetra-codec)"
else
  echo "NOTE: tetra-codec.pc not on the default pkg-config path."
  echo "      Add this to the build env (and to the tetra/flowdmr systemd units if needed):"
  echo "        export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:\$PKG_CONFIG_PATH"
  echo "      Or build with: TETRA_CODEC_LIB_DIR=/usr/local/lib cargo build --release --features flowdmr"
fi

cat <<EOF

Done. Now build FlowStation with the REAL codec (audible):
  cd /opt/flowstation && cargo build --release --features flowdmr
  sudo systemctl restart tetra

(This same codec also powers FlowStation's optional 'asterisk' feature.)
EOF
