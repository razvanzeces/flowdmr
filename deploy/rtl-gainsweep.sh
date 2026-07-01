#!/usr/bin/env bash
#
# RF diagnostic: step the tuner gain and, at each step, report the noise floor
# and the strongest bin on the target band. Tells you whether a REAL signal is
# present (rises well above the floor, at a stable frequency, peaking at some
# gain) or the band is just noise (floor + peak scale together, peak wanders),
# and roughly where the front end starts to overload (floor stops rising / dips).
#
# Run on the decoder box (Machine B). Frees the RTL by stopping flowdmr-sidecar,
# then restarts it. Needs rtl_power (rtl-sdr-blog) + python3.
#
# Usage:
#   ./rtl-gainsweep.sh <center_mhz> [span_khz] [seconds_each] [gains...]
#     center_mhz    e.g. 172.0375
#     span_khz      width scanned per step (default 200)
#     seconds_each  capture per gain       (default 6)
#     gains...      gain list dB           (default: 0 6 12 20 30 40 49)
#
# Examples:
#   ./rtl-gainsweep.sh 172.0375                 # target, default gains
#   ./rtl-gainsweep.sh 98 400 5                 # FM reference (should show a big peak)
#   ./rtl-gainsweep.sh 172.0375 200 8 0 10 20 30 40
set -euo pipefail

CENTER="${1:?usage: rtl-gainsweep.sh <center_mhz> [span_khz] [seconds] [gains...]}"
SPAN_KHZ="${2:-200}"
DUR="${3:-6}"
if [ "$#" -gt 3 ]; then shift 3; GAINS=("$@"); else GAINS=(0 6 12 20 30 40 49); fi

command -v rtl_power >/dev/null || { echo "ERROR: rtl_power not found (install-rtlsdr-v4.sh)"; exit 1; }
command -v python3   >/dev/null || { echo "ERROR: python3 not found"; exit 1; }

LOW=$(python3 -c "print(f'{($CENTER*1e6-$SPAN_KHZ*1e3/2)/1e6:.6f}M')")
HIGH=$(python3 -c "print(f'{($CENTER*1e6+$SPAN_KHZ*1e3/2)/1e6:.6f}M')")

STOPPED=0
if systemctl is-active --quiet flowdmr-sidecar 2>/dev/null; then
  echo "==> stopping flowdmr-sidecar to free the RTL-SDR"
  systemctl stop flowdmr-sidecar
  STOPPED=1
fi
trap '[ "$STOPPED" = 1 ] && { echo "==> restarting flowdmr-sidecar"; systemctl start flowdmr-sidecar || true; }' EXIT

echo "==> gain sweep on ${CENTER} MHz (${LOW}..${HIGH}), ${DUR}s each"
printf "%-6s %-10s %-12s %-9s\n" "gain" "floor_dB" "peak_MHz" "peak+dB"
printf -- "-----------------------------------------\n"
for g in "${GAINS[@]}"; do
  csv="/tmp/gsweep_${g}.csv"
  rtl_power -f "${LOW}:${HIGH}:1k" -g "$g" -i 1 -e "$DUR" "$csv" >/dev/null 2>&1 || true
  python3 - "$csv" "$g" <<'PY'
import sys, statistics
csv, g = sys.argv[1], sys.argv[2]
bins = {}
try:
    for line in open(csv):
        p = [x.strip() for x in line.split(",")]
        if len(p) < 7:
            continue
        try:
            lo = float(p[2]); st = float(p[4]); dbs = [float(x) for x in p[6:]]
        except ValueError:
            continue
        for i, db in enumerate(dbs):
            bins.setdefault(lo + i * st, []).append(db)
except FileNotFoundError:
    bins = {}
if not bins:
    print(f"{g:<6} (no data)"); sys.exit()
med = {f: statistics.median(v) for f, v in bins.items()}
floor = statistics.median(med.values())
pf = max(med, key=lambda f: med[f])
print(f"{g:<6} {floor:<10.1f} {pf/1e6:<12.4f} {med[pf]-floor:<9.1f}")
PY
done

cat <<'EOF'

How to read it:
  * A REAL signal: peak+dB grows to +20..+40 and stays on the SAME peak_MHz.
  * Just noise:    peak+dB stays small (~+3..+6) and peak_MHz wanders each row.
  * Overload:      floor_dB stops climbing (or dips) as gain rises past the knee.
EOF
