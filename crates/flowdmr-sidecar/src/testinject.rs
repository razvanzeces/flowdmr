//! `flowdmr-sidecar test-inject` — send a SYNTHETIC call to the FlowStation
//! entity over the real flowdmr-ipc wire protocol, with no RTL-SDR or dsd-neo.
//!
//! Use it to prove the TETRA side end-to-end: entity -> CMCE group call ->
//! downlink -> your TETRA radio hears a tone (or a WAV clip) on the injection TG.
//! This isolates "does injection into TETRA work" from "does DMR decode work".
//!
//! Examples:
//!   flowdmr-sidecar test-inject --tg 5000 --tone 1000 --secs 5
//!   flowdmr-sidecar test-inject --tg 5000 --wav hello-8k-mono.wav --src 2604123

use std::time::{Duration, Instant};

use flowdmr_ipc::{FlowDmrBody, FlowDmrFrame, PCM_SAMPLES_PER_FRAME, VOICE_FLAG_FIRST, VOICE_FLAG_LAST};

use crate::session::FrameSink;
use crate::wire::UdpSink;

struct Opts {
    entity: String,
    tg: u32,
    src: u32,
    secs: f64,
    tone_hz: f64,
    amplitude: i16,
    wav: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            entity: "127.0.0.1:23471".into(),
            tg: 5000,
            src: 1_234_567,
            secs: 5.0,
            tone_hz: 1000.0,
            amplitude: 8000,
            wav: None,
        }
    }
}

pub fn run<I: Iterator<Item = String>>(mut args: I) -> i32 {
    let mut o = Opts::default();
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_default();
        match a.as_str() {
            "--entity" => o.entity = val(),
            "--tg" => o.tg = val().parse().unwrap_or(o.tg),
            "--src" => o.src = val().parse().unwrap_or(o.src),
            "--secs" => o.secs = val().parse().unwrap_or(o.secs),
            "--tone" => o.tone_hz = val().parse().unwrap_or(o.tone_hz),
            "--amp" => o.amplitude = val().parse().unwrap_or(o.amplitude),
            "--wav" => o.wav = Some(val()),
            "--help" | "-h" => {
                eprintln!(
                    "test-inject: send a synthetic call to the FlowStation entity\n\
                     --entity <addr>  entity IPC address (default 127.0.0.1:23471)\n\
                     --tg <gssi>      injection TalkGroup (must be in local_ssi_ranges)\n\
                     --src <id>       DMR source id to display (default 1234567)\n\
                     --secs <n>       tone duration seconds (default 5)\n\
                     --tone <hz>      tone frequency (default 1000)\n\
                     --amp <i16>      tone amplitude (default 8000)\n\
                     --wav <path>     play an 8kHz mono 16-bit WAV instead of a tone"
                );
                return 0;
            }
            other => {
                eprintln!("test-inject: unknown arg {other}");
                return 2;
            }
        }
    }

    let pcm: Vec<i16> = match &o.wav {
        Some(path) => match load_wav_8k_mono(path) {
            Ok(s) => {
                eprintln!("test-inject: loaded {} samples ({:.1}s) from {path}", s.len(), s.len() as f64 / 8000.0);
                s
            }
            Err(e) => {
                eprintln!("test-inject: failed to read WAV {path}: {e}");
                return 1;
            }
        },
        None => gen_tone(o.tone_hz, o.secs, o.amplitude),
    };

    let mut sink = match UdpSink::connect(&o.entity) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("test-inject: cannot reach entity at {}: {e}", o.entity);
            return 1;
        }
    };

    eprintln!(
        "test-inject: sending {} frames -> {} : TG={} src={} ({})",
        pcm.len() / PCM_SAMPLES_PER_FRAME,
        o.entity,
        o.tg,
        o.src,
        if o.wav.is_some() { "wav" } else { "tone" }
    );

    let stream_id = 0xC0FFEE;
    sink.send(FlowDmrFrame::new(
        stream_id,
        FlowDmrBody::CallStart { source_id: o.src, dmr_tg: o.tg, target_gssi: o.tg, priority: 0 },
    ));

    let frames: Vec<&[i16]> = pcm.chunks(PCM_SAMPLES_PER_FRAME).collect();
    let n = frames.len();
    let frame_dt = Duration::from_micros(30_000); // 30 ms per 240-sample frame (real time)
    let t0 = Instant::now();
    for (i, chunk) in frames.iter().enumerate() {
        let mut samples = chunk.to_vec();
        samples.resize(PCM_SAMPLES_PER_FRAME, 0); // pad last frame
        let mut flags = 0;
        if i == 0 {
            flags |= VOICE_FLAG_FIRST;
        }
        if i + 1 == n {
            flags |= VOICE_FLAG_LAST;
        }
        sink.send(FlowDmrFrame::new(stream_id, FlowDmrBody::Voice { seq: i as u32, flags, pcm: samples }));

        // Pace to real time so the entity's jitter buffer behaves naturally.
        let target = t0 + frame_dt * (i as u32 + 1);
        if let Some(sleep) = target.checked_duration_since(Instant::now()) {
            std::thread::sleep(sleep);
        }
    }

    sink.send(FlowDmrFrame::new(stream_id, FlowDmrBody::CallEnd));
    // Let trailing frames drain through the jitter buffer before we exit.
    std::thread::sleep(Duration::from_millis(600));
    eprintln!("test-inject: done ({} frames, {:.1}s).", n, n as f64 * 0.03);
    0
}

fn gen_tone(hz: f64, secs: f64, amp: i16) -> Vec<i16> {
    let total = (8000.0 * secs) as usize;
    let mut out = Vec::with_capacity(total);
    for i in 0..total {
        let t = i as f64 / 8000.0;
        let v = (amp as f64 * (2.0 * std::f64::consts::PI * hz * t).sin()) as i16;
        out.push(v);
    }
    out
}

/// Minimal WAV reader: 16-bit PCM, mono 8 kHz preferred. Downmixes stereo and
/// warns (no resampling) on a non-8 kHz rate.
fn load_wav_8k_mono(path: &str) -> Result<Vec<i16>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut pos = 12;
    let mut channels = 1u16;
    let mut rate = 8000u32;
    let mut bits = 16u16;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(bytes.len());
        match id {
            b"fmt " if body_end - body_start >= 16 => {
                channels = u16::from_le_bytes([bytes[body_start + 2], bytes[body_start + 3]]);
                rate = u32::from_le_bytes([
                    bytes[body_start + 4],
                    bytes[body_start + 5],
                    bytes[body_start + 6],
                    bytes[body_start + 7],
                ]);
                bits = u16::from_le_bytes([bytes[body_start + 14], bytes[body_start + 15]]);
            }
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        pos = body_start + size + (size & 1); // chunks are word-aligned
    }
    if bits != 16 {
        return Err(format!("only 16-bit PCM supported (got {bits}-bit)"));
    }
    let data = data.ok_or("no data chunk")?;
    if rate != 8000 {
        eprintln!("test-inject: WARNING WAV is {rate} Hz, not 8000 — audio will be pitch-shifted (no resampler)");
    }
    let mut samples: Vec<i16> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    if channels == 2 {
        samples = samples.chunks_exact(2).map(|s| ((s[0] as i32 + s[1] as i32) / 2) as i16).collect();
    }
    Ok(samples)
}
