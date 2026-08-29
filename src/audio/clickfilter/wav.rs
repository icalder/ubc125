//! WAV I/O for mono, 48 kHz, signed 16-bit little-endian PCM.
//!
//! Port of the audio half of `../ubc125-ml/scripts/clickfilter/io.py`. The header the writer
//! emits is the header Python's `wave` module emits, so a Rust output file can
//! be compared byte for byte against the reference rig's artifact. No
//! third-party audio crate: the deployment path is direct arithmetic (rule 11).

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::audio::clickfilter::constants::RATE;

#[derive(Debug)]
pub enum WavError {
    Io(std::io::Error),
    NotReadable { path: String, reason: String },
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavError::Io(err) => write!(f, "{err}"),
            WavError::NotReadable { path, reason } => write!(f, "{path}: {reason}"),
        }
    }
}

impl std::error::Error for WavError {}

impl From<std::io::Error> for WavError {
    fn from(err: std::io::Error) -> Self {
        WavError::Io(err)
    }
}

/// Read a mono S16_LE 48 kHz WAV as i16 samples.
pub fn read_wav(path: &Path) -> Result<Vec<i16>, WavError> {
    let bytes = fs::read(path).map_err(|err| WavError::NotReadable {
        path: path.display().to_string(),
        reason: format!("{err}\nset UBC125_ML_DATA or pass --file"),
    })?;
    let chunks = Riff::parse(&bytes)?;
    chunks.format()?;
    let data = chunks.data()?;
    let mut samples = Vec::with_capacity(data.len() / 2);
    for pair in data.as_chunks::<2>().0 {
        samples.push(i16::from_le_bytes([pair[0], pair[1]]));
    }
    Ok(samples)
}

/// Write i16 samples as a mono S16_LE WAV at `rate`.
pub fn write_wav(path: &Path, samples: &[i16], rate: i32) -> Result<(), WavError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    file.write_all(&wav_bytes(samples, rate))?;
    Ok(())
}

/// The complete WAV image, header included.
pub fn wav_bytes(samples: &[i16], rate: i32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let byte_rate = rate as u32 * u32::from(block_align);
    let mut out: Vec<u8> = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&(rate as u32).to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// `write_wav` at the project sample rate.
pub fn write_reference_wav(path: &Path, samples: &[i16]) -> Result<(), WavError> {
    write_wav(path, samples, RATE as i32)
}

struct Riff<'a> {
    chunks: Vec<(&'a [u8; 4], &'a [u8])>,
}

impl<'a> Riff<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, WavError> {
        if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(WavError::NotReadable {
                path: String::new(),
                reason: "expected a RIFF WAVE file".into(),
            });
        }
        let mut body = &bytes[12..];
        let mut chunks = Vec::new();
        while body.len() >= 8 {
            let id: &[u8; 4] = body[0..4].try_into().expect("four bytes");
            let size = u32::from_le_bytes(body[4..8].try_into().expect("four bytes")) as usize;
            let data_start = 8;
            let end = (data_start + size).min(body.len());
            chunks.push((id, &body[data_start..end]));
            // Chunks are word-aligned; an odd size is followed by a pad byte.
            body = &body[(data_start + size + (size & 1)).min(body.len())..];
        }
        Ok(Riff { chunks })
    }

    fn find(&self, id: &[u8; 4]) -> Option<&'a [u8]> {
        self.chunks
            .iter()
            .find(|(chunk_id, _)| *chunk_id == id)
            .map(|(_, data)| *data)
    }

    fn format(&self) -> Result<(), WavError> {
        let fmt = self.find(b"fmt ").ok_or_else(|| WavError::NotReadable {
            path: String::new(),
            reason: "no fmt chunk".into(),
        })?;
        if fmt.len() < 16 {
            return Err(WavError::NotReadable {
                path: String::new(),
                reason: "truncated fmt chunk".into(),
            });
        }
        let format = &fmt[0..2];
        let channels = &fmt[2..4];
        let rate = &fmt[4..8];
        let bits = &fmt[14..16];
        if u16::from_le_bytes(format.try_into().unwrap()) != 1 {
            return Err(WavError::NotReadable {
                path: String::new(),
                reason: "expected PCM (uncompressed)".into(),
            });
        }
        if u16::from_le_bytes(channels.try_into().unwrap()) != 1 || bits != [16, 0] {
            return Err(WavError::NotReadable {
                path: String::new(),
                reason: "expected mono S16_LE".into(),
            });
        }
        if u32::from_le_bytes(rate.try_into().unwrap()) != RATE as u32 {
            return Err(WavError::NotReadable {
                path: String::new(),
                reason: format!("expected {} Hz", RATE as u32),
            });
        }
        Ok(())
    }

    fn data(&self) -> Result<&'a [u8], WavError> {
        let data = self.find(b"data").ok_or_else(|| WavError::NotReadable {
            path: String::new(),
            reason: "no data chunk".into(),
        })?;
        if data.len() % 2 != 0 {
            return Err(WavError::NotReadable {
                path: String::new(),
                reason: "odd data chunk for S16_LE".into(),
            });
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_wav(bytes: &[u8], name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ubc125-{name}.wav"));
        fs::write(&path, bytes).expect("write temp");
        path
    }

    #[test]
    fn header_matches_pythons_wave_module() {
        // The first 44 bytes of every artifact in artifacts/declick/ are these
        // fields; CPython's wave writer puts nothing else in the header.
        let expected: Vec<u8> = [
            b"RIFF".to_vec(),
            vec![42, 0, 0, 0],
            b"WAVE".to_vec(),
            b"fmt ".to_vec(),
            vec![16, 0, 0, 0],
            vec![1, 0],                // PCM
            vec![1, 0],                // mono
            vec![0x80, 0xbb, 0, 0],    // 48000 Hz
            vec![0x00, 0x77, 0x01, 0], // 96000 byte/s
            vec![2, 0],                // block align
            vec![16, 0],               // bits
            b"data".to_vec(),
            vec![6, 0, 0, 0],
        ]
        .concat();
        let image = wav_bytes(&[1, -2, 3], 48000);
        assert_eq!(&image[..44], expected.as_slice());
        assert_eq!(&image[44..], &[1, 0, 0xfe, 0xff, 3, 0]);
    }

    #[test]
    fn round_trip_recovers_the_samples() {
        let samples: Vec<i16> = (0..1000).map(|i| (i - 500) as i16).collect();
        let image = wav_bytes(&samples, 48000);
        let path = temp_wav(&image, "roundtrip");
        let got = read_wav(&path).expect("read");
        let _ = fs::remove_file(&path);
        assert_eq!(got, samples);
    }

    #[test]
    fn a_stereo_file_is_refused() {
        let mut image = wav_bytes(&[0; 4], 48000);
        image[22] = 2; // nchannels
        let path = temp_wav(&image, "stereo");
        let err = read_wav(&path).expect_err("stereo must be refused");
        let _ = fs::remove_file(&path);
        assert!(format!("{err}").contains("mono"));
    }

    #[test]
    fn a_wrong_rate_is_refused() {
        let image = wav_bytes(&[0; 4], 16000);
        let path = temp_wav(&image, "rate");
        let err = read_wav(&path).expect_err("16 kHz must be refused");
        let _ = fs::remove_file(&path);
        assert!(format!("{err}").contains("48000"));
    }
}
