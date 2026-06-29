# FlowDMR — Architecture

FlowDMR injects off-air DMR audio into a FlowStation TETRA cell as a **local
group call**, never touching the Brew/BrandMeister network. This document is the
engineering reference: data flow, the SAP message contract, the wire protocol,
timing, and the local-only guarantee. File/line references point into the
FlowStation tree (`../flowstation`) at the version this was built against.

## 1. Processes & data flow

```
 DMR RF ─▶ RTL-SDR V4 (RX) ─USB─▶ dsd-neo (child of the sidecar)
                                     │  -fs -nm : DMR, MONO 8kHz
                                     ├─ PCM  s16le 8kHz ──▶ UDP 127.0.0.1:23470
                                     └─ console text (Source/Target/CC) ─▶ stdout/stderr
 ┌──────────────────────── flowdmr-sidecar (this repo) ─────────────────────────┐
 │  pcm.rs        : recv UDP PCM, reframe into 240-sample (30 ms) frames         │
 │  decoder.rs    : supervise dsd-neo; read console lines                        │
 │  meta.rs       : regex → {source_id, talkgroup, call_end}                     │
 │  session.rs    : reconcile PCM + metadata → one TETRA call per transmission   │
 │  wire.rs       : encode flowdmr-ipc frames                                    │
 │  dashboard.rs  : http://127.0.0.1:8081  (set RX freq / gain / injection TG)   │
 └───────────────────────────────────┬──────────────────────────────────────────┘
                                      │ flowdmr-ipc over UDP 127.0.0.1:23471
 ┌──────────────────── FlowStation: net_flowdmr entity (flowdmr-entity) ─────────┐
 │  entity.rs Core : CallStart→NetworkCallStart ; Voice→encode→jitter ;          │
 │                   tick→TmdCircuitDataReq ; CallEnd→drain→NetworkCallEnd        │
 │  codec.rs       : PCM 480 → tetra_encode ×2 → 35-byte ACELP TMD block         │
 │  jitter.rs      : adaptive playout, ~56.67 ms/frame                           │
 └───────────────────────────────────┬──────────────────────────────────────────┘
                                      ▼ CMCE group call + UMAC voice → TETRA downlink
```

Two independent SDRs: FlowStation owns its TX SDR (LimeSDR via SoapySDR); FlowDMR
owns a separate RTL-SDR. Different USB devices ⇒ no contention.

## 2. The SAP contract (verified against FlowStation)

Group-call voice injection uses the same CMCE/UMAC primitives Brew uses for
network-originated group calls. Enum: `CallControl`
(`crates/tetra-saps/src/control/call_control.rs:105-123`). Envelope: `SapMsg {
sap, src, dest, msg }` (`crates/tetra-saps/src/sapmsg.rs:143`).

```
 entity ─▶ CMCE   Sap::Control   NetworkCallStart { brew_uuid, source_issi, dest_gssi, priority }
 CMCE   ─▶ entity Sap::Control   NetworkCallReady { brew_uuid, call_id, ts, usage }
 entity ─▶ UMAC   Sap::TmdSap    TmdCircuitDataReq { ts, data:[u8;35] }   (repeat, paced)
 entity ─▶ CMCE   Sap::Control   NetworkCallEnd   { brew_uuid }
```

- `dest_gssi` is the injection TalkGroup; `source_issi` is derived from the DMR
  source id (`source_issi_base + src_id % 1000`).
- The 35-byte `data` is two 137-bit ACELP frames packed into 274 bits — the exact
  layout UMAC consumes (`crates/tetra-entities/src/umac/umac_bs.rs`, and the byte
  layout in `net_asterisk/audio.rs:5-10,179`).
- Talker change within a transmission re-issues `NetworkCallStart` with the new
  `source_issi` (CMCE grants the floor to the new speaker), mirroring
  `net_brew/entity.rs:646-685`.

The default build registers the entity as `TetraEntity::Brew` (the group path
hardcodes that reply target — `group.rs:313`, `isi.rs:881`, `lifecycle.rs:142`).
The `dedicated-entity` feature + the patch in [`../integration/COEXIST.md`](../integration/COEXIST.md)
gives it a `TetraEntity::FlowDmr` identity to coexist with a live Brew.

## 3. Local-only guarantee (three independent layers)

1. **GSSI in `local_ssi_ranges`** — the entity rejects any `target_gssi` not in
   `config().cell.local_ssi_ranges` (`entity.rs` `on_call_start`). FlowStation's
   `is_brew_gssi_routable()` returns `false` for those ranges by construction
   (`net_brew/components/brew_routable.rs:33-57`), so even a misconfigured Brew
   cannot forward the traffic.
2. **No Brew messages** — the entity only ever addresses `Cmce` and `Umac`. It
   never constructs a `SapMsg` to `TetraEntity::Brew` as a *destination*.
3. **Localhost IPC** — the sidecar only ever sends to `127.0.0.1`.

## 4. Wire protocol (`flowdmr-ipc`)

UDP, one datagram per frame, little-endian. Common header (8 bytes): `magic`
`0xFD30` (u16), `version` `1` (u8), `kind` (u8), `stream_id` (u32). Per kind:

| kind | payload |
|---|---|
| `CallStart` (1) | source_id u32, dmr_tg u32, target_gssi u32, priority u8 |
| `Voice` (2)     | seq u32, flags u16, n_samples u16, pcm[n_samples] i16 |
| `CallEnd` (3)   | — |
| `SrcChange` (4) | source_id u32 |
| `Keepalive` (5) | — |

`stream_id` correlates frames to one DMR transmission (new id per transmission).
A `Voice` datagram is 8 + 8 + 480 = 496 bytes (one 30 ms frame), well under MTU.
PCM (not ACELP) is shipped so the patent-sensitive AMBE decode (in dsd-neo) and
the TETRA ACELP encode (in the entity) stay cleanly separated and the native
codec lives in exactly one place.

## 5. Timing & intelligibility

- DMR and TETRA both use **8 kHz** speech sampling — no resampling, just framing.
- A TETRA voice timeslot recurs every TDMA frame ≈ **56.67 ms** and carries one
  35-byte block (two 30 ms ACELP frames = 60 ms of audio). The entity pairs two
  240-sample PCM frames per `tetra_encode` block.
- Voice is paced by the TDMA tick, not a free-running timer: in `tick_start`,
  when `dltime.t == call.ts` and `dltime.f != 18` (frame 18 carries no traffic),
  the entity pops one block from the jitter buffer and emits one
  `TmdCircuitDataReq` — the exact cadence of `BrewEntity::drain_jitter_playout`
  (`net_brew/entity.rs:906-978`).
- The jitter buffer primes ~4 frames (~240 ms) before playout and adapts on
  underrun; on end-of-call it drains remaining frames before `NetworkCallEnd` so
  the tail isn't clipped. Total injected latency ≈ 100–250 ms — fine for one-way.
- Output is a double vocode (AMBE → PCM → ACELP): intelligible, not hi-fi. That
  is the intended trade-off.

## 6. Failure handling

- **No `NetworkCallReady`** (CMCE busy/rejected): the call is reaped after ~5 s so
  it can't leak a timeslot.
- **Sidecar/decoder silence**: the entity force-ends a call after
  `FLOWDMR_IDLE_TIMEOUT_MS` (default 1800 ms) of no voice; the sidecar also ends
  on its own silence watchdog (`silence_timeout_ms`, default 1200 ms).
- **dsd-neo crash**: the supervisor restarts it (and on RF-setting changes from
  the dashboard); status is surfaced on the dashboard and in logs.
- **Voice before metadata**: the sidecar starts a provisional call (source 0) so
  the start of a transmission isn't lost, then upgrades via `SrcChange`.

## 7. Tunables

Entity (env on the bluestation-bs process): `FLOWDMR_LISTEN`,
`FLOWDMR_SOURCE_ISSI_BASE`, `FLOWDMR_IDLE_TIMEOUT_MS`, `FLOWDMR_JITTER_FRAMES`.

Sidecar (`sidecar.toml`): decoder path/mode/extra args, RTL device & bandwidth,
PCM port, entity address, dashboard bind, silence timeout, the metadata regexes,
and the live `rx_freq_hz` / `gain_db` / `ppm` / `injection_tg`.
