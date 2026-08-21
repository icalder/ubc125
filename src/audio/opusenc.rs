//! Opus frame encoder: wraps `opus::Encoder` with the fixed settings the
//! scanner audio pipeline uses — 48 kHz mono, 20 ms frames (960 samples),
//! 24 kbps, `Application::Audio`.
//!
//! The encoder's native sample rate is 48 kHz, so the native capture path
//! (ALSA at 48 kHz) feeds it directly with no resampling.

use opus::{Application, Bitrate, Channels, Encoder, Error as OpusError};

/// Samples in one 20 ms Opus frame at 48 kHz.
pub const FRAME_SAMPLES: usize = 960;
/// Maximum encoded packet size for a single 20 ms frame (RFC 6716 §3.3).
const MAX_PACKET_SIZE: usize = 1275;
/// Default bitrate: 24 kbps, matching the old ffmpeg `-b:a 24k`.
const DEFAULT_BITRATE_BPS: u32 = 24_000;

/// Encodes 960-sample mono frames at 48 kHz into Opus packets.
pub struct OpusFrameEncoder {
    encoder: Encoder,
}

impl OpusFrameEncoder {
    /// Create an encoder at the default 24 kbps.
    pub fn new() -> Result<Self, OpusError> {
        Self::with_bitrate(DEFAULT_BITRATE_BPS)
    }

    /// Create an encoder at an explicit bitrate in bits/s.
    pub fn with_bitrate(bitrate_bps: u32) -> Result<Self, OpusError> {
        let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Audio)?;
        encoder.set_bitrate(Bitrate::Bits(bitrate_bps as i32))?;
        Ok(Self { encoder })
    }

    /// Encode exactly one 960-sample frame into an Opus packet.
    ///
    /// `samples.len()` must be [`FRAME_SAMPLES`]; any other length is
    /// rejected by the codec with a `BadArg` error.
    pub fn encode_frame(&mut self, samples: &[i16]) -> Result<Vec<u8>, OpusError> {
        debug_assert_eq!(samples.len(), FRAME_SAMPLES);
        self.encoder.encode_vec(samples, MAX_PACKET_SIZE)
    }
}

impl Default for OpusFrameEncoder {
    fn default() -> Self {
        Self::new().expect("default opus encoder configuration is always valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opus::Decoder;

    /// 440 Hz sine, amplitude `amp` (-1.0..1.0), 48 kHz.
    fn sine(freq: f32, amp: f32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| (amp * (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin())
                * i16::MAX as f32)
            .map(|s| s.round() as i16)
            .collect()
    }

    fn rms(samples: &[i16]) -> f32 {
        let sum: f64 = samples.iter().map(|s| *s as f64 * *s as f64).sum();
        (sum / samples.len() as f64).sqrt() as f32
    }

    fn decode_packet(packet: &[u8]) -> Vec<i16> {
        let mut decoder = Decoder::new(48_000, Channels::Mono).expect("decoder");
        let mut out = vec![0i16; FRAME_SAMPLES];
        let n = decoder
            .decode(packet, &mut out, false)
            .expect("decode");
        out.truncate(n);
        out
    }

    /// G5: tone frames encode and decode back to 960 samples with signal,
    /// and the TOC byte says "single 20 ms frame, mono".
    #[test]
    fn tone_round_trip() {
        let mut encoder = OpusFrameEncoder::new().expect("encoder");
        let mut decoder = Decoder::new(48_000, Channels::Mono).expect("decoder");
        let frame = sine(440.0, 0.5, FRAME_SAMPLES);
        let mut tocs: std::collections::HashMap<u8, usize> = Default::default();
        for _ in 0..1000 {
            let packet = encoder.encode_frame(&frame).expect("encode");
            assert!(!packet.is_empty());
            // TOC (RFC 6716): config (bits 7-3), stereo flag s (bit 2),
            // frame-count code c (bits 1-0). Every packet must be a single
            // 20 ms mono frame; libopus varies the config (mode/bandwidth),
            // observed 0x78 (Hybrid FB) and 0xF8 (CELT FB 20 ms).
            let toc = packet[0];
            let config = toc >> 3;
            assert!(
                matches!(config, 1 | 5 | 9 | 13 | 15 | 19 | 23 | 27 | 31),
                "TOC {toc:#04x} config {config} must encode a 20 ms frame"
            );
            assert!(toc & 0x04 == 0, "TOC {toc:#04x} must be mono (s=0)");
            assert!(toc & 0x03 == 0, "TOC {toc:#04x} must be code 0 (one frame)");
            *tocs.entry(toc).or_insert(0) += 1;
            let mut out = vec![0i16; FRAME_SAMPLES];
            let n = decoder.decode(&packet, &mut out, false).expect("decode");
            assert_eq!(n, FRAME_SAMPLES, "decoded sample count");
            out.truncate(n);
            assert!(
                rms(&out) > 100.0,
                "decoded tone RMS {} must be well above silence",
                rms(&out)
            );
        }
        assert!(!tocs.is_empty());
    }

    /// G6: 5 s of tone at the default bitrate averages within 50 % of the
    /// 24 kbps target (~75 bytes/frame). Tones encode smaller than noise;
    /// the bound is generous and only catches a misconfigured encoder.
    #[test]
    fn bitrate_near_target() {
        let mut encoder = OpusFrameEncoder::new().expect("encoder");
        let frame = sine(440.0, 0.5, FRAME_SAMPLES);
        let frames = 250; // 5 s
        let mut total = 0usize;
        for _ in 0..frames {
            total += encoder.encode_frame(&frame).expect("encode").len();
        }
        let target = 24_000u64 / 8 / 50; // ~75 bytes per 20 ms frame
        let avg = total as u64 / frames as u64;
        assert!(
            avg >= target / 2 && avg <= target * 3 / 2,
            "average packet size {avg} B outside 50 % of {target} B"
        );
    }

    /// G7: digital silence encodes to valid packets decoding to ~0 RMS, and
    /// a full-scale tone does not make the encoder fail or the decoder
    /// overflow.
    #[test]
    fn silence_and_full_scale() {
        let mut encoder = OpusFrameEncoder::new().expect("encoder");
        let silence = vec![0i16; FRAME_SAMPLES];
        for _ in 0..50 {
            let packet = encoder.encode_frame(&silence).expect("encode silence");
            let out = decode_packet(&packet);
            assert!(rms(&out) < 10.0, "decoded silence RMS must be ~0");
        }
        let full = sine(440.0, 1.0, FRAME_SAMPLES);
        for _ in 0..50 {
            let packet = encoder.encode_frame(&full).expect("encode full scale");
            let out = decode_packet(&packet);
            assert_eq!(out.len(), FRAME_SAMPLES);
        }
    }
}
