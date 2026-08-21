//! ALSA capture: blocking PCM reads for the scanner line-in.
//!
//! [`AlsaReader`] opens the device (default `hw:2`, the Pi's USB mic) at
//! 48 kHz mono S16_LE — the device's native rate, and Opus's native rate,
//! so no resampling is needed. If the device rejects 48 kHz, the open is
//! retried through the `plug:` ALSA plugin (kernel-space resampler) before
//! failing.
//!
//! Reads are blocking and happen on a `spawn_blocking` thread; the reader
//! reports xruns ([`PcmEvent::Xrun`]) so the caller can drop the torn frame
//! and keep going. [`FrameSplitter`] turns arbitrary frame counts (partial
//! reads, xruns, `plug:` period changes) into exact 960-sample Opus frames.
//!
//! The read path is behind the [`PcmReader`] trait so the pipeline that
//! consumes it can be tested with a fake reader (no sound card needed).

use std::collections::VecDeque;

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use tracing::warn;

/// Capture rate: 48 kHz (device-native on the Pi; Opus-native).
pub const CAPTURE_RATE: u32 = 48_000;
/// One ALSA period: 960 frames (20 ms), one Opus frame per period.
pub const PERIOD_FRAMES: u32 = 960;
/// Number of periods in the capture buffer (80 ms).
pub const PERIODS: u32 = 4;
/// Upper bound on a single `pcm.wait()` call, so the read loop can poll
/// its cancel flag between data arrivals.
const WAIT_TIMEOUT_MS: u32 = 50;
/// Errnos ALSA treats as recoverable (xrun): the stream resets and capture
/// continues.
const RECOVERABLE_ERRNOS: [i32; 4] =
    [libc::EPIPE, libc::ENODATA, libc::EBUSY, libc::ETIMEDOUT];

/// One blocking read outcome.
#[derive(Debug)]
pub enum PcmEvent {
    /// `n` frames were read into the buffer.
    Frames(usize),
    /// Xrun (underrun/overrun); the reader recovered, any partial frame is
    /// torn and must be dropped.
    Xrun,
    /// Unrecoverable device error; capture must stop.
    Fatal(String),
}

/// Errors from opening a capture device.
#[derive(Debug, thiserror::Error)]
pub enum PcmError {
    /// Both the direct name and the `plug:` fallback failed.
    #[error("failed to open ALSA device '{device}' (also tried 'plug:{device}'): {reason}")]
    Open {
        device: String,
        reason: String,
    },
    /// Failed to configure the stream on an opened device.
    #[error("ALSA configuration error on '{device}': {reason}")]
    Configure {
        device: String,
        reason: String,
    },
}

impl PcmError {
    fn configure(device: &str, e: alsa::Error) -> Self {
        Self::Configure {
            device: device.to_string(),
            reason: e.to_string(),
        }
    }
}

/// A blocking PCM capture reader. Implemented by [`AlsaReader`] (production)
/// and by test fakes (no sound card needed).
pub trait PcmReader: Send {
    /// Block until data, a xrun, or a fatal device error. `buf` holds the
    /// maximum number of frames to read; [`PcmEvent::Frames`] carries the
    /// count actually read.
    fn read(&mut self, buf: &mut [i16]) -> Result<PcmEvent, PcmError>;
    /// Close the device and release it.
    fn close(self) -> Result<(), PcmError>;
}

/// Blocking ALSA capture reader.
pub struct AlsaReader {
    /// The name actually opened (may be `plug:<device>`).
    name: String,
    pcm: PCM,
}

impl AlsaReader {
    /// Open `device` for capture at 48 kHz mono S16_LE, period 960 frames,
    /// 4 periods. Falls back to `plug:<device>` if the device does not
    /// natively support the rate.
    pub fn open(device: &str) -> Result<Self, PcmError> {
        let pcm = open_chain(device, |name| {
            let pcm = PCM::new(name, Direction::Capture, false)
                .map_err(|e| PcmError::Configure { device: name.to_string(), reason: e.to_string() })?;
            Self::configure(&pcm, name)?;
            Ok(pcm)
        })?;
        Ok(Self {
            name: device.to_string(),
            pcm,
        })
    }

    /// Apply the fixed capture format to an opened stream. `test_rate`
    /// fails the attempt (triggering the `plug:` fallback) when the device
    /// cannot do exactly 48 kHz.
    fn configure(pcm: &PCM, name: &str) -> Result<(), PcmError> {
        let hw = HwParams::any(pcm).map_err(|e| PcmError::configure(name, e))?;
        hw.set_access(Access::RWInterleaved)
            .map_err(|e| PcmError::configure(name, e))?;
        hw.set_format(Format::S16LE)
            .map_err(|e| PcmError::configure(name, e))?;
        hw.set_channels(1).map_err(|e| PcmError::configure(name, e))?;
        // No `ValueOr::Exact` in alsa 0.12: `test_rate` fails the attempt
        // (triggering the `plug:` fallback) when 48 kHz is unsupported.
        hw.test_rate(CAPTURE_RATE).map_err(|e| PcmError::configure(name, e))?;
        hw.set_rate(CAPTURE_RATE, ValueOr::Nearest)
            .map_err(|e| PcmError::configure(name, e))?;
        hw.set_periods(PERIODS, ValueOr::Nearest)
            .map_err(|e| PcmError::configure(name, e))?;
        hw.set_period_size(PERIOD_FRAMES as i64, ValueOr::Nearest)
            .map_err(|e| PcmError::configure(name, e))?;
        pcm.hw_params(&hw).map_err(|e| PcmError::configure(name, e))?;
        pcm.prepare().map_err(|e| PcmError::configure(name, e))?;
        Ok(())
    }
}

impl PcmReader for AlsaReader {
    fn read(&mut self, buf: &mut [i16]) -> Result<PcmEvent, PcmError> {
        // Wait for data with a short timeout so the caller's cancel flag is
        // polled regularly; a timeout is not an error, just "read now and
        // find out".
        match self.pcm.wait(Some(WAIT_TIMEOUT_MS)) {
            Ok(_) => {}
            Err(e) if e.errno() == libc::EAGAIN => {}
            Err(e) => return Ok(PcmEvent::Fatal(format!("{}: wait failed: {e}", self.name))),
        }
        // End the immutable `io_i16()` borrow before classifying, which
        // needs `&mut pcm` to recover.
        let frames = match self.pcm.io_i16() {
            Ok(io) => io.readi(buf),
            Err(e) => Err(e),
        };
        match frames {
            Ok(n) => Ok(PcmEvent::Frames(n)),
            Err(e) => Ok(classify_read_error(&mut self.pcm, &self.name, e)),
        }
    }

    fn close(self) -> Result<(), PcmError> {
        let Self { name, pcm } = self;
        // Best effort: the Drop impl also closes the device. (The alsa
        // crate's close method is named `drop`.)
        match pcm.drop() {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(device = %name, "ALSA close error: {e}");
                Ok(())
            }
        }
    }
}

/// Map a read error to an event: recoverable errnos (EPIPE underrun,
/// ENODATA, EBUSY, ETIMEDOUT) are xruns — `pcm.recover()` resets the
/// stream and capture continues; anything else is fatal.
fn classify_read_error(pcm: &mut PCM, name: &str, e: alsa::Error) -> PcmEvent {
    let err = e.errno();
    if !RECOVERABLE_ERRNOS.contains(&err) {
        return PcmEvent::Fatal(format!("{name}: {e}"));
    }
    match pcm.recover(err, true) {
        Ok(()) => PcmEvent::Xrun,
        Err(re) => PcmEvent::Fatal(format!("{name}: could not recover from xrun: {re}")),
    }
}

/// Try `device`, then `plug:<device>`; return the first success. A failure
/// of both yields [`PcmError::Open`] carrying both reasons plus the
/// device's supported rates (when they can be queried).
fn open_chain<T, F>(device: &str, try_open: F) -> Result<T, PcmError>
where
    F: Fn(&str) -> Result<T, PcmError>,
{
    match try_open(device) {
        Ok(dev) => Ok(dev),
        Err(primary) => {
            let fallback = format!("plug:{device}");
            match try_open(&fallback) {
                Ok(dev) => {
                    warn!(
                        device,
                        fallback, "device does not natively support the capture format; using the ALSA plug resampler"
                    );
                    Ok(dev)
                }
                Err(fallback_err) => Err(PcmError::Open {
                    device: device.to_string(),
                    reason: format!("{primary}; {fallback_err}; {rates}", rates = supported_rates(device)),
                }),
            }
        }
    }
}

/// The device's supported rate range, for error diagnostics (`""` when the
/// device cannot even be opened to query it).
fn supported_rates(device: &str) -> String {
    let pcm = match PCM::new(device, Direction::Capture, false) {
        Ok(pcm) => pcm,
        Err(_) => return String::new(),
    };
    match HwParams::any(&pcm) {
        Ok(hw) => match (hw.get_rate_min(), hw.get_rate_max()) {
            (Ok(min), Ok(max)) => format!("supported rates {min}..{max} Hz"),
            _ => "could not query rates".to_string(),
        },
        Err(_) => String::new(),
    }
}

/// Accumulates samples and yields exact `frame_len`-frame chunks regardless
/// of what frame counts the reader returns (partial reads, xruns, `plug:`
/// period sizes).
#[derive(Debug)]
pub struct FrameSplitter {
    frame_len: usize,
    pending: VecDeque<i16>,
}

impl FrameSplitter {
    pub fn new(frame_len: usize) -> Self {
        Self {
            frame_len,
            pending: VecDeque::with_capacity(frame_len),
        }
    }

    /// Append `samples`; return every complete frame as a copy. Incomplete
    /// remainders are held for the next call.
    pub fn feed(&mut self, samples: &[i16]) -> Vec<Vec<i16>> {
        self.pending.extend(samples.iter().copied());
        let mut frames = Vec::new();
        while self.pending.len() >= self.frame_len {
            let mut frame = Vec::with_capacity(self.frame_len);
            for _ in 0..self.frame_len {
                frame.push(self.pending.pop_front().unwrap());
            }
            frames.push(frame);
        }
        frames
    }

    /// Drop the incomplete frame (after a xrun): the torn samples must not
    /// be mixed with post-xrun samples.
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A counting-ramp fake reader: sample `i` of the stream is `i mod 960`
    /// (as i16), so chunk boundaries can be verified exactly. Returns the
    /// scripted sequence of outcomes.
    struct FakeReader {
        next_sample: u64,
        /// (kind, frame count) per call; kind 0 = frames, 1 = xrun, 2 = fatal.
        script: VecDeque<(u8, usize)>,
    }

    impl FakeReader {
        fn new(script: Vec<(u8, usize)>) -> Self {
            Self {
                next_sample: 0,
                script: script.into(),
            }
        }

        fn script_empty(&self) -> bool {
            self.script.is_empty()
        }

        fn ramp(&mut self, n: usize, buf: &mut [i16]) {
            for slot in buf.iter_mut().take(n) {
                *slot = (self.next_sample % 960) as i16;
                self.next_sample += 1;
            }
        }
    }

    impl PcmReader for FakeReader {
        fn read(&mut self, buf: &mut [i16]) -> Result<PcmEvent, PcmError> {
            let (kind, n) = self
                .script
                .pop_front()
                .unwrap_or_else(|| panic!("fake reader script exhausted"));
            match kind {
                0 => {
                    self.ramp(n, buf);
                    Ok(PcmEvent::Frames(n))
                }
                1 => Ok(PcmEvent::Xrun),
                _ => Ok(PcmEvent::Fatal("scripted fatal".into())),
            }
        }

        fn close(self) -> Result<(), PcmError> {
            Ok(())
        }
    }

    /// Drive the splitter exactly as the capture loop does, until the fake's
    /// script is exhausted. `Fatal` propagates as `Err`.
    fn run_splitter(mut reader: FakeReader, buf_len: usize) -> Result<Vec<Vec<i16>>, String> {
        let mut splitter = FrameSplitter::new(960);
        let mut buf = vec![0i16; buf_len];
        let mut frames = Vec::new();
        while !reader.script_empty() {
            match reader
                .read(&mut buf)
                .map_err(|e| e.to_string())?
            {
                PcmEvent::Frames(n) => frames.extend(splitter.feed(&buf[..n])),
                PcmEvent::Xrun => {
                    splitter.reset();
                }
                PcmEvent::Fatal(r) => return Err(r),
            }
        }
        Ok(frames)
    }

    /// G9: arbitrary read sizes produce exact 960-sample ramp chunks, no
    /// sample lost or duplicated at chunk boundaries.
    #[test]
    fn splitter_exact_chunks_across_odd_reads() {
        // 1 + 3 + 7 + 1000 + 1 + 500 + 459 = 1971 samples = 2 frames + 51 held.
        let reader = FakeReader::new(vec![
            (0, 1),
            (0, 3),
            (0, 7),
            (0, 1000),
            (0, 1),
            (0, 500),
            (0, 459),
        ]);
        let frames = run_splitter(reader, 2048).expect("no fatal");
        assert_eq!(frames.len(), 2, "1971 samples = 2 x 960 + 51 held");
        // Frame k, position i: ramp sample (k*960 + i) mod 960 == i.
        for (k, frame) in frames.iter().enumerate() {
            assert_eq!(frame.len(), 960);
            for (i, s) in frame.iter().enumerate() {
                assert_eq!(*s, i as i16, "frame {k} sample {i} must be i mod 960");
            }
        }
    }

    /// G9: a xrun drops only the torn chunk; subsequent samples continue the
    /// ramp unbroken.
    #[test]
    fn splitter_xrun_drops_torn_chunk_only() {
        // 500 samples held, xrun tears them, then 960 fresh samples arrive.
        let reader = FakeReader::new(vec![(0, 500), (1, 0), (0, 960)]);
        let frames = run_splitter(reader, 2048).expect("no fatal");
        assert_eq!(frames.len(), 1, "only the post-xrun frame is complete");
        // The 500 pre-xrun samples were dropped by the reset; the device
        // ramp itself is uninterrupted, so the frame starts at sample 500.
        let frame = &frames[0];
        for (i, s) in frame.iter().enumerate() {
            assert_eq!(*s, ((500 + i) % 960) as i16, "sample {i} after xrun");
        }
    }

    /// G9: a fatal event propagates to the caller.
    #[test]
    fn splitter_fatal_propagates() {
        let reader = FakeReader::new(vec![(0, 100), (2, 0)]);
        let err = run_splitter(reader, 1024).expect_err("fatal must propagate");
        assert_eq!(err, "scripted fatal");
    }

    /// G10: the fallback chain tries the plain device, then `plug:`, and
    /// reports both failures with the device name.
    #[test]
    fn open_chain_falls_back_to_plug_and_reports_both() {
        let tried: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let result: Result<(), PcmError> = open_chain("hw:2", |name| {
            tried.lock().unwrap().push(name.to_string());
            Err(PcmError::Configure {
                device: name.to_string(),
                reason: format!("no 48 kHz on {name}"),
            })
        });
        let err = result.expect_err("both attempts fail");
        assert_eq!(
            *tried.lock().unwrap(),
            vec!["hw:2".to_string(), "plug:hw:2".to_string()],
            "must try the device, then the plug fallback"
        );
        let PcmError::Open { device, reason } = err else {
            panic!("expected Open error, got {err:?}")
        };
        assert_eq!(device, "hw:2");
        assert!(reason.contains("hw:2"), "reason names the primary failure: {reason}");
        assert!(
            reason.contains("plug:hw:2"),
            "reason names the fallback failure: {reason}"
        );
    }

    /// G10: a direct open success never touches the fallback.
    #[test]
    fn open_chain_success_no_fallback() {
        let tried: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let value: u32 = open_chain("hw:2", |name| {
            tried.lock().unwrap().push(name.to_string());
            Ok(42)
        })
        .expect("first attempt succeeds");
        assert_eq!(value, 42);
        assert_eq!(*tried.lock().unwrap(), vec!["hw:2".to_string()]);
    }

    /// G10: a failing direct open recovers through the `plug:` fallback.
    #[test]
    fn open_chain_fallback_success() {
        let tried: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let value: u32 = open_chain("hw:2", |name| {
            tried.lock().unwrap().push(name.to_string());
            if name == "hw:2" {
                Err(PcmError::Configure {
                    device: name.to_string(),
                    reason: "rate 48000 not available".into(),
                })
            } else {
                Ok(7)
            }
        })
        .expect("plug fallback succeeds");
        assert_eq!(value, 7);
        assert_eq!(
            *tried.lock().unwrap(),
            vec!["hw:2".to_string(), "plug:hw:2".to_string()]
        );
    }
}
