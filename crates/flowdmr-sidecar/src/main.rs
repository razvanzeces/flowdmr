//! FlowDMR sidecar — receives DMR off-air via an RTL-SDR (through dsd-neo),
//! and streams decoded audio + talker id to the FlowStation FlowDMR entity for
//! local injection as a TETRA group call. Includes a tiny control dashboard.
//!
//! Usage:
//!   flowdmr-sidecar --config /etc/flowdmr/sidecar.toml
//!   flowdmr-sidecar --write-default-config /etc/flowdmr/sidecar.toml
//!
//! Everything is localhost-only; this process never talks to Brew or any network.

// `% N == 0` is clearer than `.is_multiple_of(N)` and avoids a newer-toolchain
// dependency for the Pi cross-build.
#![allow(clippy::manual_is_multiple_of)]

mod config;
mod dashboard;
mod decoder;
mod meta;
mod pcm;
mod session;
mod status;
mod wire;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use config::{Config, SharedConfig};
use session::CallManager;
use status::SharedStatus;
use wire::UdpSink;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut config_path: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => config_path = args.next(),
            "--write-default-config" => {
                let path = args.next().unwrap_or_else(|| "flowdmr-sidecar.toml".to_string());
                if let Err(e) = std::fs::write(&path, Config::default().to_toml()) {
                    eprintln!("failed to write default config to {path}: {e}");
                    std::process::exit(1);
                }
                println!("wrote default config to {path}");
                return;
            }
            "--help" | "-h" => {
                println!(
                    "flowdmr-sidecar\n\n  --config <path>                load config TOML\n  \
                     --write-default-config <path>  write a default config and exit\n"
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let cfg = match &config_path {
        Some(path) => match Config::load(path) {
            Ok(c) => {
                tracing::info!("flowdmr-sidecar: loaded config from {path}");
                c
            }
            Err(e) => {
                tracing::error!("flowdmr-sidecar: failed to load {path}: {e} — using defaults");
                Config::default()
            }
        },
        None => {
            tracing::warn!("flowdmr-sidecar: no --config given, using built-in defaults");
            Config::default()
        }
    };

    let shared = SharedConfig::new(cfg, config_path);
    let status = SharedStatus::new();
    let cm = Arc::new(Mutex::new(CallManager::new()));
    let start = Instant::now();

    let base_sink = match UdpSink::connect(&shared.static_cfg.entity_addr) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("flowdmr-sidecar: cannot open IPC socket to {}: {e}", shared.static_cfg.entity_addr);
            std::process::exit(1);
        }
    };

    // Decoder supervisor (also drives metadata parsing).
    {
        let (cfg, cm, status, start) = (shared.clone(), cm.clone(), status.clone(), start);
        let sink = base_sink.try_clone().expect("clone sink");
        std::thread::Builder::new()
            .name("flowdmr-decoder".into())
            .spawn(move || decoder::run(cfg, cm, sink, status, start))
            .expect("spawn decoder thread");
    }

    // PCM ingest from dsd-neo.
    {
        let (cfg, cm, status, start) = (shared.clone(), cm.clone(), status.clone(), start);
        let sink = base_sink.try_clone().expect("clone sink");
        let port = shared.static_cfg.dsd_pcm_port;
        std::thread::Builder::new()
            .name("flowdmr-pcm".into())
            .spawn(move || {
                if let Err(e) = pcm::run(port, cfg, cm, sink, status, start) {
                    tracing::error!("flowdmr-sidecar: PCM ingest failed: {e}");
                }
            })
            .expect("spawn pcm thread");
    }

    // Watchdog: end calls on silence + periodic keepalive.
    {
        let (cfg, cm, start) = (shared.clone(), cm.clone(), start);
        let mut sink = base_sink.try_clone().expect("clone sink");
        let silence = shared.static_cfg.silence_timeout_ms;
        std::thread::Builder::new()
            .name("flowdmr-watchdog".into())
            .spawn(move || {
                let mut ticks = 0u64;
                loop {
                    std::thread::sleep(Duration::from_millis(100));
                    let now_ms = start.elapsed().as_millis() as u64;
                    {
                        let mut mgr = cm.lock().expect("cm lock");
                        mgr.tick(now_ms, silence, &mut sink);
                        ticks += 1;
                        if ticks % 20 == 0 {
                            mgr.keepalive(&mut sink); // ~ every 2s
                        }
                    }
                    let _ = &cfg;
                }
            })
            .expect("spawn watchdog thread");
    }

    tracing::info!(
        "flowdmr-sidecar: started. entity={} pcm_port={} dashboard={}",
        shared.static_cfg.entity_addr,
        shared.static_cfg.dsd_pcm_port,
        shared.static_cfg.dashboard_bind
    );

    // Dashboard on the main thread (blocking).
    if let Err(e) = dashboard::run(&shared.static_cfg.dashboard_bind, shared.clone(), status) {
        tracing::error!("flowdmr-sidecar: dashboard failed: {e}");
        std::process::exit(1);
    }
}
