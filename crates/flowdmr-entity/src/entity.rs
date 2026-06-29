//! The FlowStation-side injection entity.
//!
//! Receives decoded DMR audio + metadata from the FlowDMR sidecar over UDP,
//! transcodes PCM -> TETRA ACELP, and originates a LOCAL group call on a
//! configured TalkGroup (GSSI). It speaks only the CMCE group-call contract
//! (`NetworkCallStart` / `NetworkCallReady` / voice via `TmdCircuitDataReq` /
//! `NetworkCallEnd`) and the downlink — it NEVER sends anything to Brew.
//!
//! Local-only guarantee: every requested `target_gssi` is checked against the
//! cell's `local_ssi_ranges`; a GSSI outside those ranges is refused, so traffic
//! can never be routed to Brew (`is_brew_gssi_routable` returns false for local
//! ranges by construction).
//!
//! The pure call-management logic lives in [`Core`], decoupled from the UDP
//! socket and `SharedConfig` so the full SAP sequence is unit-testable without
//! hardware or the proprietary codec.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use uuid::Uuid;

use tetra_config::bluestation::SharedConfig;
use tetra_core::{Sap, TdmaTime, tetra_entities::TetraEntity};
use tetra_entities::{MessageQueue, TetraEntityTrait};
use tetra_saps::{SapMsg, SapMsgInner, control::call_control::CallControl, tmd::TmdCircuitDataReq};

use flowdmr_ipc::{FlowDmrBody, FlowDmrFrame, PCM_SAMPLES_PER_FRAME};

use crate::codec::{Vocoder, TMD_PACKED_BYTES};
use crate::jitter::VoiceJitterBuffer;

/// The TETRA entity identity this injector registers under.
///
/// Default build impersonates `Brew`: the smallest possible FlowStation patch
/// (no protocol-code changes). With this identity, the real Brew interconnect
/// must be disabled — consistent with the local-only requirement. Enable the
/// `dedicated-entity` feature (plus the coexist patch) to register as a
/// dedicated `TetraEntity::FlowDmr` and run alongside a live Brew.
#[cfg(not(feature = "dedicated-entity"))]
const SELF_ENTITY: TetraEntity = TetraEntity::Brew;
#[cfg(feature = "dedicated-entity")]
const SELF_ENTITY: TetraEntity = TetraEntity::FlowDmr;

/// If resources are never allocated (no `NetworkCallReady`), reap the stuck call
/// after this long so it can't leak.
const NO_RESOURCE_REAP: Duration = Duration::from_secs(5);

const SAMPLES_PER_BLOCK: usize = PCM_SAMPLES_PER_FRAME * 2; // 480 = 60 ms

/// Tunables read from the environment (keeps FlowStation's config untouched).
#[derive(Clone)]
struct Settings {
    listen: String,
    source_issi_base: u32,
    idle_timeout: Duration,
    jitter_initial_latency_frames: usize,
}

impl Settings {
    fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let parse = |k: &str, default: u64| env(k).and_then(|v| v.parse().ok()).unwrap_or(default);
        Self {
            listen: env("FLOWDMR_LISTEN").unwrap_or_else(|| "127.0.0.1:23471".to_string()),
            source_issi_base: parse("FLOWDMR_SOURCE_ISSI_BASE", 9_000_000) as u32,
            idle_timeout: Duration::from_millis(parse("FLOWDMR_IDLE_TIMEOUT_MS", 1800)),
            jitter_initial_latency_frames: parse("FLOWDMR_JITTER_FRAMES", 2) as usize,
        }
    }
}

/// One active injected call (keyed by its `brew_uuid` in `Core::calls`).
struct ActiveCall {
    source_issi: u32,
    dest_gssi: u32,
    priority: u8,
    /// Set once CMCE replies with `NetworkCallReady`.
    ts: Option<u8>,
    /// When the call was created (for the no-resource reaper).
    created_at: Instant,
    vocoder: Vocoder,
    jitter: VoiceJitterBuffer,
    pending_pcm: Vec<i16>,
    last_voice_at: Instant,
    /// True once a `CallEnd`/timeout was observed: drain remaining audio, then release.
    ending: bool,
}

/// Pure call-management core: turns IPC frames + ticks into SAP messages.
struct Core {
    settings: Settings,
    dltime: TdmaTime,
    stream_to_uuid: HashMap<u32, Uuid>,
    calls: HashMap<Uuid, ActiveCall>,
    decoded_calls: u64,
}

impl Core {
    fn new(settings: Settings) -> Self {
        Self {
            settings,
            dltime: TdmaTime::default(),
            stream_to_uuid: HashMap::new(),
            calls: HashMap::new(),
            decoded_calls: 0,
        }
    }

    fn map_source_issi(&self, source_id: u32) -> u32 {
        self.settings.source_issi_base.saturating_add(source_id % 1000)
    }

    fn handle_frame(&mut self, frame: FlowDmrFrame, is_local: &dyn Fn(u32) -> bool, queue: &mut MessageQueue) {
        let stream_id = frame.stream_id;
        match frame.body {
            FlowDmrBody::CallStart { source_id, dmr_tg, target_gssi, priority } => {
                self.on_call_start(queue, is_local, stream_id, source_id, dmr_tg, target_gssi, priority)
            }
            FlowDmrBody::Voice { seq, flags, pcm } => self.on_voice(stream_id, seq, flags, pcm),
            FlowDmrBody::SrcChange { source_id } => self.on_src_change(queue, stream_id, source_id),
            FlowDmrBody::CallEnd => self.on_call_end(stream_id),
            FlowDmrBody::Keepalive => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_call_start(
        &mut self,
        queue: &mut MessageQueue,
        is_local: &dyn Fn(u32) -> bool,
        stream_id: u32,
        source_id: u32,
        dmr_tg: u32,
        target_gssi: u32,
        priority: u8,
    ) {
        // ── LOCAL-ONLY GUARD ──────────────────────────────────────────────
        if !is_local(target_gssi) {
            tracing::warn!(
                "FlowDmrEntity: REFUSING injection on GSSI {} — not in cell.local_ssi_ranges \
                 (would be Brew-routable). DMR src={} tg={} dropped.",
                target_gssi,
                source_id,
                dmr_tg
            );
            return;
        }

        // Restart semantics: if a call already exists for this stream, end it.
        if let Some(old) = self.stream_to_uuid.remove(&stream_id) {
            self.calls.remove(&old);
            queue.push_back(end_msg(old));
        }

        let Some(vocoder) = Vocoder::new() else {
            tracing::error!("FlowDmrEntity: vocoder allocation failed; dropping DMR call src={source_id}");
            return;
        };
        let uuid = Uuid::new_v4();
        let source_issi = self.map_source_issi(source_id);
        let now = Instant::now();
        self.calls.insert(
            uuid,
            ActiveCall {
                source_issi,
                dest_gssi: target_gssi,
                priority,
                ts: None,
                created_at: now,
                vocoder,
                jitter: VoiceJitterBuffer::with_initial_latency(self.settings.jitter_initial_latency_frames),
                pending_pcm: Vec::with_capacity(SAMPLES_PER_BLOCK),
                last_voice_at: now,
                ending: false,
            },
        );
        self.stream_to_uuid.insert(stream_id, uuid);
        self.decoded_calls += 1;

        tracing::info!(
            "FlowDmrEntity: DMR call start src_id={} -> ISSI {} on GSSI {} (dmr_tg={}, uuid={})",
            source_id,
            source_issi,
            target_gssi,
            dmr_tg,
            uuid
        );
        queue.push_back(SapMsg::new(
            Sap::Control,
            SELF_ENTITY,
            TetraEntity::Cmce,
            SapMsgInner::CmceCallControl(CallControl::NetworkCallStart {
                brew_uuid: uuid,
                source_issi,
                dest_gssi: target_gssi,
                priority,
            }),
        ));
    }

    fn on_voice(&mut self, stream_id: u32, _seq: u32, _flags: u16, pcm: Vec<i16>) {
        let Some(&uuid) = self.stream_to_uuid.get(&stream_id) else {
            tracing::trace!("FlowDmrEntity: voice for unknown stream_id={stream_id} (no CallStart) — dropped");
            return;
        };
        if pcm.len() != PCM_SAMPLES_PER_FRAME {
            tracing::trace!("FlowDmrEntity: voice frame wrong size {} (want {})", pcm.len(), PCM_SAMPLES_PER_FRAME);
            return;
        }
        let Some(call) = self.calls.get_mut(&uuid) else { return };
        call.last_voice_at = Instant::now();
        call.pending_pcm.extend_from_slice(&pcm);

        while call.pending_pcm.len() >= SAMPLES_PER_BLOCK {
            let mut a = [0i16; PCM_SAMPLES_PER_FRAME];
            let mut b = [0i16; PCM_SAMPLES_PER_FRAME];
            a.copy_from_slice(&call.pending_pcm[..PCM_SAMPLES_PER_FRAME]);
            b.copy_from_slice(&call.pending_pcm[PCM_SAMPLES_PER_FRAME..SAMPLES_PER_BLOCK]);
            call.pending_pcm.drain(..SAMPLES_PER_BLOCK);
            let block: [u8; TMD_PACKED_BYTES] = call.vocoder.encode_block(&a, &b);
            call.jitter.push(block.to_vec());
        }
    }

    fn on_src_change(&mut self, queue: &mut MessageQueue, stream_id: u32, source_id: u32) {
        let Some(&uuid) = self.stream_to_uuid.get(&stream_id) else { return };
        let new_issi = self.map_source_issi(source_id);
        let Some(call) = self.calls.get_mut(&uuid) else { return };
        if call.source_issi == new_issi {
            return;
        }
        let dropped = call.jitter.flush();
        call.source_issi = new_issi;
        call.pending_pcm.clear();
        let (dest_gssi, priority) = (call.dest_gssi, call.priority);
        tracing::info!("FlowDmrEntity: talker change uuid={} -> ISSI {} (flushed {} frames)", uuid, new_issi, dropped);
        queue.push_back(SapMsg::new(
            Sap::Control,
            SELF_ENTITY,
            TetraEntity::Cmce,
            SapMsgInner::CmceCallControl(CallControl::NetworkCallStart {
                brew_uuid: uuid,
                source_issi: new_issi,
                dest_gssi,
                priority,
            }),
        ));
    }

    fn on_call_end(&mut self, stream_id: u32) {
        if let Some(&uuid) = self.stream_to_uuid.get(&stream_id) {
            if let Some(call) = self.calls.get_mut(&uuid) {
                call.ending = true; // drained + released in playout
                tracing::info!("FlowDmrEntity: DMR call end signalled uuid={} (draining)", uuid);
            }
        }
    }

    /// Pace voice into the downlink, one ACELP block per matching timeslot.
    /// Mirrors `BrewEntity::drain_jitter_playout`: frame 18 carries no traffic,
    /// and a call only plays on its own allocated timeslot.
    fn playout(&mut self, queue: &mut MessageQueue) {
        if self.dltime.f == 18 {
            return;
        }
        let cur_ts = self.dltime.t;
        let now = Instant::now();
        let uuid_to_stream: HashMap<Uuid, u32> = self.stream_to_uuid.iter().map(|(s, u)| (*u, *s)).collect();
        let mut sends: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut to_release: Vec<(u32, Uuid)> = Vec::new();

        for (uuid, call) in self.calls.iter_mut() {
            let stream = uuid_to_stream.get(uuid).copied();
            let Some(ts) = call.ts else {
                // Resources not yet allocated; reap if ending or stuck too long.
                if call.ending || now.duration_since(call.created_at) > NO_RESOURCE_REAP {
                    if let Some(s) = stream {
                        to_release.push((s, *uuid));
                    }
                }
                continue;
            };

            if !call.ending && now.duration_since(call.last_voice_at) > self.settings.idle_timeout {
                call.ending = true;
                tracing::info!("FlowDmrEntity: idle timeout uuid={} — ending call", uuid);
            }

            if ts != cur_ts {
                continue;
            }

            if call.ending {
                match call.jitter.pop_drain() {
                    Some(block) => sends.push((ts, block)),
                    None => {
                        if let Some(s) = stream {
                            to_release.push((s, *uuid));
                        }
                    }
                }
            } else if let Some(block) = call.jitter.pop_ready() {
                sends.push((ts, block));
            }
        }

        for (ts, data) in sends {
            queue.push_back(SapMsg::new(
                Sap::TmdSap,
                SELF_ENTITY,
                TetraEntity::Umac,
                SapMsgInner::TmdCircuitDataReq(TmdCircuitDataReq { ts, data }),
            ));
        }
        for (stream_id, uuid) in to_release {
            self.stream_to_uuid.remove(&stream_id);
            self.calls.remove(&uuid);
            tracing::info!("FlowDmrEntity: released call uuid={}", uuid);
            queue.push_back(end_msg(uuid));
        }
    }

    fn on_network_call_ready(&mut self, queue: &mut MessageQueue, brew_uuid: Uuid, call_id: u16, ts: u8, usage: u8) {
        if let Some(call) = self.calls.get_mut(&brew_uuid) {
            call.ts = Some(ts);
            tracing::info!("FlowDmrEntity: call ready uuid={} call_id={} ts={} usage={}", brew_uuid, call_id, ts, usage);
        } else {
            tracing::warn!("FlowDmrEntity: NetworkCallReady for unknown uuid={} — releasing orphan", brew_uuid);
            queue.push_back(end_msg(brew_uuid));
        }
    }

    fn on_network_call_end(&mut self, brew_uuid: Uuid) {
        if self.calls.remove(&brew_uuid).is_some() {
            self.stream_to_uuid.retain(|_, u| *u != brew_uuid);
            tracing::info!("FlowDmrEntity: call ended by CMCE uuid={}", brew_uuid);
        }
    }
}

fn end_msg(brew_uuid: Uuid) -> SapMsg {
    SapMsg::new(
        Sap::Control,
        SELF_ENTITY,
        TetraEntity::Cmce,
        SapMsgInner::CmceCallControl(CallControl::NetworkCallEnd { brew_uuid }),
    )
}

/// FlowStation entity: owns the IPC socket + config and delegates logic to [`Core`].
pub struct FlowDmrEntity {
    config: SharedConfig,
    rx: Receiver<FlowDmrFrame>,
    core: Core,
    #[allow(dead_code)]
    last_frame_at: Option<Instant>,
}

impl FlowDmrEntity {
    /// Bind the IPC socket and spawn the receive thread. Mirrors the
    /// `BrewEntity::new(cfg, …)` construction pattern used in `main.rs`.
    pub fn new(config: SharedConfig) -> std::io::Result<Self> {
        let settings = Settings::from_env();
        let socket = UdpSocket::bind(&settings.listen)?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        tracing::info!(
            "FlowDmrEntity: listening for sidecar on {} (identity={:?}, source_issi_base={})",
            settings.listen,
            SELF_ENTITY,
            settings.source_issi_base
        );

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("flowdmr-ipc-rx".into()).spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((len, _addr)) => match FlowDmrFrame::decode(&buf[..len]) {
                        Ok(frame) => {
                            if tx.send(frame).is_err() {
                                return; // entity dropped
                            }
                        }
                        Err(err) => tracing::trace!("FlowDmrEntity: bad IPC frame: {err}"),
                    },
                    Err(err)
                        if err.kind() == std::io::ErrorKind::WouldBlock
                            || err.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(err) => {
                        tracing::warn!("FlowDmrEntity: IPC socket recv error: {err}");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })?;

        Ok(Self { config, rx, core: Core::new(settings), last_frame_at: None })
    }
}

impl TetraEntityTrait for FlowDmrEntity {
    fn entity(&self) -> TetraEntity {
        SELF_ENTITY
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        match message.msg {
            SapMsgInner::CmceCallControl(CallControl::NetworkCallReady { brew_uuid, call_id, ts, usage }) => {
                self.core.on_network_call_ready(queue, brew_uuid, call_id, ts, usage)
            }
            SapMsgInner::CmceCallControl(CallControl::NetworkCallEnd { brew_uuid }) => {
                self.core.on_network_call_end(brew_uuid)
            }
            // Ignore everything else (impersonating Brew may deliver unrelated control here).
            _ => {}
        }
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        self.core.dltime = ts;
        // Split borrows so the local-only closure can read config while core mutates.
        let Self { config, rx, core, last_frame_at } = self;
        while let Ok(frame) = rx.try_recv() {
            *last_frame_at = Some(Instant::now());
            let is_local = |g: u32| config.config().cell.local_ssi_ranges.contains(g);
            core.handle_frame(frame, &is_local, queue);
        }
        core.playout(queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            listen: "127.0.0.1:0".into(),
            source_issi_base: 9_000_000,
            idle_timeout: Duration::from_millis(1800),
            jitter_initial_latency_frames: 0, // release immediately for deterministic tests
        }
    }

    // Pull every queued SapMsg out for assertions.
    fn drain(queue: &mut MessageQueue) -> Vec<SapMsg> {
        let mut out = Vec::new();
        while let Some(m) = queue.pop_front() {
            out.push(m);
        }
        out
    }

    fn local(g: u32) -> bool {
        (5000..=5999).contains(&g)
    }

    fn voice_frame(stream: u32, seq: u32) -> FlowDmrFrame {
        FlowDmrFrame::new(stream, FlowDmrBody::Voice { seq, flags: 0, pcm: vec![0i16; PCM_SAMPLES_PER_FRAME] })
    }

    #[test]
    fn call_start_emits_network_call_start_to_cmce() {
        let mut core = Core::new(settings());
        let mut q = MessageQueue::new();
        core.handle_frame(
            FlowDmrFrame::new(1, FlowDmrBody::CallStart { source_id: 2_604_123, dmr_tg: 9, target_gssi: 5000, priority: 0 }),
            &local,
            &mut q,
        );
        let msgs = drain(&mut q);
        assert_eq!(msgs.len(), 1);
        match &msgs[0].msg {
            SapMsgInner::CmceCallControl(CallControl::NetworkCallStart { source_issi, dest_gssi, .. }) => {
                assert_eq!(*dest_gssi, 5000);
                assert_eq!(*source_issi, 9_000_000 + 123); // base + src%1000
            }
            other => panic!("expected NetworkCallStart, got {other:?}"),
        }
        assert_eq!(*msgs[0].get_dest(), TetraEntity::Cmce);
        assert_eq!(*msgs[0].get_source(), SELF_ENTITY);
    }

    #[test]
    fn non_local_gssi_is_refused() {
        let mut core = Core::new(settings());
        let mut q = MessageQueue::new();
        core.handle_frame(
            FlowDmrFrame::new(1, FlowDmrBody::CallStart { source_id: 1, dmr_tg: 9, target_gssi: 1234, priority: 0 }),
            &local, // 1234 is NOT in 5000..=5999
            &mut q,
        );
        assert!(drain(&mut q).is_empty(), "non-local GSSI must emit nothing (never reaches Brew)");
        assert!(core.calls.is_empty());
    }

    #[test]
    fn full_sequence_start_voice_end() {
        let mut core = Core::new(settings());
        let mut q = MessageQueue::new();

        // 1) CallStart -> NetworkCallStart
        core.handle_frame(
            FlowDmrFrame::new(7, FlowDmrBody::CallStart { source_id: 5, dmr_tg: 9, target_gssi: 5001, priority: 0 }),
            &local,
            &mut q,
        );
        let uuid = match &drain(&mut q)[0].msg {
            SapMsgInner::CmceCallControl(CallControl::NetworkCallStart { brew_uuid, .. }) => *brew_uuid,
            _ => panic!("no NetworkCallStart"),
        };

        // 2) CMCE allocates resources on ts=2.
        core.on_network_call_ready(&mut q, uuid, 0x100, 2, 0);
        assert!(drain(&mut q).is_empty());

        // 3) Feed enough voice to clear the jitter priming threshold.
        // 10 × 30ms frames = 5 × 60ms ACELP blocks (>= BASE_FRAMES priming depth).
        for seq in 0..10 {
            core.on_voice(7, seq, 0, vec![0i16; PCM_SAMPLES_PER_FRAME]);
        }

        // 4) Playout on the matching timeslot (jitter latency 0) yields TmdCircuitDataReq.
        core.dltime = TdmaTime { t: 2, f: 1, m: 1, h: 0 };
        core.playout(&mut q);
        let msgs = drain(&mut q);
        assert!(
            msgs.iter().any(|m| matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_)) && *m.get_dest() == TetraEntity::Umac),
            "expected a TmdCircuitDataReq to UMAC, got {msgs:?}"
        );
        if let Some(SapMsg { msg: SapMsgInner::TmdCircuitDataReq(req), .. }) =
            msgs.iter().find(|m| matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_)))
        {
            assert_eq!(req.ts, 2);
            assert_eq!(req.data.len(), TMD_PACKED_BYTES); // 35-byte ACELP block
        }

        // 5) Wrong timeslot emits nothing.
        core.dltime = TdmaTime { t: 3, f: 1, m: 1, h: 0 };
        core.playout(&mut q);
        assert!(drain(&mut q).iter().all(|m| !matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_))));

        // 6) Frame 18 carries no traffic.
        core.dltime = TdmaTime { t: 2, f: 18, m: 1, h: 0 };
        core.on_voice(7, 99, 0, vec![0i16; PCM_SAMPLES_PER_FRAME]);
        core.on_voice(7, 100, 0, vec![0i16; PCM_SAMPLES_PER_FRAME]);
        core.playout(&mut q);
        assert!(drain(&mut q).is_empty(), "frame 18 must not play out");

        // 7) CallEnd -> drain remaining -> NetworkCallEnd, then call removed.
        core.on_call_end(7);
        core.dltime = TdmaTime { t: 2, f: 1, m: 1, h: 0 };
        let mut saw_end = false;
        for _ in 0..50 {
            core.playout(&mut q);
            for m in drain(&mut q) {
                if matches!(m.msg, SapMsgInner::CmceCallControl(CallControl::NetworkCallEnd { .. })) {
                    saw_end = true;
                }
            }
            if core.calls.is_empty() {
                break;
            }
        }
        assert!(saw_end, "expected NetworkCallEnd after draining");
        assert!(core.calls.is_empty(), "call must be removed after end");
    }

    #[test]
    fn talker_change_reissues_call_start_with_new_issi() {
        let mut core = Core::new(settings());
        let mut q = MessageQueue::new();
        core.handle_frame(
            FlowDmrFrame::new(3, FlowDmrBody::CallStart { source_id: 10, dmr_tg: 9, target_gssi: 5002, priority: 0 }),
            &local,
            &mut q,
        );
        drain(&mut q);
        core.on_src_change(&mut q, 3, 20);
        let msgs = drain(&mut q);
        match &msgs[0].msg {
            SapMsgInner::CmceCallControl(CallControl::NetworkCallStart { source_issi, .. }) => {
                assert_eq!(*source_issi, 9_000_000 + 20);
            }
            other => panic!("expected re-issued NetworkCallStart, got {other:?}"),
        }
    }

    #[test]
    fn voice_for_unknown_stream_is_dropped() {
        let mut core = Core::new(settings());
        let mut q = MessageQueue::new();
        core.on_voice(999, 0, 0, vec![0i16; PCM_SAMPLES_PER_FRAME]);
        core.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        core.playout(&mut q);
        assert!(drain(&mut q).is_empty());
    }
}
