#!/usr/bin/env bash
#
# RF diagnostic: sweep a band with rtl_power and render a waterfall PNG, so you
# can SEE what's on the target frequency and whether a strong out-of-band signal
# (e.g. the BTS TX) is raising the noise floor / overloading the front end.
#
# Frees the RTL-SDR by stopping flowdmr-sidecar for the duration, then restarts
# it. Run on the DECODER box (Machine B). Needs: rtl_power (rtl-sdr-blog),
# python3-numpy, python3-matplotlib.
#
# Usage:
#   ./rtl-waterfall.sh <center_mhz> [span_khz] [seconds] [gain]
#     center_mhz  target, e.g. 172.0375
#     span_khz    total width scanned  (default 1000 = +/-500 kHz)
#     seconds     capture duration     (default 30)
#     gain        tuner gain dB        (default 20)
#
# Examples:
#   ./rtl-waterfall.sh 172.0375                 # narrow look at the target
#   ./rtl-waterfall.sh 172.0375 4000 60         # +/-2 MHz, one minute
#   ./rtl-waterfall.sh 172.0 20000 30 5         # wide 20 MHz VHF sweep, low gain
set -euo pipefail

CENTER="${1:?usage: rtl-waterfall.sh <center_mhz> [span_khz] [seconds] [gain]}"
SPAN_KHZ="${2:-1000}"
DURATION="${3:-30}"
GAIN="${4:-20}"

OUT_DIR="${OUT_DIR:-/tmp}"
CSV="$OUT_DIR/rtl_scan.csv"
PNG="$OUT_DIR/waterfall.png"

command -v rtl_power >/dev/null || { echo "ERROR: rtl_power not found (install-rtlsdr-v4.sh)"; exit 1; }
command -v python3   >/dev/null || { echo "ERROR: python3 not found"; exit 1; }

LOW=$(python3 -c "print(f'{($CENTER*1e6 - $SPAN_KHZ*1e3/2)/1e6:.6f}M')")
HIGH=$(python3 -c "print(f'{($CENTER*1e6 + $SPAN_KHZ*1e3/2)/1e6:.6f}M')")
BIN="1k"

STOPPED=0
if systemctl is-active --quiet flowdmr-sidecar 2>/dev/null; then
  echo "==> stopping flowdmr-sidecar to free the RTL-SDR"
  systemctl stop flowdmr-sidecar
  STOPPED=1
fi
restart_sidecar() {
  [ "$STOPPED" = 1 ] && { echo "==> restarting flowdmr-sidecar"; systemctl start flowdmr-sidecar || true; }
}
trap restart_sidecar EXIT

echo "==> rtl_power  ${LOW}..${HIGH}  bin=${BIN}  gain=${GAIN}dB  ${DURATION}s"
rtl_power -f "${LOW}:${HIGH}:${BIN}" -g "$GAIN" -i 1 -e "$DURATION" "$CSV"

echo "==> rendering waterfall"
python3 "$(dirname "$0")/rtl_waterfall.py" "$CSV" "$PNG" "$CENTER"

cat <<EOF

Done.
  Waterfall PNG : $PNG
  Raw CSV       : $CSV

Copy the PNG to your Mac to view it:
  scp root@<B-ip>:$PNG ~/Downloads/
EOF
