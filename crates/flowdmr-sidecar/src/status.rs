//! Shared runtime status surfaced by the mini-dashboard.

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
