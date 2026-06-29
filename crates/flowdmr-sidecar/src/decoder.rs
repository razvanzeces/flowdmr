//! dsd-neo / dsd-fme child-process supervisor.
//!
//! Builds the decoder command line from the live RF settings, spawns it, reads
//! its console output line-by-line to drive call metadata, and restarts it when
//! the dashboard changes the frequency/gain/ppm or the child dies.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::SharedConfig;
use crate::meta::MetaParser;
use crate::session::CallManager;
use crate::status::SharedStatus;
use crate::wire::UdpSink;

/// Build the dsd-neo argument vector from current settings.
pub fn build_args(cfg: &SharedConfig) -> Vec<String> {
    let s = &cfg.static_cfg;
    let live = cfg.live();
    let gain = live.gain_db.round() as i64;
    // dsd-neo RTL input: rtl:<dev>:<freq>:<gain>:<ppm>:<bandwidth_khz>:<squelch>:<volume>
    // Frequency is given as MHz with an `M` suffix, matching the dsd-neo examples
    // (e.g. `851.0125M`).
    let freq = format!("{:.4}M", live.rx_freq_hz as f64 / 1_000_000.0);
    let rtl = format!(
        "rtl:{}:{}:{}:{}:{}:{}:{}",
        s.rtl_device, freq, gain, live.ppm, s.rtl_bandwidth_khz, s.squelch, s.volume
    );
    let mut args: Vec<String> = s.dsd_mode_args.clone();
    args.push("-i".into());
    args.push(rtl);
    args.push("-o".into());
    args.push(format!("udp:127.0.0.1:{}", s.dsd_pcm_port));
    args.extend(s.dsd_extra_args.clone());
    args
}

/// Run the supervisor loop (blocking). Never returns under normal operation.
pub fn run(
    cfg: SharedConfig,
    cm: Arc<Mutex<CallManager>>,
    mut sink: UdpSink,
    status: SharedStatus,
    start: Instant,
) {
    let parser = match MetaParser::new(&cfg.static_cfg.re_source, &cfg.static_cfg.re_talkgroup, &cfg.static_cfg.re_call_end) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("flowdmr-sidecar: invalid metadata regex in config: {e}");
            status.update(|s| s.last_error = Some(format!("regex: {e}")));
            return;
        }
    };

    loop {
        let args = build_args(&cfg);
        let gen_at_spawn = cfg.restart_generation();
        tracing::info!("flowdmr-sidecar: launching `{} {}`", cfg.static_cfg.dsd_bin, args.join(" "));

        let mut child = match spawn(&cfg.static_cfg.dsd_bin, &args) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("flowdmr-sidecar: failed to launch decoder: {e}");
                status.update(|s| {
                    s.decoder_running = false;
                    s.last_error = Some(format!("spawn: {e}"));
                });
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        let pid = child.id();
        status.update(|s| {
            s.decoder_running = true;
            s.decoder_pid = Some(pid);
            s.last_error = None;
        });

        // Merge stdout+stderr into one line channel via reader threads.
        let (tx, rx) = mpsc::channel::<String>();
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, tx.clone());
        }
        drop(tx);

        // Process lines until restart requested or child exits.
        loop {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(line) => {
                    let m = parser.parse_line(&line);
                    if !m.is_empty() {
                        let now_ms = start.elapsed().as_millis() as u64;
                        let tg = cfg.live().injection_tg;
                        let mut mgr = cm.lock().expect("cm lock");
                        mgr.on_meta(&m, tg, now_ms, &mut sink);
                        let active = mgr.has_active_call();
                        let src = mgr.current_source();
                        let total = mgr.calls_started();
                        drop(mgr);
                        status.update(|s| {
                            s.last_meta_line = line.clone();
                            s.active_call = active;
                            s.current_source = src;
                            s.calls_total = total;
                        });
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break, // child output closed
            }

            // Restart if RF parameters changed.
            if cfg.restart_generation() != gen_at_spawn {
                tracing::info!("flowdmr-sidecar: RF settings changed — restarting decoder");
                let _ = child.kill();
                break;
            }
            // Detect child exit.
            match child.try_wait() {
                Ok(Some(st)) => {
                    tracing::warn!("flowdmr-sidecar: decoder exited ({st}); restarting");
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("flowdmr-sidecar: try_wait error: {e}");
                    break;
                }
            }
        }

        let _ = child.wait();
        status.update(|s| {
            s.decoder_running = false;
            s.decoder_pid = None;
            s.decoder_restarts += 1;
        });
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn spawn(bin: &str, args: &[String]) -> std::io::Result<Child> {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn spawn_reader<R: std::io::Read + Send + 'static>(reader: R, tx: mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn builds_expected_rtl_command() {
        let mut cfg = Config::default();
        cfg.rtl_device = 0;
        cfg.dsd_pcm_port = 23470;
        let mut shared = SharedConfig::new(cfg, None);
        let mut live = shared.live();
        live.rx_freq_hz = 439_000_000;
        live.gain_db = 0.0;
        live.ppm = 0;
        shared.apply_live(live);
        // Rebuild shared so static_cfg reflects defaults (apply_live only changes live).
        let _ = &mut shared;

        let args = build_args(&shared);
        let joined = args.join(" ");
        assert!(joined.contains("-i rtl:0:439.0000M:0:0:12:0:2"), "got: {joined}");
        assert!(joined.contains("-o udp:127.0.0.1:23470"), "got: {joined}");
        assert!(joined.starts_with("-fs -nm"), "mode args first: {joined}");
    }
}
