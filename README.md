# FlowDMR

**Local DMR → TETRA injector for [FlowStation](https://github.com/razvanzeces/flowstation).**

FlowDMR receives DMR off-air with a cheap **RX-only RTL-SDR** (e.g. RTL-SDR Blog V4),
decodes the audio and the **transmitter's source ID**, transcodes DMR audio to TETRA
ACELP, and injects it into your FlowStation cell as a **local group call** on a
TalkGroup you choose. It runs as a separate process next to FlowStation on the same
Raspberry Pi.

> ⚠️ **Private — keep out of the public FlowStation repo.** FlowDMR is a separate
> workspace. It must never be committed/pushed to `github.com/razvanzeces/flowstation`.
> The only touch-point is a small, local, **un-committed** patch applied by
> `integration/apply.sh` (revertible with `integration/revert.sh`).

```
  DMR off-air ──▶ RTL-SDR V4 (RX) ──▶ dsd-neo (AMBE→PCM + SRC/TG)
                                          │ PCM(UDP) + metadata
                                ┌─────────▼──────────┐
                                │ flowdmr-sidecar     │  ← mini control dashboard
                                │ (this repo)         │     http://127.0.0.1:8081
                                └─────────┬──────────┘
                                          │ flowdmr-ipc (UDP 127.0.0.1:23471)
                ╔═════════════════════════▼═══════════════════════════════╗
                ║ FlowStation (bluestation-bs --features flowdmr)          ║
                ║   net_flowdmr entity:                                    ║
                ║     PCM → tetra_encode (ACELP) → CMCE local GROUP CALL   ║
                ║     on your TalkGroup → TETRA downlink                    ║
                ║   ╳ GSSI pinned to local_ssi_ranges ⇒ NEVER reaches Brew ║
                ╚══════════════════════════════════════════════════════════╝
```

It will **not** sound like native TETRA (it's a double-vocode AMBE→PCM→ACELP), but it
is intelligible — exactly the design goal.

---

## Crates

| Crate | What it is |
|---|---|
| `flowdmr-ipc` | The shared wire protocol (the "plugin contract") used by both sides. Zero deps. |
| `flowdmr-entity` | The FlowStation-side `TetraEntityTrait` injector. Links the native `tetra-codec`. |
| `flowdmr-sidecar` | The standalone binary: drives dsd-neo, ingests PCM, parses metadata, serves the dashboard. |

## Repository layout assumption

FlowDMR expects to sit **next to** your FlowStation checkout:

```
TETRA/
├── flowstation/      (public repo — github.com/razvanzeces/flowstation)
└── flowdmr/          (this repo — PRIVATE)
```

`flowdmr-entity` path-depends on `../flowstation/crates/*`. If your FlowStation lives
elsewhere, edit the paths in `crates/flowdmr-entity/Cargo.toml`.

---

## Quick start (dev, on your laptop — no SDR, no codec)

```sh
# Build + test everything except the entity's native codec:
cargo test -p flowdmr-ipc
cargo test -p flowdmr-sidecar
cargo test -p flowdmr-entity --no-default-features --features codec-stub

# Run the sidecar with the built-in defaults and open the dashboard:
cargo run -p flowdmr-sidecar -- --write-default-config sidecar.toml
cargo run -p flowdmr-sidecar -- --config sidecar.toml
#   → http://127.0.0.1:8081   (dsd-neo will be "stopped" if not installed)
```

## Deploy (Raspberry Pi)

1. **RTL-SDR V4 driver** (the blog fork is mandatory for V4):
   ```sh
   sudo ./deploy/install-rtlsdr-v4.sh
   ```
2. **dsd-neo** (the DMR decoder; builds mbelib — read the AMBE patent note it prints):
   ```sh
   sudo ./deploy/install-dsd-neo.sh
   ```
3. **Patch + build FlowStation with FlowDMR** (real ACELP codec via pkg-config):
   ```sh
   ./integration/apply.sh /path/to/flowstation
   cd /path/to/flowstation && cargo build --release --features flowdmr
   ```
4. **Configure** a local TalkGroup. The injection GSSI **must** be inside the cell's
   `local_ssi_ranges` in FlowStation's config — that is what guarantees the traffic
   never reaches Brew. Example FlowStation config:
   ```toml
   [cell]
   local_ssi_ranges = [[5000, 5999]]   # FlowDMR injects on 5000 by default
   # Using FlowDMR's default (Brew-impersonation) mode? Disable Brew:
   # remove/disable the [brew] section.
   ```
5. **Entity tuning** (optional, via env on the bluestation-bs process):
   `FLOWDMR_LISTEN` (default `127.0.0.1:23471`), `FLOWDMR_SOURCE_ISSI_BASE`
   (default `9000000`), `FLOWDMR_IDLE_TIMEOUT_MS` (default `1800`),
   `FLOWDMR_JITTER_FRAMES` (default `2`).
6. **Run the sidecar:**
   ```sh
   ./deploy/install-sidecar-service.sh        # installs + enables systemd unit
   sudo systemctl start flowdmr-sidecar
   ```
   Open `http://<pi>:8081` to set the RX frequency, gain and injection TalkGroup.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full design, the SAP message
sequence, the wire protocol, and the timing model.

---

## Two integration modes

- **Default — Brew impersonation** (smallest footprint): the entity registers as
  `TetraEntity::Brew`. **You must not run the real Brew interconnect at the same time.**
  Perfect for a purely local cell. No FlowStation protocol code is touched.
- **Coexist with Brew** (advanced): build with `--features flowdmr` **and**
  `flowdmr-entity/dedicated-entity`, after applying the extra patch described in
  [`integration/COEXIST.md`](integration/COEXIST.md). Lets FlowDMR and a live Brew run
  together by giving FlowDMR its own `TetraEntity::FlowDmr` identity.

## Legal / licensing

- **AMBE+2** (DMR's vocoder) is patent-encumbered. `mbelib` software decoding is widely
  used for **personal, receive-only** experimentation but shipping it in a product is
  patent-exposed. Confirm your intended use; consider a licensed hardware vocoder
  (ThumbDV/DV3000) if needed.
- **Re-transmitting** decoded off-air traffic on a TETRA downlink may engage interception
  / secrecy-of-communications and TX-authorization rules that vary by country. You are
  responsible for operating only on frequencies and content you are authorized to use.
