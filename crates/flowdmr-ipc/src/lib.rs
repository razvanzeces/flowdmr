//! FlowDMR IPC — the wire protocol shared by the FlowDMR sidecar (DMR receiver)
//! and the FlowStation-side injection entity.
//!
//! Transport is UDP on localhost. One datagram = one [`FlowDmrFrame`]. All
//! integers are little-endian. The protocol is deliberately tiny and
//! dependency-free so the sidecar and the in-process FlowStation entity can
//! never drift on the format.
//!
//! ## Frame layout
//!
//! Common header (8 bytes):
//! ```text
//!   off  field      type
//!   0    magic      u16   = 0xFD30
//!   2    version    u8    = 1
//!   3    kind       u8    (see FlowDmrKind)
//!   4    stream_id  u32   per-call correlation id assigned by the sidecar
//! ```
//!
//! Per-kind payload follows the header:
//! - `CallStart` : source_id u32, dmr_tg u32, target_gssi u32, priority u8
//! - `Voice`     : seq u32, flags u16, n_samples u16, pcm[n_samples] i16
//! - `SrcChange` : source_id u32
//! - `CallEnd`   : (empty)
//! - `Keepalive` : (empty)

#![forbid(unsafe_code)]

/// Magic marker at the start of every frame ("FlowDMR v3 era").
pub const FLOWDMR_MAGIC: u16 = 0xFD30;
/// Protocol version. Bump on any incompatible layout change.
pub const FLOWDMR_VERSION: u8 = 1;
/// PCM samples in one voice frame (30 ms @ 8 kHz). Matches the TETRA codec
/// frame size (`TETRA_PCM_SAMPLES_PER_FRAME = 240`).
pub const PCM_SAMPLES_PER_FRAME: usize = 240;

/// Voice flag: first voice frame of a call.
pub const VOICE_FLAG_FIRST: u16 = 1 << 0;
/// Voice flag: last voice frame of a call (end-of-transmission).
pub const VOICE_FLAG_LAST: u16 = 1 << 1;

const HEADER_LEN: usize = 8;

/// Frame discriminator (the `kind` byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FlowDmrKind {
    CallStart = 1,
    Voice = 2,
    CallEnd = 3,
    SrcChange = 4,
    Keepalive = 5,
}

impl FlowDmrKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::CallStart,
            2 => Self::Voice,
            3 => Self::CallEnd,
            4 => Self::SrcChange,
            5 => Self::Keepalive,
            _ => return None,
        })
    }
}

/// The body of a frame, discriminated by [`FlowDmrKind`].
#[derive(Debug, Clone, PartialEq)]
pub enum FlowDmrBody {
    /// A new DMR transmission started. The entity originates a local TETRA
    /// group call on `target_gssi` with a source ISSI derived from `source_id`.
    CallStart {
        /// DMR source / radio ID of the talker.
        source_id: u32,
        /// DMR talkgroup (informational — used for display/logging).
        dmr_tg: u32,
        /// Target TETRA GSSI to inject on. MUST be inside the cell's
        /// `local_ssi_ranges`; the entity rejects anything else.
        target_gssi: u32,
        /// TETRA call priority (0 = normal).
        priority: u8,
    },
    /// One 30 ms PCM frame (240 × i16, mono, 8 kHz).
    Voice {
        /// Monotonic per-call sequence number (for jitter ordering / logging).
        seq: u32,
        /// Bitfield of `VOICE_FLAG_*`.
        flags: u16,
        /// PCM samples; expected length [`PCM_SAMPLES_PER_FRAME`].
        pcm: Vec<i16>,
    },
    /// The DMR transmission ended (or sustained silence detected).
    CallEnd,
    /// The talker changed within the same DMR call (new source on same TG).
    SrcChange {
        /// New DMR source / radio ID.
        source_id: u32,
    },
    /// Liveness ping so the entity can report "sidecar connected".
    Keepalive,
}

/// A fully-decoded FlowDMR frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowDmrFrame {
    /// Per-call correlation id assigned by the sidecar (stable for the duration
    /// of one DMR transmission; new id on each new transmission).
    pub stream_id: u32,
    pub body: FlowDmrBody,
}

impl FlowDmrFrame {
    pub fn new(stream_id: u32, body: FlowDmrBody) -> Self {
        Self { stream_id, body }
    }

    pub fn kind(&self) -> FlowDmrKind {
        match self.body {
            FlowDmrBody::CallStart { .. } => FlowDmrKind::CallStart,
            FlowDmrBody::Voice { .. } => FlowDmrKind::Voice,
            FlowDmrBody::CallEnd => FlowDmrKind::CallEnd,
            FlowDmrBody::SrcChange { .. } => FlowDmrKind::SrcChange,
            FlowDmrBody::Keepalive => FlowDmrKind::Keepalive,
        }
    }

    /// Serialize into a freshly allocated byte buffer (one UDP datagram).
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN + 16);
        buf.extend_from_slice(&FLOWDMR_MAGIC.to_le_bytes());
        buf.push(FLOWDMR_VERSION);
        buf.push(self.kind() as u8);
        buf.extend_from_slice(&self.stream_id.to_le_bytes());

        match &self.body {
            FlowDmrBody::CallStart {
                source_id,
                dmr_tg,
                target_gssi,
                priority,
            } => {
                buf.extend_from_slice(&source_id.to_le_bytes());
                buf.extend_from_slice(&dmr_tg.to_le_bytes());
                buf.extend_from_slice(&target_gssi.to_le_bytes());
                buf.push(*priority);
            }
            FlowDmrBody::Voice { seq, flags, pcm } => {
                buf.extend_from_slice(&seq.to_le_bytes());
                buf.extend_from_slice(&flags.to_le_bytes());
                buf.extend_from_slice(&(pcm.len() as u16).to_le_bytes());
                for sample in pcm {
                    buf.extend_from_slice(&sample.to_le_bytes());
                }
            }
            FlowDmrBody::SrcChange { source_id } => {
                buf.extend_from_slice(&source_id.to_le_bytes());
            }
            FlowDmrBody::CallEnd | FlowDmrBody::Keepalive => {}
        }
        buf
    }

    /// Parse a frame from a received datagram.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(DecodeError::TooShort);
        }
        let magic = u16::from_le_bytes([bytes[0], bytes[1]]);
        if magic != FLOWDMR_MAGIC {
            return Err(DecodeError::BadMagic(magic));
        }
        let version = bytes[2];
        if version != FLOWDMR_VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let kind = FlowDmrKind::from_u8(bytes[3]).ok_or(DecodeError::BadKind(bytes[3]))?;
        let stream_id = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut r = Reader::new(&bytes[HEADER_LEN..]);

        let body = match kind {
            FlowDmrKind::CallStart => FlowDmrBody::CallStart {
                source_id: r.u32()?,
                dmr_tg: r.u32()?,
                target_gssi: r.u32()?,
                priority: r.u8()?,
            },
            FlowDmrKind::Voice => {
                let seq = r.u32()?;
                let flags = r.u16()?;
                let n = r.u16()? as usize;
                let mut pcm = Vec::with_capacity(n);
                for _ in 0..n {
                    pcm.push(r.i16()?);
                }
                FlowDmrBody::Voice { seq, flags, pcm }
            }
            FlowDmrKind::SrcChange => FlowDmrBody::SrcChange {
                source_id: r.u32()?,
            },
            FlowDmrKind::CallEnd => FlowDmrBody::CallEnd,
            FlowDmrKind::Keepalive => FlowDmrBody::Keepalive,
        };
        Ok(Self { stream_id, body })
    }
}

/// Errors returned by [`FlowDmrFrame::decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    BadMagic(u16),
    BadVersion(u8),
    BadKind(u8),
    Truncated,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "datagram shorter than header"),
            DecodeError::BadMagic(m) => write!(f, "bad magic 0x{m:04X}"),
            DecodeError::BadVersion(v) => write!(f, "unsupported version {v}"),
            DecodeError::BadKind(k) => write!(f, "unknown kind {k}"),
            DecodeError::Truncated => write!(f, "payload truncated"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Minimal little-endian byte reader.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        if end > self.buf.len() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16, DecodeError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: FlowDmrFrame) {
        let bytes = frame.encode();
        let decoded = FlowDmrFrame::decode(&bytes).expect("decode");
        assert_eq!(frame, decoded);
    }

    #[test]
    fn call_start_round_trip() {
        round_trip(FlowDmrFrame::new(
            42,
            FlowDmrBody::CallStart {
                source_id: 2_604_001,
                dmr_tg: 9,
                target_gssi: 5000,
                priority: 0,
            },
        ));
    }

    #[test]
    fn voice_round_trip() {
        let pcm: Vec<i16> = (0..PCM_SAMPLES_PER_FRAME as i32)
            .map(|i| (i * 17 - 2000) as i16)
            .collect();
        round_trip(FlowDmrFrame::new(
            7,
            FlowDmrBody::Voice {
                seq: 12345,
                flags: VOICE_FLAG_FIRST,
                pcm,
            },
        ));
    }

    #[test]
    fn control_frames_round_trip() {
        round_trip(FlowDmrFrame::new(1, FlowDmrBody::CallEnd));
        round_trip(FlowDmrFrame::new(1, FlowDmrBody::Keepalive));
        round_trip(FlowDmrFrame::new(
            1,
            FlowDmrBody::SrcChange { source_id: 999 },
        ));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = FlowDmrFrame::new(1, FlowDmrBody::Keepalive).encode();
        // Zero the low magic byte: 0xFD30 -> 0xFD00 (little-endian).
        bytes[0] = 0;
        assert_eq!(FlowDmrFrame::decode(&bytes), Err(DecodeError::BadMagic(0xFD00)));
    }

    #[test]
    fn rejects_truncated_voice() {
        let mut bytes = FlowDmrFrame::new(
            1,
            FlowDmrBody::Voice {
                seq: 1,
                flags: 0,
                pcm: vec![1, 2, 3],
            },
        )
        .encode();
        bytes.truncate(bytes.len() - 2);
        assert_eq!(FlowDmrFrame::decode(&bytes), Err(DecodeError::Truncated));
    }

    #[test]
    fn voice_frame_fits_one_datagram() {
        let pcm = vec![0i16; PCM_SAMPLES_PER_FRAME];
        let bytes = FlowDmrFrame::new(1, FlowDmrBody::Voice { seq: 0, flags: 0, pcm }).encode();
        // 8 header + 8 voice-subheader + 480 PCM bytes = 496, well under MTU.
        assert_eq!(bytes.len(), 8 + 8 + PCM_SAMPLES_PER_FRAME * 2);
        assert!(bytes.len() < 1400);
    }
}
