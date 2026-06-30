//! Per-call WAV recorder. One file per DMR transmission (PTT cycle), named
//! `<localtime>_tg<talkgroup>_src<source>.wav`, 8 kHz / 16-bit / mono.
//!
//! WAV is used because it needs no external encoder (mp3/mp4 would require
//! lame/ffmpeg) — we just write a 44-byte header + the raw PCM and patch the
//! size fields when the call ends. The file is written to `<name>.part` while
//! the call is live and renamed on finish, once the real source/talkgroup are
//! known.

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SAMPLE_RATE: u32 = 8000;

struct Active {
    file: File,
    part_path: PathBuf,
    samples: u32,
    started: chrono::DateTime<chrono::Local>,
    src: u32,
    tg: u32,
}

/// Records decoded call audio to per-call WAV files. A `None` dir = disabled
/// (no-op), which is what the unit tests use.
pub struct Recorder {
    dir: Option<PathBuf>,
    min_samples: u32,
    active: Option<Active>,
    pub files_written: u64,
    pub last_file: Option<String>,
}

impl Recorder {
    pub fn disabled() -> Self {
        Self { dir: None, min_samples: 0, active: None, files_written: 0, last_file: None }
    }

    /// `dir` = where to drop the .wav files; `min_secs` skips calls shorter than that.
    pub fn new(dir: &str, min_secs: f32) -> Self {
        let path = PathBuf::from(dir);
        if let Err(e) = fs::create_dir_all(&path) {
            tracing::warn!("flowdmr-sidecar: cannot create recordings dir {dir}: {e}");
            return Self::disabled();
        }
        Self {
            dir: Some(path),
            min_samples: (min_secs.max(0.0) * SAMPLE_RATE as f32) as u32,
            active: None,
            files_written: 0,
            last_file: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    /// Begin a recording. `src`/`tg` are the best-known DMR ids at start (0 if not
    /// yet decoded); refine with [`note_meta`]. Any in-progress file is finished first.
    pub fn start(&mut self, src: u32, tg: u32) {
        let Some(dir) = self.dir.clone() else { return };
        self.finish();
        let part_path = dir.join(".rec_current.part");
        let file = match File::create(&part_path) {
            Ok(mut f) => {
                if write_wav_header(&mut f, 0).is_err() {
                    return;
                }
                f
            }
            Err(e) => {
                tracing::warn!("flowdmr-sidecar: cannot open recording file: {e}");
                return;
            }
        };
        self.active = Some(Active {
            file,
            part_path,
            samples: 0,
            started: chrono::Local::now(),
            src,
            tg,
        });
    }

    /// Update the call's source/talkgroup as they get decoded (keeps last non-zero).
    pub fn note_meta(&mut self, src: Option<u32>, tg: Option<u32>) {
        if let Some(a) = self.active.as_mut() {
            if let Some(s) = src {
                if s != 0 {
                    a.src = s;
                }
            }
            if let Some(t) = tg {
                if t != 0 {
                    a.tg = t;
                }
            }
        }
    }

    /// Append one frame of PCM samples.
    pub fn write(&mut self, pcm: &[i16]) {
        if let Some(a) = self.active.as_mut() {
            let mut buf = Vec::with_capacity(pcm.len() * 2);
            for s in pcm {
                buf.extend_from_slice(&s.to_le_bytes());
            }
            if a.file.write_all(&buf).is_ok() {
                a.samples += pcm.len() as u32;
            }
        }
    }

    /// Finalize the current recording: patch the WAV sizes and rename to the
    /// final `<time>_tg<tg>_src<src>.wav`. Drops calls shorter than `min_samples`.
    pub fn finish(&mut self) {
        let Some(mut a) = self.active.take() else { return };
        let Some(dir) = self.dir.clone() else { return };

        if a.samples < self.min_samples {
            drop(a.file);
            let _ = fs::remove_file(&a.part_path);
            return;
        }
        if patch_wav_sizes(&mut a.file, a.samples).is_err() {
            tracing::warn!("flowdmr-sidecar: failed to finalize recording header");
        }
        drop(a.file);

        let name = format!(
            "{}_tg{}_src{}.wav",
            a.started.format("%Y%m%d-%H%M%S"),
            a.tg,
            a.src
        );
        let final_path = dir.join(&name);
        match fs::rename(&a.part_path, &final_path) {
            Ok(_) => {
                self.files_written += 1;
                self.last_file = Some(name);
                tracing::info!(
                    "flowdmr-sidecar: saved recording {} ({:.1}s)",
                    final_path.display(),
                    a.samples as f32 / SAMPLE_RATE as f32
                );
            }
            Err(e) => tracing::warn!("flowdmr-sidecar: cannot rename recording: {e}"),
        }
    }
}

fn write_wav_header(f: &mut File, data_len: u32) -> std::io::Result<()> {
    let byte_rate = SAMPLE_RATE * 2; // mono, 16-bit
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // audio format = PCM
    f.write_all(&1u16.to_le_bytes())?; // channels = 1
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    Ok(())
}

fn patch_wav_sizes(f: &mut File, samples: u32) -> std::io::Result<()> {
    let data_len = samples * 2;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.seek(SeekFrom::Start(40))?;
    f.write_all(&data_len.to_le_bytes())?;
    f.flush()
}

/// List `.wav` recordings in `dir`, newest first: (filename, bytes).
pub fn list_recordings(dir: &Path, limit: usize) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64, std::time::SystemTime)> = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".wav") {
            continue;
        }
        if let Ok(m) = e.metadata() {
            let mtime = m.modified().unwrap_or(std::time::UNIX_EPOCH);
            v.push((name, m.len(), mtime));
        }
    }
    v.sort_by_key(|x| std::cmp::Reverse(x.2)); // newest first
    v.truncate(limit);
    v.into_iter().map(|(n, sz, _)| (n, sz)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_noop() {
        let mut r = Recorder::disabled();
        r.start(1, 2);
        r.write(&[0i16; 240]);
        r.finish();
        assert_eq!(r.files_written, 0);
        assert!(!r.enabled());
    }

    #[test]
    fn writes_a_valid_wav() {
        let dir = std::env::temp_dir().join(format!("flowdmr-rec-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut r = Recorder::new(dir.to_str().unwrap(), 0.0);
        assert!(r.enabled());
        r.start(0, 9);
        r.note_meta(Some(2604123), Some(9));
        for _ in 0..10 {
            r.write(&[1000i16; 240]);
        }
        r.finish();
        assert_eq!(r.files_written, 1);

        let files = list_recordings(&dir, 10);
        assert_eq!(files.len(), 1);
        let (name, size) = &files[0];
        assert!(name.contains("tg9") && name.contains("src2604123"), "name: {name}");
        // 2400 samples * 2 bytes + 44 header
        assert_eq!(*size, 2400 * 2 + 44);
        let bytes = fs::read(dir.join(name)).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let _ = fs::remove_dir_all(&dir);
    }
}
