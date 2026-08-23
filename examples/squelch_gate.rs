//! Offline squelch-gate harness: runs the streaming [`SquelchGate`] frame
//! by frame (960-sample chunks, exactly the production call path) over a
//! 48 kHz mono s16 WAV, writes the gated audio, and prints the state
//! transitions on stderr for ear-cross-checking.
//!
//! Usage:
//!   squelch_gate <in.wav> <out.wav> [--close DB] [--reopen DB] [--confirm MS]
//!       [--fade MS] [--fade-out MS] [--delay FRAMES]
//!
//! Defaults: the [`SquelchGateConfig::default`] measured on raw60.wav
//! (close −45, reopen −42, 20 ms confirm, 20 ms fade-in/out, 2-frame delay).

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use ubc125::audio::{PcmFrameFilter, SquelchGate, SquelchGateConfig};

const FRAME: usize = 960; // 20 ms @ 48 kHz
const RATE: u32 = 48_000;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut in_path = None;
    let mut out_path = None;
    let mut cfg = SquelchGateConfig::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--close" => {
                i += 1;
                cfg.close_db = args[i].parse().expect("--close takes a dBFS value");
            }
            "--reopen" => {
                i += 1;
                cfg.reopen_db = args[i].parse().expect("--reopen takes a dBFS value");
            }
            "--confirm" => {
                i += 1;
                cfg.close_confirm_ms = args[i].parse().expect("--confirm takes ms");
            }
            "--fade" => {
                i += 1;
                cfg.fade_ms = args[i].parse().expect("--fade takes ms");
            }
            "--fade-out" => {
                i += 1;
                cfg.fade_out_ms = args[i].parse().expect("--fade-out takes ms");
            }
            "--delay" => {
                i += 1;
                cfg.delay_frames = args[i].parse().expect("--delay takes frames");
            }
            a if a.starts_with("--") => {
                eprintln!("unknown flag: {a}");
                return ExitCode::FAILURE;
            }
            a if in_path.is_none() => in_path = Some(a),
            a if out_path.is_none() => out_path = Some(a),
            _ => {
                eprintln!("too many positional arguments");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }
    let (Some(in_path), Some(out_path)) = (in_path, out_path) else {
        eprintln!("usage: squelch_gate <in.wav> <out.wav> [--close DB] [--reopen DB] [--confirm MS] [--fade MS] [--fade-out MS] [--delay FRAMES]");
        return ExitCode::FAILURE;
    };

    let samples = match read_wav(&in_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read {in_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut gate = SquelchGate::new(cfg);
    let delay_frames = gate.latency_frames();
    eprintln!(
        "in: {in_path}  {} samples ({} s)  cfg: close={:.1} reopen={:.1} confirm={}ms fade_in={}ms fade_out={}ms delay={} frames",
        samples.len(),
        samples.len() as f64 / RATE as f64,
        cfg.close_db,
        cfg.reopen_db,
        cfg.close_confirm_ms,
        cfg.fade_ms,
        if cfg.fade_out_ms == 0 {
            cfg.fade_ms
        } else {
            cfg.fade_out_ms.max(cfg.fade_ms)
        },
        delay_frames,
    );

    let mut out = Vec::with_capacity(samples.len());
    let mut open_time = 0u64;
    let mut opens = 0;
    let mut was_open = false;
    for chunk in samples.chunks_exact(FRAME) {
        let mut frame = chunk.to_vec();
        let db = SquelchGate::frame_dbfs(&frame);
        gate.process_frame(&mut frame);
        let is_open = gate.is_open();
        let t = out.len() as f64 / RATE as f64;
        if is_open != was_open {
            eprintln!(
                "t={t:8.3}s  {}  (frame peak {db:.1} dBFS)",
                if is_open { "OPEN" } else { "CLOSE" }
            );
            if is_open {
                opens += 1;
            }
            was_open = is_open;
        }
        if is_open {
            open_time += frame.len() as u64;
        }
        out.extend(frame);
    }
    // The gate deliberately adds a fixed look-ahead delay. Drain its tail
    // so an offline render has the same sample count as its input.
    for frame in gate.flush() {
        out.extend(frame);
    }
    // Remove the startup latency from an offline render. This keeps the WAV
    // aligned with the input while retaining the final delayed frames.
    let delay_samples = delay_frames * FRAME;
    if out.len() > samples.len() {
        out.drain(..delay_samples.min(out.len()));
        out.truncate(samples.len());
    }

    if let Err(e) = write_wav(Path::new(out_path), &out) {
        eprintln!("write {out_path}: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!(
        "out: {out_path}  {} opens  open {:.1}% of capture",
        opens,
        100.0 * open_time as f64 / out.len() as f64,
    );
    ExitCode::SUCCESS
}

/// A read cursor over a byte slice.
struct Cursor<'a> {
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn tag(&mut self) -> Result<[u8; 4], String> {
        if self.rest.len() < 4 {
            return Err("truncated RIFF".into());
        }
        let t: [u8; 4] = self.rest[..4].try_into().unwrap();
        self.rest = &self.rest[4..];
        Ok(t)
    }

    fn u32(&mut self) -> Result<u32, String> {
        if self.rest.len() < 4 {
            return Err("truncated RIFF".into());
        }
        let v = u32::from_le_bytes(self.tag()?);
        Ok(v)
    }
}

/// Minimal RIFF/WAVE reader: 48 kHz mono 16-bit PCM only.
fn read_wav(path: &str) -> Result<Vec<i16>, String> {
    let b = fs::read(path).map_err(|e| e.to_string())?;
    let mut c = Cursor { rest: b.as_slice() };
    if c.tag()? != *b"RIFF" {
        return Err("not a RIFF/WAVE file".into());
    }
    let _riff_size = c.u32()?;
    if c.tag()? != *b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut rate = 0u32;
    let mut channels = 0u16;
    let mut bits = 0u16;
    let mut data: Option<&[u8]> = None;
    while c.rest.len() >= 8 {
        let id = c.tag()?;
        let size = c.u32()? as usize;
        if c.rest.len() < size {
            return Err("truncated chunk".into());
        }
        let body = &c.rest[..size];
        c.rest = &c.rest[size + size % 2..];
        match &id[..] {
            b"fmt " => {
                let _ = u16::from_le_bytes([body[0], body[1]]); // PCM = 1
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                bits = u16::from_le_bytes([body[14], body[15]]);
            }
            b"data" => data = Some(body),
            _ => {}
        }
    }
    let data = data.ok_or("no data chunk")?;
    if rate != RATE || channels != 1 || bits != 16 {
        return Err(format!(
            "need 48 kHz mono s16, got {rate} Hz {channels} ch {bits} bit"
        ));
    }
    if data.len() % 2 != 0 {
        return Err("odd data size".into());
    }
    let mut samples = Vec::with_capacity(data.len() / 2);
    for c in data.chunks_exact(2) {
        samples.push(i16::from_le_bytes([c[0], c[1]]));
    }
    Ok(samples)
}

/// Minimal 44-byte header WAVE writer: 48 kHz mono 16-bit PCM.
fn write_wav(path: &Path, samples: &[i16]) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes()); // block align
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    fs::write(path, v).map_err(|e| e.to_string())
}
