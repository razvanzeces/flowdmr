//! TETRA ACELP vocoder: turns 8 kHz PCM into 274-bit TMD voice blocks.
//!
//! The real encoder is FlowStation's native `libtetra-codec` (the same library
//! the Asterisk bridge links). One `tetra_encode` call consumes 240 PCM samples
//! (30 ms) and produces a 137-bit coded frame; two coded frames are bit-packed
//! into a 35-byte (274-bit) TMD block — exactly the byte layout UMAC expects in
//! `TmdCircuitDataReq.data`.
//!
//! With the `codec-stub` feature the encoder is a pure-Rust no-op that emits
//! silent blocks, so the whole pipeline builds and runs on a laptop without the
//! proprietary codec (audio will be silent — for wiring/timing tests only).

/// PCM samples per codec frame (30 ms @ 8 kHz).
pub const PCM_SAMPLES_PER_FRAME: usize = 240;
/// Coded bits per ACELP frame.
const CODED_BITS_PER_FRAME: usize = 137;
/// Coded bytes per ACELP frame (ceil(137/8)).
const CODED_BYTES_PER_FRAME: usize = (CODED_BITS_PER_FRAME + 7) / 8; // 18
/// Bits in one TMD block (two ACELP frames).
const TMD_BITS_PER_BLOCK: usize = CODED_BITS_PER_FRAME * 2; // 274
/// Packed bytes in one TMD block (ceil(274/8)).
pub const TMD_PACKED_BYTES: usize = (TMD_BITS_PER_BLOCK + 7) / 8; // 35

/// A TETRA voice vocoder instance (one per active call).
pub struct Vocoder {
    inner: Inner,
}

impl Vocoder {
    pub fn new() -> Option<Self> {
        Inner::new().map(|inner| Self { inner })
    }

    /// Encode one 60 ms block: two 240-sample PCM frames -> 35-byte TMD block.
    pub fn encode_block(
        &mut self,
        pcm_a: &[i16; PCM_SAMPLES_PER_FRAME],
        pcm_b: &[i16; PCM_SAMPLES_PER_FRAME],
    ) -> [u8; TMD_PACKED_BYTES] {
        let coded_a = self.inner.encode_frame(pcm_a);
        let coded_b = self.inner.encode_frame(pcm_b);
        join_codec_frames_to_tmd_block(&coded_a, &coded_b)
    }
}

/// Pack two 137-bit coded frames into a 274-bit (35-byte) TMD block.
fn join_codec_frames_to_tmd_block(
    frame_a: &[u8; CODED_BYTES_PER_FRAME],
    frame_b: &[u8; CODED_BYTES_PER_FRAME],
) -> [u8; TMD_PACKED_BYTES] {
    let mut out = [0u8; TMD_PACKED_BYTES];
    for bit_idx in 0..TMD_BITS_PER_BLOCK {
        let (frame, frame_bit) = if bit_idx < CODED_BITS_PER_FRAME {
            (frame_a, bit_idx)
        } else {
            (frame_b, bit_idx - CODED_BITS_PER_FRAME)
        };
        set_packed_bit(&mut out, bit_idx, get_packed_bit(frame, frame_bit));
    }
    out
}

#[inline]
fn get_packed_bit(data: &[u8], bit_idx: usize) -> u8 {
    (data[bit_idx / 8] >> (7 - (bit_idx % 8))) & 1
}

#[inline]
fn set_packed_bit(data: &mut [u8], bit_idx: usize, bit: u8) {
    if bit & 1 != 0 {
        data[bit_idx / 8] |= 1 << (7 - (bit_idx % 8));
    }
}

// ─── Real native codec (FFI) ─────────────────────────────────────────────────
#[cfg(not(feature = "codec-stub"))]
mod imp {
    use super::{CODED_BYTES_PER_FRAME, PCM_SAMPLES_PER_FRAME};
    use std::ptr::NonNull;

    #[repr(C)]
    struct RawTetraCodec {
        _private: [u8; 0],
    }

    #[link(name = "tetra-codec")]
    unsafe extern "C" {
        fn tetra_encoder_create() -> *mut RawTetraCodec;
        fn tetra_codec_destroy(st: *mut RawTetraCodec);
        fn tetra_encode(st: *mut RawTetraCodec, pcm: *const i16, coded: *mut u8);
    }

    pub(super) struct Inner {
        ptr: NonNull<RawTetraCodec>,
    }

    // The encoder is owned by exactly one call and only ever touched on the
    // entity thread through &mut self. Moving the owner between threads is safe.
    unsafe impl Send for Inner {}

    impl Inner {
        pub(super) fn new() -> Option<Self> {
            let ptr = unsafe { tetra_encoder_create() };
            NonNull::new(ptr).map(|ptr| Self { ptr })
        }

        pub(super) fn encode_frame(
            &mut self,
            pcm: &[i16; PCM_SAMPLES_PER_FRAME],
        ) -> [u8; CODED_BYTES_PER_FRAME] {
            let mut coded = [0u8; CODED_BYTES_PER_FRAME];
            unsafe {
                tetra_encode(self.ptr.as_ptr(), pcm.as_ptr(), coded.as_mut_ptr());
            }
            coded
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            unsafe { tetra_codec_destroy(self.ptr.as_ptr()) }
        }
    }
}

// ─── Stub codec (no native lib) ──────────────────────────────────────────────
#[cfg(feature = "codec-stub")]
mod imp {
    use super::{CODED_BYTES_PER_FRAME, PCM_SAMPLES_PER_FRAME};

    pub(super) struct Inner;

    impl Inner {
        pub(super) fn new() -> Option<Self> {
            tracing::warn!(
                "flowdmr-entity: using STUB codec (feature codec-stub) — injected audio will be SILENT"
            );
            Some(Inner)
        }

        pub(super) fn encode_frame(
            &mut self,
            _pcm: &[i16; PCM_SAMPLES_PER_FRAME],
        ) -> [u8; CODED_BYTES_PER_FRAME] {
            [0u8; CODED_BYTES_PER_FRAME]
        }
    }
}

use imp::Inner;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_is_35_bytes() {
        assert_eq!(TMD_PACKED_BYTES, 35);
        assert_eq!(CODED_BYTES_PER_FRAME, 18);
    }

    #[test]
    fn bit_pack_round_trips_274_bits() {
        // Fill two frames with a deterministic pattern, join, and verify each
        // packed bit matches the source frame bit.
        let mut a = [0u8; CODED_BYTES_PER_FRAME];
        let mut b = [0u8; CODED_BYTES_PER_FRAME];
        for i in 0..CODED_BITS_PER_FRAME {
            if i % 3 == 0 {
                set_packed_bit(&mut a, i, 1);
            }
            if i % 2 == 0 {
                set_packed_bit(&mut b, i, 1);
            }
        }
        let block = join_codec_frames_to_tmd_block(&a, &b);
        for i in 0..TMD_BITS_PER_BLOCK {
            let (frame, fb) = if i < CODED_BITS_PER_FRAME {
                (&a, i)
            } else {
                (&b, i - CODED_BITS_PER_FRAME)
            };
            assert_eq!(get_packed_bit(&block, i), get_packed_bit(frame, fb), "bit {i}");
        }
    }
}
