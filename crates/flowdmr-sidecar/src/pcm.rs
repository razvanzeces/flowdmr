//! PCM ingest: receive the decoder's 8 kHz s16 audio over UDP and reframe it
//! into fixed 240-sample (30 ms) frames for the call manager.
//!
//! dsd-neo's `-o udp:...` emits a raw little-endian s16 mono stream split across
//! datagrams of arbitrary length, so we accumulate samples and emit whole frames.

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
    let mut acc: Vec<i16> = Vec::with_capacity(PCM_SAMPLES_PER_FRAME * 4);

    loop {
        let len = match socket.recv(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("flowdmr-sidecar: PCM recv error: {e}");
                continue;
            }
        };
        // Decode whole s16 little-endian samples.
        let mut i = 0;
        while i + 1 < len {
            acc.push(i16::from_le_bytes([buf[i], buf[i + 1]]));
            i += 2;
        }

        while acc.len() >= PCM_SAMPLES_PER_FRAME {
            let frame: Vec<i16> = acc.drain(..PCM_SAMPLES_PER_FRAME).collect();
            let now_ms = start.elapsed().as_millis() as u64;
            let tg = cfg.live().injection_tg;
            {
                let mut mgr = cm.lock().expect("cm lock");
                mgr.on_pcm(frame, tg, now_ms, &mut sink);
            }
            status.update(|s| {
                s.pcm_frames += 1;
                s.seconds_since_pcm = Some(0);
            });
        }
    }
}
