#!/usr/bin/env bash
#
# Install the flowdmr-sidecar binary, a default config, and the systemd unit.
# Run with sudo from the flowdmr repo root AFTER `cargo build --release -p flowdmr-sidecar`.
#
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

HERE="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$HERE/target/release/flowdmr-sidecar"
[ -x "$BIN" ] || { echo "build first: cargo build --release -p flowdmr-sidecar"; exit 1; }

echo "==> Service user 'flowdmr' (in plugdev for USB/RTL-SDR access)"
id -u flowdmr >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin flowdmr
usermod -aG plugdev flowdmr || true

echo "==> Installing binary -> /usr/local/bin/flowdmr-sidecar"
install -m 0755 "$BIN" /usr/local/bin/flowdmr-sidecar

echo "==> Installing config -> /etc/flowdmr/sidecar.toml (kept if it already exists)"
mkdir -p /etc/flowdmr
if [ ! -f /etc/flowdmr/sidecar.toml ]; then
  /usr/local/bin/flowdmr-sidecar --write-default-config /etc/flowdmr/sidecar.toml
  chown -R flowdmr:flowdmr /etc/flowdmr
  echo "   -> edit /etc/flowdmr/sidecar.toml to set your frequency and injection TG"
fi

echo "==> Installing systemd unit"
install -m 0644 "$HERE/deploy/flowdmr-sidecar.service" /etc/systemd/system/flowdmr-sidecar.service
systemctl daemon-reload
systemctl enable flowdmr-sidecar.service

cat <<EOF

Installed. Start it with:
  sudo systemctl start flowdmr-sidecar
  journalctl -u flowdmr-sidecar -f

Dashboard: http://<this-host>:8081
EOF
