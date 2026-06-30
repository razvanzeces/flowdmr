#!/usr/bin/env bash
#
# Install flowdmr-sidecar as a systemd service (runs as root, like tetra.service).
# Run with sudo from the flowdmr repo root AFTER:
#   cargo build --release -p flowdmr-sidecar
#
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

HERE="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$HERE/target/release/flowdmr-sidecar"
[ -x "$BIN" ] || { echo "build first: cargo build --release -p flowdmr-sidecar"; exit 1; }

echo "==> Installing binary -> /usr/local/bin/flowdmr-sidecar"
install -m 0755 "$BIN" /usr/local/bin/flowdmr-sidecar

echo "==> Config -> /etc/flowdmr/sidecar.toml  +  logs -> /var/log/flowdmr/"
mkdir -p /etc/flowdmr /var/log/flowdmr
if [ ! -f /etc/flowdmr/sidecar.toml ]; then
  /usr/local/bin/flowdmr-sidecar --write-default-config /etc/flowdmr/sidecar.toml
  echo "   -> EDIT /etc/flowdmr/sidecar.toml: set [live] rx_freq_hz, injection_tg, gain_db"
else
  echo "   -> keeping existing /etc/flowdmr/sidecar.toml"
fi

echo "==> systemd unit"
install -m 0644 "$HERE/deploy/flowdmr-sidecar.service" /etc/systemd/system/flowdmr-sidecar.service
systemctl daemon-reload
systemctl enable flowdmr-sidecar.service

cat <<EOF

Installed. Edit the config, then:
  sudo systemctl restart flowdmr-sidecar
  journalctl -u flowdmr-sidecar -f          # or: tail -f /var/log/flowdmr/flowdmr.log

Dashboard (with live decoder log + audio meter):  http://<this-host>:8081
EOF
