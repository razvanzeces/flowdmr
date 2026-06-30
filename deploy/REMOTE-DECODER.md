# Split deployment: remote DMR decoder → BTS injector (over LAN)

The BTS's own TETRA transmitter (e.g. a 30 W PA) desensitises / overloads any
RTL-SDR sitting next to it — no gain or attenuator setting recovers a usable DMR
signal. The fix is to **move the RX off the BTS**: run the decoder on a second
machine that has clean DMR reception, and stream the decoded audio to FlowStation
over the LAN. The FlowDMR IPC is plain UDP, so this needs only address changes.

```
  ┌─────────────────────────────┐         LAN          ┌──────────────────────────┐
  │  Machine B — REMOTE DECODER  │   UDP :23471         │  Machine A — BTS         │
  │  RTL-SDR + dsd-neo           │ ───────────────────► │  FlowStation             │
  │  flowdmr-sidecar             │   FlowDmr frames     │  FlowDMR entity (inject) │
  │  (clean RF location)         │   (audio + src id)   │  LimeSDR + 30 W TETRA TX │
  └─────────────────────────────┘                      └──────────────────────────┘
```

Nothing changes about the local-only guarantee: the audio crosses *your* LAN, is
injected as a LOCAL group call on a `local_ssi_ranges` GSSI, and never touches
Brew.

## Machine A — BTS (FlowStation + entity)

Make the entity listen on all interfaces instead of loopback. Set it in
FlowStation's service environment (systemd) and restart:

```ini
# /etc/systemd/system/<flowstation>.service   →  [Service]
Environment=FLOWDMR_LISTEN=0.0.0.0:23471
```
```sh
sudo systemctl daemon-reload && sudo systemctl restart <flowstation>
# confirm in the log:
#   FlowDmrEntity: listening for sidecar on 0.0.0.0:23471
```

Find A's LAN IP (you'll point the decoder at it):
```sh
ip -4 addr show | grep -oP 'inet \K[\d.]+' | grep -v 127.0.0.1
```

Open the port if a firewall is active:
```sh
sudo ufw allow from 192.168.0.0/16 to any port 23471 proto udp   # adjust subnet
```

No RTL-SDR on this machine. Do **not** run flowdmr-sidecar here.

## Machine B — remote decoder (RTL-SDR + sidecar)

Pick a spot with clean 70 cm reception, well away from the BTS PA. Install the
sidecar + dsd-neo as usual, then point it at A and restart:

```toml
# /etc/flowdmr/sidecar.toml
entity_addr   = "192.168.1.50:23471"   # ← Machine A's LAN IP + FLOWDMR_LISTEN port
dashboard_bind = "0.0.0.0:8081"        # dashboard now lives on the decoder box

[live]
rx_freq_hz  = 439400000
injection_tg = 9                       # a GSSI inside the cell's local_ssi_ranges
```
```sh
sudo systemctl restart flowdmr-sidecar
```

## Verify the link

- Machine A log shows calls arriving: `FlowDmrEntity: DMR call start ...`.
- Machine B dashboard (`http://<B-ip>:8081`) shows the decoder running + the live log.
- Keepalives flow every ~2 s even when idle, so A knows the decoder is connected.

## Notes

- The UDP IPC is unauthenticated — keep it on a trusted LAN (same trust model as
  the dashboard). For an untrusted network, tunnel it (WireGuard) instead of
  exposing 23471.
- Voice frames are < 600 bytes (240 samples), well under the MTU — no
  fragmentation, negligible bandwidth (~16 kB/s during a call).
- Clock/jitter across the LAN is absorbed by the entity's jitter buffer
  (`FLOWDMR_JITTER_FRAMES`, default 2). Raise it a notch if the network is bursty.
