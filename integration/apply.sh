#!/usr/bin/env bash
#
# Wire the FlowDMR entity into a FlowStation checkout — the ONLY change FlowDMR
# makes to FlowStation. It adds an optional `flowdmr` feature + path-dependency to
# the bluestation-bs binary and a feature-gated registration block. With the
# feature OFF (the default), FlowStation is byte-for-byte unchanged at runtime.
#
# These edits live only in your LOCAL working tree. Do NOT commit them to the
# public FlowStation repo. `revert.sh` restores the originals.
#
# Usage:
#   ./apply.sh [path-to-flowstation]      (default: ../../flowstation, a sibling)
#
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
FS="${1:-$(cd "$HERE/../.." && pwd)/flowstation}"
CARGO="$FS/bins/bluestation-bs/Cargo.toml"
MAIN="$FS/bins/bluestation-bs/src/main.rs"

[ -f "$CARGO" ] || { echo "ERROR: $CARGO not found — pass the FlowStation path as arg 1"; exit 1; }
[ -f "$MAIN" ]  || { echo "ERROR: $MAIN not found"; exit 1; }

echo "FlowStation: $FS"
cp -n "$CARGO" "$CARGO.flowdmr.bak"
cp -n "$MAIN"  "$MAIN.flowdmr.bak"

# 1) Cargo features (after the asterisk feature line)
if ! grep -q '^flowdmr = ' "$CARGO"; then
  perl -0777 -pi -e 's{(asterisk = \["tetra-entities/asterisk"\]\n)}{$1flowdmr = ["dep:flowdmr-entity"]\nflowdmr-codec-stub = ["flowdmr-entity/codec-stub"]\n}' "$CARGO"
  echo "  + Cargo.toml: features flowdmr, flowdmr-codec-stub"
fi

# 2) Optional path dependency (after the tetra-entities dependency line)
if ! grep -q '^flowdmr-entity = ' "$CARGO"; then
  perl -0777 -pi -e 's{(tetra-entities = \{ workspace = true \}\n)}{$1flowdmr-entity = \{ path = "../../../flowdmr/crates/flowdmr-entity", optional = true, default-features = false, features = ["real-codec"] \}\n}' "$CARGO"
  echo "  + Cargo.toml: optional dep flowdmr-entity"
fi

# 3) use import (after the Brew entity import)
if ! grep -q 'flowdmr_entity::FlowDmrEntity' "$MAIN"; then
  perl -0777 -pi -e 's{(use tetra_entities::net_brew::entity::BrewEntity;\n)}{$1#[cfg(feature = "flowdmr")]\nuse flowdmr_entity::FlowDmrEntity;\n}' "$MAIN"
  echo "  + main.rs: use FlowDmrEntity"
fi

# 4) registration block (immediately before "// Init network time")
if ! grep -q 'FlowDMR local injector' "$MAIN"; then
  perl -0777 -pi -e 's{(\n    // Init network time\n)}{\n    // FlowDMR: register the local DMR->TETRA injector (impersonates Brew).\n    // Disable the Brew interconnect in config when using this default mode.\n    #[cfg(feature = "flowdmr")]\n    \{\n        match FlowDmrEntity::new(cfg.clone()) \{\n            Ok(entity) => \{\n                router.register_entity(Box::new(entity));\n                eprintln!(" -> FlowDMR local injector enabled");\n            \}\n            Err(err) => eprintln!("WARNING: FlowDMR init failed: \{\}", err),\n        \}\n    \}\n$1}' "$MAIN"
  echo "  + main.rs: FlowDmrEntity registration"
fi

cat <<EOF

Done. Build FlowStation with FlowDMR:

  Device (Raspberry Pi, real ACELP codec via pkg-config):
    cargo build --release --features flowdmr

  Dev / no native codec (silent audio — wiring tests only):
    cargo build --features 'flowdmr flowdmr-codec-stub'

Run the entity (inside bluestation-bs) — configure via env if needed:
    FLOWDMR_LISTEN=127.0.0.1:23471 FLOWDMR_SOURCE_ISSI_BASE=9000000 ./bluestation-bs ...

Then start the sidecar (separate process). Revert these edits anytime with:
    $HERE/revert.sh "$FS"
EOF
