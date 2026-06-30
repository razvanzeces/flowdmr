//! Shared runtime status + a live decoder-log ring, surfaced by the dashboard.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub decoder_running: bool,
    pub decoder_pid: Option<u32>,
    pub decoder_restarts: u64,
    pub pcm_frames: u64,
    pub seconds_since_pcm: Option<u64>,
    pub last_meta_line: String,
    pub active_call: bool,
    pub current_source: Option<u32>,
    pub calls_total: u64,
    pub last_error: Option<String>,
    /// Peak |sample| of the audio going to the encoder (with peak-hold/decay),
    /// AFTER pcm_gain — this is what the ACELP encoder actually sees.
    pub pcm_peak: i16,
    /// Count of frames whose peak hit (near) full scale — i.e. clipping.
    pub pcm_clip: u64,
}

#[derive(Clone, Default)]
pub struct SharedStatus(Arc<Mutex<Status>>);

impl SharedStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Status {
        self.0.lock().expect("status lock").clone()
    }

    pub fn update<F: FnOnce(&mut Status)>(&self, f: F) {
        let mut g = self.0.lock().expect("status lock");
        f(&mut g);
    }
}

/// A bounded ring of the most recent decoder console lines (live tail for the UI).
#[derive(Clone)]
pub struct SharedLog {
    lines: Arc<Mutex<VecDeque<String>>>,
    cap: usize,
}

impl SharedLog {
    pub fn new(cap: usize) -> Self {
        Self { lines: Arc::new(Mutex::new(VecDeque::with_capacity(cap))), cap }
    }

    pub fn push(&self, line: &str) {
        let mut g = self.lines.lock().expect("log lock");
        if g.len() >= self.cap {
            g.pop_front();
        }
        g.push_back(line.to_string());
    }

    /// The last `n` lines joined with newlines (oldest first).
    pub fn tail(&self, n: usize) -> String {
        let g = self.lines.lock().expect("log lock");
        let skip = g.len().saturating_sub(n);
        g.iter().skip(skip).cloned().collect::<Vec<_>>().join("\n")
    }
}
