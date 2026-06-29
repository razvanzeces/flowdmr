#!/usr/bin/env bash
#
# Undo apply.sh — restore the original bluestation-bs Cargo.toml and main.rs.
#
# Usage:
#   ./revert.sh [path-to-flowstation]      (default: ../../flowstation)
#
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
FS="${1:-$(cd "$HERE/../.." && pwd)/flowstation}"
CARGO="$FS/bins/bluestation-bs/Cargo.toml"
MAIN="$FS/bins/bluestation-bs/src/main.rs"

restored=0
for f in "$CARGO" "$MAIN"; do
  if [ -f "$f.flowdmr.bak" ]; then
    mv -f "$f.flowdmr.bak" "$f"
    echo "restored $f"
    restored=1
  fi
done

if [ "$restored" = 0 ]; then
  echo "No .flowdmr.bak files found — nothing to revert (or already reverted)."
fi
