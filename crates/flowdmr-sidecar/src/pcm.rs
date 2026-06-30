//! PCM ingest: receive the decoder's 8 kHz s16 audio over UDP and reframe it
//! into fixed 240-sample (30 ms) frames for the call manager.
//!
//! dsd-neo's `-o udp:...` emits a raw little-endian s16 stream split across
//! datagrams of arbitrary length. NOTE: dsd-neo emits **stereo** even in DMR
//! mono mode (verified: its `-w` wav is "stereo 8000 Hz"), so by default we take
//! the LEFT channel of each interleaved L/R pair — reading stereo as mono gives
//! garbled, double-speed audio. Toggle via `dsd_audio_stereo`.

use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use flowdmr_ipc::PCM_SAMPLES_PER_FRAME;

use crate::config::SharedConfig;
use crate::session::CallManager;
use crate::status::SharedStatus;
use crate::wire::UdpSink;

/// Bind the PCM UDP socket and run the receive/reframe loop (blocking).
pub fn run(
    port: u16,
    cfg: SharedConfig,
    cm: Arc<Mutex<CallManager>>,
    mut sink: UdpSink,
    status: SharedStatus,
    start: Instant,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind(("127.0.0.1", port))?;
    tracing::info!("flowdmr-sidecar: PCM ingest listening on 127.0.0.1:{port}");
    let mut buf = [0u8; 4096];
    let mut bytebuf: Vec<u8> = Vec::with_capacity(8192);
    let mut acc: Vec<i16> = Vec::with_capacity(PCM_SAMPLES_PER_FRAME * 4);
    let gain = cfg.static_cfg.pcm_gain;
    let apply_gain = (gain - 1.0).abs() > 1e-4;
    let stereo = cfg.static_cfg.dsd_audio_stereo;
    let unit = if stereo { 4 } else { 2 }; // bytes consumed per emitted sample

    loop {
        let len = match socket.recv(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("flowdmr-sidecar: PCM recv error: {e}");
                continue;
            }
        };
        // Buffer raw bytes and consume only WHOLE L/R frames, carrying any
        // partial frame to the next datagram. Without this, a packet that splits
        // mid-pair would swap L/R for a burst — audible as a garbled/"encrypted"
        // patch until the stream re-aligned. Take the left channel (stereo) or
        // every sample (mono), optionally attenuating for ACELP headroom.
        bytebuf.extend_from_slice(&buf[..len]);
        let usable = (bytebuf.len() / unit) * unit;
        if usable > 0 {
            extract_samples(&bytebuf[..usable], stereo, gain, apply_gain, &mut acc);
            bytebuf.drain(..usable);
        }

        while acc.len() >= PCM_SAMPLES_PER_FRAME {
            let frame: Vec<i16> = acc.drain(..PCM_SAMPLES_PER_FRAME).collect();
            let now_ms = start.elapsed().as_millis() as u64;
            let tg = cfg.live().injection_tg;
            // Level meter: peak of what the ACELP encoder actually receives.
            let peak = frame.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0).min(32767) as i16;
            let clipped = peak >= 32760;
            {
                let mut mgr = cm.lock().expect("cm lock");
                mgr.on_pcm(frame, tg, now_ms, &mut sink);
            }
            status.update(|s| {
                s.pcm_frames += 1;
                s.seconds_since_pcm = Some(0);
                // Slow peak-hold decay (~0.26 dB/frame) so the bar shows the voice
                // peak, not the floor between syllables.
                s.pcm_peak = if peak > s.pcm_peak { peak } else { (s.pcm_peak as f32 * 0.97) as i16 };
                if clipped {
                    s.pcm_clip = s.pcm_clip.wrapping_add(1);
                }
            });
        }
    }
}

/// Append mono samples from a UDP datagram of interleaved little-endian s16.
/// When `stereo`, take the LEFT channel of each L/R pair (step 4 bytes);
/// otherwise every sample (step 2). `gain` is applied when `apply_gain`.
///
/// Assumes each datagram starts on a frame boundary (left sample first), which
/// is how dsd-neo packetizes — it never splits an L/R pair across datagrams.
fn extract_samples(bytes: &[u8], stereo: bool, gain: f32, apply_gain: bool, out: &mut Vec<i16>) {
    let step = if stereo { 4 } else { 2 };
    let mut i = 0;
    while i + 1 < bytes.len() {
        let mut s = i16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if apply_gain {
            s = (s as f32 * gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        out.push(s);
        i += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le_bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn stereo_takes_left_channel_only() {
        // Interleaved L/R: L=100,200,300  R=-1,-2,-3
        let bytes = le_bytes(&[100, -1, 200, -2, 300, -3]);
        let mut out = Vec::new();
        extract_samples(&bytes, true, 1.0, false, &mut out);
        assert_eq!(out, vec![100, 200, 300]);
    }

    #[test]
    fn mono_takes_every_sample() {
        let bytes = le_bytes(&[10, 20, 30, 40]);
        let mut out = Vec::new();
        extract_samples(&bytes, false, 1.0, false, &mut out);
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    #[test]
    fn gain_is_applied_and_clamped() {
        let bytes = le_bytes(&[10000, 20000]); // mono
        let mut out = Vec::new();
        extract_samples(&bytes, false, 2.0, true, &mut out);
        assert_eq!(out, vec![20000, i16::MAX]); // 20000*2 clamps to 32767
    }
}
