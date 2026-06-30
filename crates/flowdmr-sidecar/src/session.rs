//! Call lifecycle manager: reconciles the (call-less) PCM stream with the
//! metadata event stream into a clean sequence of FlowDMR IPC frames.
//!
//! It drives one TETRA call per DMR transmission: `CallStart` (with the talker's
//! source id and the configured injection GSSI), a run of `Voice` frames, optional
//! `SrcChange` on talker change, and `CallEnd` on metadata end-of-call or a PCM
//! silence timeout. Time is passed in as milliseconds so the silence watchdog is
//! deterministically testable.

use flowdmr_ipc::{FlowDmrBody, FlowDmrFrame, VOICE_FLAG_FIRST};

use crate::meta::MetaLine;
use crate::recorder::Recorder;

/// Sink for outbound IPC frames (UDP in production, a Vec in tests).
pub trait FrameSink {
    fn send(&mut self, frame: FlowDmrFrame);
}

struct Session {
    stream_id: u32,
    source_id: u32,
    dmr_tg: u32,
    last_pcm_ms: u64,
    seq: u32,
    first_voice_pending: bool,
}

/// Reconciles metadata + PCM into IPC frames for one active call at a time.
pub struct CallManager {
    session: Option<Session>,
    next_stream_id: u32,
    calls_started: u64,
    /// If non-zero, every injected call uses THIS single source id and talker
    /// changes are suppressed — one stable speaker on the TG instead of a churn
    /// of every decoded DMR id. 0 = pass through the decoded per-talker ids.
    fixed_source_id: u32,
    recorder: Recorder,
}

impl CallManager {
    pub fn new(fixed_source_id: u32) -> Self {
        Self {
            session: None,
            next_stream_id: 1,
            calls_started: 0,
            fixed_source_id,
            recorder: Recorder::disabled(),
        }
    }

    /// Attach a call recorder (writes a WAV per call). Disabled by default.
    pub fn set_recorder(&mut self, recorder: Recorder) {
        self.recorder = recorder;
    }

    pub fn has_active_call(&self) -> bool {
        self.session.is_some()
    }

    pub fn current_source(&self) -> Option<u32> {
        self.session.as_ref().map(|s| s.source_id)
    }

    /// Total number of calls started since launch.
    pub fn calls_started(&self) -> u64 {
        self.calls_started
    }

    /// Apply a parsed metadata line.
    pub fn on_meta<S: FrameSink>(&mut self, m: &MetaLine, target_gssi: u32, now_ms: u64, sink: &mut S) {
        if m.call_end {
            self.end_call(sink);
            return;
        }
        if m.source.is_none() && m.talkgroup.is_none() {
            return;
        }
        // Refine the recording's filename metadata with the real decoded ids.
        self.recorder.note_meta(m.source, m.talkgroup);

        let fixed = self.fixed_source_id;
        match self.session.as_mut() {
            None => {
                // Metadata-led call start (the common case: header precedes audio).
                self.start_call(m.source.unwrap_or(0), m.talkgroup.unwrap_or(0), target_gssi, now_ms, sink);
            }
            Some(sess) => {
                if let Some(tg) = m.talkgroup {
                    sess.dmr_tg = tg;
                }
                // With a fixed source id we keep one stable speaker — never emit
                // talker changes (each one flushes the jitter and re-grants the floor).
                if fixed == 0 {
                    if let Some(src) = m.source {
                        if src != sess.source_id {
                            sess.source_id = src;
                            sink.send(FlowDmrFrame::new(sess.stream_id, FlowDmrBody::SrcChange { source_id: src }));
                        }
                    }
                }
            }
        }
    }

    /// Apply one 30 ms PCM frame (240 samples).
    pub fn on_pcm<S: FrameSink>(&mut self, pcm: Vec<i16>, target_gssi: u32, now_ms: u64, sink: &mut S) {
        if self.session.is_none() {
            // Audio without a prior metadata header: start a provisional call so
            // we don't lose the start of the transmission. The real source id
            // arrives shortly via on_meta -> SrcChange.
            self.start_call(0, 0, target_gssi, now_ms, sink);
        }
        self.recorder.write(&pcm); // archive the call audio
        let sess = self.session.as_mut().expect("session present");
        sess.last_pcm_ms = now_ms;
        let flags = if sess.first_voice_pending {
            sess.first_voice_pending = false;
            VOICE_FLAG_FIRST
        } else {
            0
        };
        let seq = sess.seq;
        sess.seq = sess.seq.wrapping_add(1);
        sink.send(FlowDmrFrame::new(sess.stream_id, FlowDmrBody::Voice { seq, flags, pcm }));
    }

    /// Watchdog: end the call if no PCM has arrived for `silence_timeout_ms`.
    pub fn tick<S: FrameSink>(&mut self, now_ms: u64, silence_timeout_ms: u64, sink: &mut S) {
        let ended = self
            .session
            .as_ref()
            .is_some_and(|s| now_ms.saturating_sub(s.last_pcm_ms) >= silence_timeout_ms);
        if ended {
            self.end_call(sink);
        }
    }

    /// Send a keepalive so the entity can report "sidecar connected".
    pub fn keepalive<S: FrameSink>(&self, sink: &mut S) {
        sink.send(FlowDmrFrame::new(0, FlowDmrBody::Keepalive));
    }

    fn start_call<S: FrameSink>(&mut self, source_id: u32, dmr_tg: u32, target_gssi: u32, now_ms: u64, sink: &mut S) {
        // Record the REAL decoded ids (before the fixed-id override used for injection).
        self.recorder.start(source_id, dmr_tg);
        // A fixed source id overrides the decoded one (including the provisional 0).
        let source_id = if self.fixed_source_id != 0 { self.fixed_source_id } else { source_id };
        let stream_id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1).max(1);
        self.calls_started = self.calls_started.wrapping_add(1);
        self.session = Some(Session {
            stream_id,
            source_id,
            dmr_tg,
            last_pcm_ms: now_ms,
            seq: 0,
            first_voice_pending: true,
        });
        sink.send(FlowDmrFrame::new(
            stream_id,
            FlowDmrBody::CallStart { source_id, dmr_tg, target_gssi, priority: 0 },
        ));
    }

    fn end_call<S: FrameSink>(&mut self, sink: &mut S) {
        self.recorder.finish();
        if let Some(sess) = self.session.take() {
            sink.send(FlowDmrFrame::new(sess.stream_id, FlowDmrBody::CallEnd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowdmr_ipc::PCM_SAMPLES_PER_FRAME;

    #[derive(Default)]
    struct VecSink(Vec<FlowDmrFrame>);
    impl FrameSink for VecSink {
        fn send(&mut self, frame: FlowDmrFrame) {
            self.0.push(frame);
        }
    }

    fn pcm() -> Vec<i16> {
        vec![0i16; PCM_SAMPLES_PER_FRAME]
    }

    #[test]
    fn meta_led_call_then_voice_then_end() {
        let mut cm = CallManager::new(0);
        let mut sink = VecSink::default();

        cm.on_meta(&MetaLine { source: Some(2_604_001), talkgroup: Some(9), call_end: false }, 5000, 0, &mut sink);
        cm.on_pcm(pcm(), 5000, 10, &mut sink);
        cm.on_pcm(pcm(), 5000, 40, &mut sink);
        cm.on_meta(&MetaLine { call_end: true, ..Default::default() }, 5000, 100, &mut sink);

        let kinds: Vec<_> = sink.0.iter().map(|f| f.kind()).collect();
        use flowdmr_ipc::FlowDmrKind::*;
        assert_eq!(kinds, vec![CallStart, Voice, Voice, CallEnd]);

        match &sink.0[0].body {
            FlowDmrBody::CallStart { source_id, dmr_tg, target_gssi, .. } => {
                assert_eq!((*source_id, *dmr_tg, *target_gssi), (2_604_001, 9, 5000));
            }
            _ => unreachable!(),
        }
        // First voice frame flagged.
        match &sink.0[1].body {
            FlowDmrBody::Voice { flags, .. } => assert_eq!(*flags & VOICE_FLAG_FIRST, VOICE_FLAG_FIRST),
            _ => unreachable!(),
        }
        assert!(!cm.has_active_call());
    }

    #[test]
    fn pcm_before_meta_starts_provisional_then_srcchange() {
        let mut cm = CallManager::new(0);
        let mut sink = VecSink::default();
        cm.on_pcm(pcm(), 5001, 0, &mut sink); // provisional CallStart (src 0) + Voice
        cm.on_meta(&MetaLine { source: Some(777), talkgroup: Some(9), call_end: false }, 5001, 20, &mut sink);

        use flowdmr_ipc::FlowDmrKind::*;
        let kinds: Vec<_> = sink.0.iter().map(|f| f.kind()).collect();
        assert_eq!(kinds, vec![CallStart, Voice, SrcChange]);
        assert_eq!(cm.current_source(), Some(777));
    }

    #[test]
    fn silence_timeout_ends_call() {
        let mut cm = CallManager::new(0);
        let mut sink = VecSink::default();
        cm.on_meta(&MetaLine { source: Some(1), talkgroup: Some(2), call_end: false }, 5000, 0, &mut sink);
        cm.on_pcm(pcm(), 5000, 0, &mut sink);
        cm.tick(500, 1200, &mut sink); // not yet
        assert!(cm.has_active_call());
        cm.tick(1300, 1200, &mut sink); // exceeded
        assert!(!cm.has_active_call());
        assert_eq!(sink.0.last().unwrap().kind(), flowdmr_ipc::FlowDmrKind::CallEnd);
    }

    #[test]
    fn fixed_source_id_one_speaker_no_srcchange() {
        let mut cm = CallManager::new(7); // fixed id 7
        let mut sink = VecSink::default();
        cm.on_meta(&MetaLine { source: Some(100), talkgroup: Some(9), call_end: false }, 5000, 0, &mut sink);
        cm.on_meta(&MetaLine { source: Some(200), talkgroup: Some(9), call_end: false }, 5000, 50, &mut sink);
        // Exactly one CallStart with id 7, and NO SrcChange despite the id changing.
        use flowdmr_ipc::FlowDmrKind::*;
        let kinds: Vec<_> = sink.0.iter().map(|f| f.kind()).collect();
        assert_eq!(kinds, vec![CallStart]);
        match &sink.0[0].body {
            FlowDmrBody::CallStart { source_id, .. } => assert_eq!(*source_id, 7),
            _ => unreachable!(),
        }
    }

    #[test]
    fn talker_change_within_call() {
        let mut cm = CallManager::new(0);
        let mut sink = VecSink::default();
        cm.on_meta(&MetaLine { source: Some(100), talkgroup: Some(9), call_end: false }, 5000, 0, &mut sink);
        cm.on_meta(&MetaLine { source: Some(200), talkgroup: Some(9), call_end: false }, 5000, 50, &mut sink);
        let src_changes: Vec<_> = sink
            .0
            .iter()
            .filter_map(|f| match f.body {
                FlowDmrBody::SrcChange { source_id } => Some(source_id),
                _ => None,
            })
            .collect();
        assert_eq!(src_changes, vec![200]);
    }
}
