//! Per-call playout jitter buffer for downlink voice.
//!
//! Ported from FlowStation's `net_brew::components::jitter_buffer` so the entity
//! depends only on FlowStation's stable public API (trait/SAP/config/codec), not
//! its internal `net_brew` modules. Holds already-encoded 35-byte ACELP blocks
//! and releases one per playout tick (~56.67 ms, one TETRA TCH/S frame).

use std::collections::VecDeque;

const MIN_FRAMES: usize = 2;
const BASE_FRAMES: usize = 4;
const TARGET_MAX_FRAMES: usize = 12;
const MAX_FRAMES: usize = 24;

/// Adaptive playout buffer for one call's ACELP voice blocks.
#[derive(Default)]
pub struct VoiceJitterBuffer {
    frames: VecDeque<Vec<u8>>,
    started: bool,
    target_frames: usize,
    underrun_boost: usize,
    stable_pops: u32,
    dropped_overflow: u64,
    underruns: u64,
    initial_latency_frames: usize,
}

impl VoiceJitterBuffer {
    pub fn with_initial_latency(initial_latency_frames: usize) -> Self {
        let initial = initial_latency_frames.min(TARGET_MAX_FRAMES - MIN_FRAMES);
        Self {
            target_frames: BASE_FRAMES + initial,
            initial_latency_frames: initial,
            ..Default::default()
        }
    }

    /// Queue one encoded 35-byte ACELP block.
    pub fn push(&mut self, acelp_data: Vec<u8>) {
        if self.target_frames == 0 {
            self.target_frames = BASE_FRAMES + self.initial_latency_frames;
        }
        self.frames.push_back(acelp_data);
        while self.frames.len() > MAX_FRAMES {
            self.frames.pop_front();
            self.dropped_overflow += 1;
        }
    }

    /// Release the next block once enough have buffered (initial latency), then
    /// one per call. Returns `None` while priming or on underrun.
    pub fn pop_ready(&mut self) -> Option<Vec<u8>> {
        if self.target_frames == 0 {
            self.target_frames = BASE_FRAMES + self.initial_latency_frames;
        }
        if !self.started {
            if self.frames.len() < self.target_frames {
                return None;
            }
            self.started = true;
        }
        match self.frames.pop_front() {
            Some(frame) => {
                if self.frames.len() >= self.target_frames {
                    self.stable_pops = self.stable_pops.saturating_add(1);
                    if self.stable_pops >= 80 {
                        self.stable_pops = 0;
                        if self.underrun_boost > 0 {
                            self.underrun_boost -= 1;
                            self.recompute_target();
                        }
                    }
                } else {
                    self.stable_pops = 0;
                }
                Some(frame)
            }
            None => {
                self.started = false;
                self.underruns += 1;
                self.underrun_boost = (self.underrun_boost + 1).min(4);
                self.stable_pops = 0;
                self.recompute_target();
                None
            }
        }
    }

    /// Unconditional drain — used after end-of-transmission so trailing audio
    /// plays out instead of being clipped.
    pub fn pop_drain(&mut self) -> Option<Vec<u8>> {
        self.frames.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Drop all buffered audio (speaker change / teardown) and re-prime.
    pub fn flush(&mut self) -> usize {
        let count = self.frames.len();
        self.frames.clear();
        self.started = false;
        self.underrun_boost = 0;
        self.stable_pops = 0;
        self.target_frames = BASE_FRAMES + self.initial_latency_frames;
        count
    }

    fn recompute_target(&mut self) {
        let target = BASE_FRAMES + self.initial_latency_frames + self.underrun_boost;
        self.target_frames = target.clamp(MIN_FRAMES, TARGET_MAX_FRAMES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primes_then_releases_one_per_pop() {
        let mut j = VoiceJitterBuffer::with_initial_latency(0); // target = BASE_FRAMES (4)
        for i in 0..3 {
            j.push(vec![i as u8]);
        }
        assert!(j.pop_ready().is_none(), "should prime until target reached");
        j.push(vec![3]);
        // Now 4 buffered == target; starts releasing.
        assert_eq!(j.pop_ready(), Some(vec![0]));
        assert_eq!(j.pop_ready(), Some(vec![1]));
    }

    #[test]
    fn drain_returns_until_empty() {
        let mut j = VoiceJitterBuffer::with_initial_latency(0);
        j.push(vec![1]);
        j.push(vec![2]);
        assert_eq!(j.pop_drain(), Some(vec![1]));
        assert_eq!(j.pop_drain(), Some(vec![2]));
        assert_eq!(j.pop_drain(), None);
    }

    #[test]
    fn flush_clears_and_reprimes() {
        let mut j = VoiceJitterBuffer::with_initial_latency(0);
        for i in 0..6 {
            j.push(vec![i]);
        }
        assert_eq!(j.flush(), 6);
        assert!(j.is_empty());
        assert!(j.pop_ready().is_none());
    }
}
