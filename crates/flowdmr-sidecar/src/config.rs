//! Sidecar configuration. Loaded from a TOML file; the live-mutable subset
//! (RX frequency, gain, injection talkgroup) is also editable from the
//! mini-dashboard at runtime.

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// Settings the mini-dashboard can change at runtime (trigger a decoder restart
/// when the RF parameters change).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSettings {
    /// RX centre frequency in Hz (the DMR channel).
    pub rx_freq_hz: u64,
    /// Tuner gain in dB (0 = let the driver pick / AGC per `gain_mode`).
    pub gain_db: f32,
    /// Frequency correction in PPM.
    pub ppm: i32,
    /// TETRA GSSI to inject decoded audio onto. MUST be inside the cell's
    /// `local_ssi_ranges` or the entity will refuse it.
    pub injection_tg: u32,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            rx_freq_hz: 439_000_000,
            gain_db: 0.0,
            ppm: 0,
            injection_tg: 5000,
        }
    }
}

/// Static settings (require a restart of the sidecar to change).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ── Decoder process ────────────────────────────────────────────────────
    /// Path to the dsd-neo / dsd-fme binary.
    pub dsd_bin: String,
    /// Decoder-mode args. Default `-fs -nm` = DMR BS/MS + MONO audio (mono is
    /// REQUIRED: dsd-neo's UDP audio is stereo otherwise). No `-N` so it stays in
    /// console text mode and the metadata parser can read call lines.
    pub dsd_mode_args: Vec<String>,
    /// Extra raw args appended to the dsd-neo command line (advanced tuning).
    pub dsd_extra_args: Vec<String>,
    /// RTL-SDR device index (as understood by dsd-neo's `rtl:<dev>:...`).
    pub rtl_device: u32,
    /// RTL DSP bandwidth (kHz) in the `rtl:` input string. MUST give an integer
    /// samples-per-symbol for DMR (4800 sym/s): 24 -> SPS 5, 48 -> SPS 10. Do NOT
    /// use 12 (SPS 2.5 — dsd-neo can't sync reliably).
    pub rtl_bandwidth_khz: u32,
    /// Squelch level passed to dsd-neo (0 = open).
    pub squelch: u32,
    /// Sample volume multiplier passed to dsd-neo. Keep at 1 — higher values clip
    /// voice peaks, which then re-encode badly through ACELP (breath/noise survives,
    /// voice goes robotic).
    pub volume: u32,
    /// Linear gain applied to the decoded PCM in the sidecar BEFORE it is sent to
    /// the entity. < 1.0 gives the ACELP encoder headroom so formant peaks don't
    /// clip (try 0.6–0.8 if voice sounds robotic but breath/noise is clean).
    pub pcm_gain: f32,
    /// UDP port dsd-neo streams 8 kHz PCM to (its `-o udp:127.0.0.1:<port>`).
    pub dsd_pcm_port: u16,

    // ── IPC to the FlowStation entity ──────────────────────────────────────
    /// Address of the FlowStation FlowDMR entity (its `FLOWDMR_LISTEN`).
    pub entity_addr: String,

    // ── Mini-dashboard ─────────────────────────────────────────────────────
    /// Bind address for the control dashboard. 0.0.0.0 = reachable from the LAN
    /// (e.g. http://<pi-ip>:8081). NOTE: no auth — only expose on a trusted
    /// network; use 127.0.0.1 to restrict it to the Pi itself.
    pub dashboard_bind: String,

    // ── Call detection ─────────────────────────────────────────────────────
    /// End a call after this many milliseconds with no PCM (silence).
    pub silence_timeout_ms: u64,
    /// If non-zero, inject EVERYTHING as one stable speaker with this DMR source
    /// id (talker changes suppressed) — clean single id on the TG instead of a
    /// churn of every decoded id, which matters on busy multi-talkgroup systems.
    /// Set to 0 to pass through the real per-talker ids.
    pub fixed_source_id: u32,

    // ── Metadata parsing (tune for your dsd-neo build) ─────────────────────
    /// Regex capturing the DMR source/radio id (group 1). Applied to each
    /// decoder console line.
    pub re_source: String,
    /// Regex capturing the DMR talkgroup (group 1).
    pub re_talkgroup: String,
    /// Regex that, when matched, marks end-of-call / loss of sync.
    pub re_call_end: String,

    // ── Live (dashboard-editable) ──────────────────────────────────────────
    pub live: LiveSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dsd_bin: "dsd-neo".to_string(),
            dsd_mode_args: vec!["-fs".to_string(), "-nm".to_string()],
            dsd_extra_args: vec![],
            rtl_device: 0,
            rtl_bandwidth_khz: 24,
            squelch: 0,
            volume: 1,
            pcm_gain: 0.8,
            dsd_pcm_port: 23470,
            entity_addr: "127.0.0.1:23471".to_string(),
            dashboard_bind: "0.0.0.0:8081".to_string(),
            silence_timeout_ms: 1200,
            fixed_source_id: 1,
            // Defaults tuned for common dsd-fme / dsd-neo console output. Adjust
            // for your build via the config file if metadata isn't picked up.
            re_source: r"(?i)\b(?:source|src)\D{0,4}(\d{2,8})".to_string(),
            re_talkgroup: r"(?i)\b(?:target|group|tg|tgt)\D{0,4}(\d{1,8})".to_string(),
            re_call_end: r"(?i)(no sync|sync:\s*no|call end|terminator|voice end)".to_string(),
            live: LiveSettings::default(),
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(cfg)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }
}

/// Thread-shared handle to the config, with the live subset behind an RwLock and
/// a restart flag the decoder supervisor watches.
#[derive(Clone)]
pub struct SharedConfig {
    pub static_cfg: Arc<Config>,
    live: Arc<RwLock<LiveSettings>>,
    /// Bumped whenever the live RF parameters change so the supervisor restarts
    /// the decoder. (Generation counter — supervisor compares against its last.)
    restart_gen: Arc<RwLock<u64>>,
    config_path: Arc<Option<String>>,
}

impl SharedConfig {
    pub fn new(cfg: Config, config_path: Option<String>) -> Self {
        let live = cfg.live.clone();
        Self {
            static_cfg: Arc::new(cfg),
            live: Arc::new(RwLock::new(live)),
            restart_gen: Arc::new(RwLock::new(0)),
            config_path: Arc::new(config_path),
        }
    }

    pub fn live(&self) -> LiveSettings {
        self.live.read().expect("live lock").clone()
    }

    pub fn restart_generation(&self) -> u64 {
        *self.restart_gen.read().expect("gen lock")
    }

    /// Apply a dashboard update. Returns true if the decoder must restart
    /// (RF parameters changed). The injection TG alone does not require a
    /// restart (it's applied to new call frames immediately).
    pub fn apply_live(&self, new: LiveSettings) -> bool {
        let mut guard = self.live.write().expect("live lock");
        let rf_changed = guard.rx_freq_hz != new.rx_freq_hz
            || guard.gain_db != new.gain_db
            || guard.ppm != new.ppm;
        *guard = new.clone();
        drop(guard);

        if rf_changed {
            *self.restart_gen.write().expect("gen lock") += 1;
        }
        self.persist(&new);
        rf_changed
    }

    fn persist(&self, live: &LiveSettings) {
        if let Some(path) = self.config_path.as_ref() {
            let mut cfg = (*self.static_cfg).clone();
            cfg.live = live.clone();
            if let Err(e) = std::fs::write(path, cfg.to_toml()) {
                tracing::warn!("flowdmr-sidecar: failed to persist config to {path}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_toml() {
        let cfg = Config::default();
        let text = cfg.to_toml();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.entity_addr, cfg.entity_addr);
        assert_eq!(back.live.injection_tg, cfg.live.injection_tg);
    }

    #[test]
    fn rf_change_triggers_restart_but_tg_does_not() {
        let shared = SharedConfig::new(Config::default(), None);
        let mut live = shared.live();
        live.injection_tg = 5123;
        assert!(!shared.apply_live(live), "TG change must not restart decoder");

        let mut live = shared.live();
        live.rx_freq_hz = 440_100_000;
        assert!(shared.apply_live(live), "freq change must restart decoder");
        assert_eq!(shared.restart_generation(), 1);
    }
}
