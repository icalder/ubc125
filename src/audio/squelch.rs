//! Squelch gate: a coarse, level-driven on/off filter that mutes the
//! scanner's noise floor — and the squelch open/close transitions that
//! ride on it — while passing active speech untouched.
//!
//! The gate is the complement of the abandoned per-transient de-clicker
//! (see the `declick` branch): instead of classifying every transient
//! as click-vs-speech, it maintains squelch state from the audio level,
//! the same coarse decision the scanner's own squelch makes.
//!
//! Both transitions are **sample-accurate and anchored at the crossing**,
//! and are exact mirrors of each other:
//!
//! - **closed → open** (fade-in): scan the frame for the first *sample*
//!   at or above `reopen_db`; mute everything before it and ramp 0→1 over
//!   `fade_ms` starting at that sample. The crossing sits anywhere in the
//!   20 ms frame, so the fade is anchored there, not at the frame start.
//!   This swallows the squelch-release pop at the onset.
//!
//! - **open → closed** (fade-out, the mirror): scan the frame for the
//!   first *sustained* drop below `close_db` — a run of `close_confirm_ms`
//!   consecutive samples below the close level. The confirmation sample is
//!   the **end** of the 1→0 fade: the fade starts `fade_out_ms` earlier
//!   (or `fade_ms` when it is zero) and is applied retroactively to the
//!   buffered tail. The run-confirm keeps
//!   single-sample dips in live speech from tripping it; if speech recovers
//!   above `reopen_db` mid-fade the fade is cancelled and the gain ramps
//!   back to 1.
//!
//! Measured on the 60 s raw capture (`test-audio/raw60.wav`):
//! - Closed-state floor peaks: −56…−58.5 dBFS; the floor never re-opens
//!   the gate at `reopen_db` (−42), which sits 3 dB above the highest
//!   floor spike (−45.0).
//! - Run-length analysis of the whole capture: below −45 dBFS there are
//!   only ~10 runs of ≥20 ms, which are the real inter-transmission gaps
//!   (the capture has 10 transmissions). Speech essentially never dips
//!   below −45 for 20 ms, so `close_db` (−45) + a 20 ms confirm triggers
//!   on genuine transmission ends without flapping on syllable dips
//!   (at −42 there are 45 such runs — constant flapping).
//! - Onsets are full-scale release pops; the close is a smooth decay of the
//!   noise floor, so anchoring the fade at the start of the decay (rather
//!   than at the closed floor) removes the audible "off" tail.

use std::f64::consts::PI;

use crate::audio::filter::PcmFrameFilter;

/// Capture sample rate (ALSA capture is 48 kHz mono s16).
const RATE: f64 = 48_000.0;
const SAMPLES_PER_MS: f64 = RATE / 1000.0;
const FRAME_SAMPLES: usize = 960;

/// Peak of a sample magnitude in dBFS; `peak` 0 (digital silence) maps
/// to −160 dB, far below any threshold.
fn peak_dbfs(peak: u32) -> f64 {
    if peak < 1 {
        return -160.0;
    }
    20.0 * (peak as f64 / 32_768.0).log10()
}

/// Gate parameters. See the module docs for the measurement rationale
/// behind the defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SquelchGateConfig {
    /// Sample level (dBFS) below which a sustained run (see
    /// `close_confirm_ms`) is "no speech" and starts the close fade. The
    /// capture's speech never dips below this for 20 ms, so it separates
    /// transmission ends from syllable dips.
    pub close_db: f64,
    /// Sample level (dBFS) at or above which the gate reopens from closed
    /// (or cancels an in-flight close fade). Must sit above the highest
    /// floor spike so the floor can never re-open the gate.
    pub reopen_db: f64,
    /// Duration (ms) below `close_db` required before the close fade is
    /// confirmed. The confirmation sample is the fade's endpoint; the
    /// buffered look-back makes the preceding `fade_out_ms` editable. Long
    /// enough to ignore syllable dips.
    pub close_confirm_ms: u32,
    /// Raised-cosine length (ms) of the fade-in on reopen and of the
    /// ramp-back after a cancelled close.
    pub fade_ms: u32,
    /// Raised-cosine length (ms) of the close fade. Zero inherits
    /// `fade_ms`; other values below `fade_ms` are raised to `fade_ms`, so
    /// the fade-out is never shorter than the fade-in. The default is 20 ms
    /// through inheritance.
    pub fade_out_ms: u32,
    /// Minimum number of complete frames held back while the gate makes its
    /// level decision. The gate increases this automatically when
    /// `fade_out_ms` needs more look-back. Two 20 ms frames cover the default
    /// confirmation interval and close fade. Zero explicitly disables the
    /// look-back and is useful as a zero-latency control.
    pub delay_frames: u32,
}

impl Default for SquelchGateConfig {
    fn default() -> Self {
        Self {
            close_db: -45.0,
            reopen_db: -42.0,
            close_confirm_ms: 20,
            fade_ms: 20,
            fade_out_ms: 0,
            delay_frames: 2,
        }
    }
}

/// Gate state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    /// Audio passes at full gain (or a fade-in / ramp-back is winding up).
    Open,
    /// Close fade in progress; gain ramping 1→0 toward the confirmed floor.
    Closing,
    /// Muted; waiting for a sample at or above `reopen_db`.
    Closed,
}

/// A raised-cosine gain ramp from `from` to `to` over `total` samples.
struct Ramp {
    from: f64,
    to: f64,
    total: u32,
    done: u32,
}

impl Ramp {
    fn new(from: f64, to: f64, total: u32) -> Self {
        Self {
            from,
            to,
            total,
            done: 0,
        }
    }

    /// Gain at the `i`-th sample of this call (offset by `done` from the
    /// ramp start), raised-cosine from `from` to `to`.
    fn gain(&self, i: u32) -> f64 {
        if self.total == 0 {
            return self.to;
        }
        let x = ((self.done + i) as f64 / self.total as f64).min(1.0);
        self.from + (self.to - self.from) * 0.5 * (1.0 - (PI * x).cos())
    }
}

/// A frame waiting in the gate's look-ahead buffer.
///
/// Samples remain unmodified until they leave the buffer. Gains are stored
/// separately so a close detected in a later frame can fade the already
/// buffered tail without losing the original samples if a close is
/// cancelled by speech returning.
struct BufferedFrame {
    samples: Vec<i16>,
    gains: Vec<f64>,
}

impl BufferedFrame {
    fn new(samples: Vec<i16>) -> Self {
        let gains = vec![1.0; samples.len()];
        Self { samples, gains }
    }

    fn render_into(self, output: &mut [i16]) {
        debug_assert_eq!(self.samples.len(), output.len());
        for ((out, sample), gain) in output.iter_mut().zip(self.samples).zip(self.gains) {
            *out = (sample as f64 * gain).round().clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}

/// One squelch gate per capture generation (see `PcmFrameFilter::for_capture`).
pub struct SquelchGate {
    cfg: SquelchGateConfig,
    fade_total: u32,
    fade_out_total: u32,
    close_confirm: u32,
    buffer_frames: usize,
    /// Linear close threshold (|sample|), from `cfg.close_db`.
    close_level: u32,
    /// Linear reopen threshold (|sample|), from `cfg.reopen_db`.
    reopen_level: u32,
    state: GateState,
    ramp: Option<Ramp>,
    /// Consecutive samples below `close_level` (open-state close detector).
    below_count: u32,
    /// Frames held back so the confirmation look-behind is still editable.
    buffer: std::collections::VecDeque<BufferedFrame>,
}

impl SquelchGate {
    pub fn new(cfg: SquelchGateConfig) -> Self {
        let close_level = (32_768.0 * 10f64.powf(cfg.close_db / 20.0)).ceil() as u32;
        let reopen_level = (32_768.0 * 10f64.powf(cfg.reopen_db / 20.0)).ceil() as u32;
        let fade_out_ms = if cfg.fade_out_ms == 0 {
            cfg.fade_ms
        } else {
            cfg.fade_out_ms.max(cfg.fade_ms)
        };
        let fade_total = (cfg.fade_ms as f64 * SAMPLES_PER_MS) as u32;
        let fade_out_total = (fade_out_ms as f64 * SAMPLES_PER_MS) as u32;
        // The look-back must cover both the close confirmation and the
        // requested fade. Do not allow a longer A/B fade to silently lose
        // its beginning at the front of the ring buffer.
        let close_confirm = (cfg.close_confirm_ms as f64 * SAMPLES_PER_MS)
            .round()
            .max(1.0) as u32;
        let required_frames = (fade_out_total as usize)
            .div_ceil(FRAME_SAMPLES)
            .max((close_confirm as usize).div_ceil(FRAME_SAMPLES));
        Self {
            fade_total,
            fade_out_total,
            // A zero confirmation interval still means one sample. Apart
            // from being safer, this avoids subtracting one from zero when
            // calculating the crossing anchor.
            close_confirm,
            close_level: close_level.min(32_768),
            reopen_level: reopen_level.min(32_768),
            state: GateState::Closed,
            ramp: None,
            below_count: 0,
            buffer: std::collections::VecDeque::new(),
            buffer_frames: if cfg.delay_frames == 0 {
                0
            } else {
                (cfg.delay_frames as usize).max(required_frames)
            },
            cfg,
        }
    }

    /// True while audio is passing (open or fading out).
    pub fn is_open(&self) -> bool {
        self.state != GateState::Closed
    }

    /// Actual look-ahead latency in complete frames. This can exceed the
    /// configured minimum when a longer close fade requires it.
    pub fn latency_frames(&self) -> usize {
        self.buffer_frames
    }

    /// Peak of this frame in dBFS (for logging/harnesses).
    pub fn frame_dbfs(frame: &[i16]) -> f64 {
        // `i16::MIN` needs the i32 round-trip: `wrapping_abs` would keep
        // it negative and the `as u32` cast would sign-extend.
        let peak = frame
            .iter()
            .fold(0u32, |m, &s| m.max((s as i32).unsigned_abs() as u32));
        peak_dbfs(peak)
    }

    /// Gain of the last sample applied by the active ramp (or the
    /// state's constant gain when no ramp is active).
    fn current_gain(&self) -> f64 {
        match &self.ramp {
            Some(r) if r.total > 0 => {
                let i = r.done.saturating_sub(1) as f64;
                let x = (i / r.total as f64).min(1.0);
                r.from + (r.to - r.from) * 0.5 * (1.0 - (PI * x).cos())
            }
            Some(r) => r.to,
            _ => match self.state {
                GateState::Closed => 0.0,
                _ => 1.0,
            },
        }
    }

    /// Apply a ramp to one buffered frame, replacing its gain schedule.
    fn apply_ramp_to_frame(ramp: &mut Ramp, frame: &mut BufferedFrame, start: usize) {
        if start >= frame.gains.len() {
            return;
        }
        for gain in frame.gains.iter_mut().skip(start) {
            if ramp.done < ramp.total {
                // `done` is advanced after each sample, so the offset is
                // supplied by the ramp itself. Supplying both `done` and a
                // loop index would advance the curve twice.
                *gain = ramp.gain(0);
                ramp.done = ramp.done.saturating_add(1);
            } else {
                *gain = ramp.to;
            }
        }
    }

    /// Apply a ramp to the buffered stream starting at `start`, measured
    /// from the oldest buffered sample. Samples before `start` retain their
    /// existing schedule. Returns true when the ramp completed.
    fn apply_ramp_to_buffer(
        buffer: &mut std::collections::VecDeque<BufferedFrame>,
        ramp: &mut Ramp,
        start: usize,
    ) -> bool {
        let mut position = 0usize;
        for frame in buffer {
            if position + frame.gains.len() <= start {
                position += frame.gains.len();
                continue;
            }
            let frame_start = start.saturating_sub(position);
            Self::apply_ramp_to_frame(ramp, frame, frame_start);
            position += frame.gains.len();
        }
        ramp.done >= ramp.total
    }

    /// Apply the active ramp to the newest buffered frame.
    fn apply_ramp_to_latest(&mut self) {
        let Some(ramp) = &mut self.ramp else {
            return;
        };
        if let Some(frame) = self.buffer.back_mut() {
            Self::apply_ramp_to_frame(ramp, frame, 0);
        }
    }

    /// Start the close fade far enough before the confirmation sample that
    /// its final sample lands exactly at the confirmed return to floor. The
    /// newest frame is already buffered, so the fade can reach backwards
    /// into the audible decay tail instead of only fading a later frame.
    fn close_at(&mut self, end: usize) {
        self.state = GateState::Closing;
        let mut ramp = Ramp::new(1.0, 0.0, self.fade_out_total);
        let newest_start = self
            .buffer
            .iter()
            .take(self.buffer.len().saturating_sub(1))
            .map(|frame| frame.gains.len())
            .sum::<usize>();
        let end = newest_start.saturating_add(end);
        // For a non-zero fade, the first of N samples is N-1 samples before
        // the endpoint. A zero-length fade cuts at the endpoint sample.
        let fade_samples = (self.fade_out_total as usize).max(1);
        let anchor = end.saturating_add(1).saturating_sub(fade_samples);
        let complete = Self::apply_ramp_to_buffer(&mut self.buffer, &mut ramp, anchor);
        self.ramp = if complete { None } else { Some(ramp) };
        if complete {
            self.state = GateState::Closed;
        }
    }

    /// Return all buffered frames in order. This is used at finite-capture
    /// shutdown to preserve the fixed delay's final samples.
    fn drain_buffer(&mut self) -> Vec<Vec<i16>> {
        let mut output = Vec::with_capacity(self.buffer.len());
        while let Some(frame) = self.buffer.pop_front() {
            let mut samples = vec![0i16; frame.samples.len()];
            frame.render_into(&mut samples);
            output.push(samples);
        }
        output
    }

    /// Mute `frame[..cross]` and open the gate at the crossing sample
    /// with a fresh fade-in ramp.
    fn open_at(&mut self, cross: usize) {
        let Some(frame) = self.buffer.back_mut() else {
            return;
        };
        frame.gains[..cross].fill(0.0);
        self.state = GateState::Open;
        self.below_count = 0;
        self.ramp = Some(Ramp::new(0.0, 1.0, self.fade_total));
        self.apply_ramp_to_latest_from(cross);
    }
}

impl PcmFrameFilter for SquelchGate {
    fn process_frame(&mut self, frame: &mut [i16]) {
        // The input is copied into a short look-ahead buffer. This is the
        // intentional latency of the gate; PassThrough remains zero-copy.
        self.buffer.push_back(BufferedFrame::new(frame.to_vec()));

        match self.state {
            GateState::Closed => {
                // Sample-accurate reopen: the first sample at or above the
                // reopen level starts the fade-in. The current frame is
                // already buffered, so its pre-crossing samples are muted
                // before it is eventually emitted.
                let cross = self.buffer.back().and_then(|buffered| {
                    buffered
                        .samples
                        .iter()
                        .position(|&s| (s as i32).unsigned_abs() as u32 >= self.reopen_level)
                });
                let Some(cross) = cross else {
                    self.buffer.back_mut().unwrap().gains.fill(0.0);
                    self.emit_delayed(frame);
                    return;
                };
                self.open_at(cross);
            }
            GateState::Open => {
                // Drop a finished ramp (e.g. a completed ramp-back) so a
                // settled gate re-arms its close detector.
                if self.ramp.as_ref().is_some_and(|r| r.done >= r.total) {
                    self.ramp = None;
                }
                if self.ramp.is_some() {
                    // A fade-in or ramp-back is still in progress: apply
                    // it and skip close detection (speech is recovering).
                    self.apply_ramp_to_latest();
                } else {
                    // The first run of close_confirm consecutive samples
                    // below close_level confirms the floor. Its final
                    // sample is the endpoint of the fade. `below_count`
                    // persists across frames.
                    let mut cross = None;
                    if let Some(buffered) = self.buffer.back() {
                        for (i, &s) in buffered.samples.iter().enumerate() {
                            let mag = (s as i32).unsigned_abs() as u32;
                            if mag < self.close_level {
                                self.below_count = self.below_count.saturating_add(1);
                                if self.below_count >= self.close_confirm {
                                    // The confirmation sample is the
                                    // endpoint of the fade. It may be in a
                                    // later frame than the run's start;
                                    // buffered frames make the look-back
                                    // editable.
                                    cross = Some(i);
                                    break;
                                }
                            } else {
                                self.below_count = 0;
                            }
                        }
                    }
                    if let Some(start) = cross {
                        self.close_at(start);
                    }
                }
            }
            GateState::Closing => {
                let recovered = self.buffer.back().is_some_and(|buffered| {
                    Self::frame_dbfs(&buffered.samples) >= self.cfg.reopen_db
                });
                if recovered {
                    // Speech returned before the fade finished. Keep the
                    // pending close fade on the buffered tail, then ramp
                    // the returning frame back to full gain. No already
                    // emitted samples can be changed, but the delay ensures
                    // the pending tail remains coherent.
                    let from = self.current_gain();
                    self.state = GateState::Open;
                    self.below_count = 0;
                    self.ramp = Some(Ramp::new(from, 1.0, self.fade_total));
                    self.apply_ramp_to_latest();
                } else {
                    self.apply_ramp_to_latest();
                    if self.ramp.as_ref().is_some_and(|r| r.done >= r.total) {
                        if let Some(frame) = self.buffer.back_mut() {
                            frame.gains.fill(0.0);
                        }
                        self.state = GateState::Closed;
                        self.ramp = None;
                    }
                }
            }
        }
        self.emit_delayed(frame);
    }

    fn flush(&mut self) -> Vec<Vec<i16>> {
        self.drain_buffer()
    }

    fn for_capture(&self) -> Box<dyn PcmFrameFilter> {
        Box::new(Self::new(self.cfg))
    }
}

impl SquelchGate {
    /// Apply the active ramp to the newest buffered frame from `start`.
    fn apply_ramp_to_latest_from(&mut self, start: usize) {
        let Some(ramp) = &mut self.ramp else {
            return;
        };
        if let Some(frame) = self.buffer.back_mut() {
            Self::apply_ramp_to_frame(ramp, frame, start);
        }
    }

    /// Emit the oldest held frame, or silence while the fixed delay fills.
    fn emit_delayed(&mut self, output: &mut [i16]) {
        if self.buffer.len() > self.buffer_frames {
            let buffered = self.buffer.pop_front().unwrap();
            buffered.render_into(output);
        } else {
            output.fill(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 960; // 20 ms @ 48 kHz

    fn frame_at(dbfs: f64) -> Vec<i16> {
        let amp = (32_767.0 * 10f64.powf(dbfs / 20.0)) as i16;
        vec![amp; FRAME]
    }

    fn mags(f: &[i16]) -> Vec<u32> {
        f.iter()
            .map(|&s| (s as i32).unsigned_abs() as u32)
            .collect()
    }

    fn run(mut cfg: SquelchGateConfig, frames: &[Vec<i16>]) -> (Vec<Vec<i16>>, Vec<bool>) {
        // These tests exercise the state machine and crossing math. The
        // delay-specific behavior is covered by the tests below.
        cfg.delay_frames = 0;
        let mut gate = SquelchGate::new(cfg);
        let mut out = Vec::with_capacity(frames.len());
        let mut open = Vec::with_capacity(frames.len());
        for f in frames {
            let mut c = f.clone();
            gate.process_frame(&mut c);
            open.push(gate.is_open());
            out.push(c);
        }
        (out, open)
    }

    #[test]
    fn closed_mutes_floor() {
        let frames: Vec<Vec<i16>> = (0..10).map(|_| frame_at(-60.0)).collect();
        let (out, open) = run(SquelchGateConfig::default(), &frames);
        assert!(open.iter().all(|&o| !o));
        assert!(out.iter().all(|f| f.iter().all(|&s| s == 0)));
    }

    #[test]
    fn opens_at_crossing_and_fades_in() {
        let mut frames: Vec<Vec<i16>> = (0..5).map(|_| frame_at(-160.0)).collect();
        frames.extend((0..5).map(|_| frame_at(-20.0)));
        let (out, open) = run(SquelchGateConfig::default(), &frames);
        assert!(open[..5].iter().all(|&o| !o));
        assert!(open[5..].iter().all(|&o| o));
        // Muted before open.
        assert!(out[..5].iter().all(|f| f.iter().all(|&s| s == 0)));
        // Crossing at sample 0 of the open frame: the 20 ms fade spans
        // exactly that frame, starting at zero (continuous with the
        // mute); full gain from the next frame.
        assert_eq!(out[5][0], 0);
        let ramp = mags(&out[5]);
        assert!(
            ramp.windows(2).all(|w| w[0] <= w[1]),
            "fade must ramp monotonically"
        );
        assert_eq!(out[6], frames[6]);
        assert_eq!(out[9], frames[9]);
    }

    #[test]
    fn fade_starts_at_crossing_not_frame_start() {
        // 500 muted-then-loud samples in one frame: the crossing sits at
        // sample 500, so the pre-crossing part is muted and the fade is
        // anchored there, not at the frame start.
        let mut frame = vec![0i16; FRAME];
        let loud = frame_at(-20.0)[0];
        frame[500..].fill(loud);
        let mut frames: Vec<Vec<i16>> = (0..5).map(|_| frame_at(-160.0)).collect();
        frames.push(frame.clone());
        frames.extend((0..3).map(|_| frame_at(-20.0)));
        let (out, _) = run(SquelchGateConfig::default(), &frames);
        assert!(out[5][..500].iter().all(|&s| s == 0), "pre-crossing muted");
        assert_eq!(out[5][500], 0, "fade starts at the crossing");
        let ramp = mags(&out[5][500..]);
        assert!(ramp.windows(2).all(|w| w[0] <= w[1]));
        // Fade continues across the frame boundary: 460 samples ramped in
        // frame 5, so the first full-gain sample is frame 6 index 960-460.
        assert_eq!(out[6][500], frames[6][500]);
    }

    #[test]
    fn close_fade_ends_at_confirmation_sample() {
        // The floor is confirmed after 2 ms below close_db. With a longer
        // 4 ms fade, the fade must begin before the run and end at the
        // confirmation sample, rather than ending 4 ms later.
        let cfg = SquelchGateConfig {
            close_confirm_ms: 2,
            fade_ms: 4,
            fade_out_ms: 4,
            ..SquelchGateConfig::default()
        };
        let tail = frame_at(-48.0)[0];
        let mut frame = frame_at(-20.0);
        frame[500..].fill(tail);
        let mut frames: Vec<Vec<i16>> = (0..5).map(|_| frame_at(-20.0)).collect();
        frames.push(frame.clone());
        frames.extend((0..5).map(|_| frame_at(-48.0)));
        let (out, open) = run(cfg, &frames);

        // The 4 ms fade begins 96 samples before the threshold run.
        assert_eq!(out[5][..404], frame[..404]);
        assert!(out[5][500].unsigned_abs() < frame[500].unsigned_abs());
        let endpoint = 500 + 2 * 48 - 1;
        assert!(out[5][endpoint].unsigned_abs() <= 1);
        assert!(out[5][endpoint + 1..].iter().all(|&s| s == 0));
        assert!(!open[5], "the gate closes at the confirmation sample");
    }

    #[test]
    fn speech_pause_fades_out_and_reopens() {
        // A 140 ms pause (below `close_db`) closes the gate with a
        // sample-accurate fade; the resumption reopens with a fade-in.
        let mut frames: Vec<Vec<i16>> = (0..10).map(|_| frame_at(-20.0)).collect();
        frames.extend((0..7).map(|_| frame_at(-54.0))); // 140 ms pause
        frames.extend((0..10).map(|_| frame_at(-20.0)));
        let (out, open) = run(SquelchGateConfig::default(), &frames);
        assert!(open[..10].iter().all(|&o| o));
        // Frame 10: first pause frame; the close run starts at sample 0
        // (whole frame below the close level) so the fade spans it and the
        // gate ends the frame closed (20 ms fade == 20 ms frame).
        assert!(!open[10]);
        let ramp = mags(&out[10]);
        assert!(
            ramp.windows(2).all(|w| w[0] >= w[1]),
            "close fade monotonic"
        );
        assert!(ramp[959] < 10, "faded to (near) silence by frame end");
        // Rest of the pause is muted.
        assert!(!open[11] && !open[16]);
        assert!(out[11..=16].iter().all(|f| f.iter().all(|&s| s == 0)));
        // Resumption reopens with a fresh fade-in.
        assert!(open[17]);
        assert_eq!(out[17][0], 0);
        assert_eq!(out[18], frames[18]);
    }

    #[test]
    fn quiet_speech_above_close_db_never_closes() {
        // Quiet speech at −37 (the quietest measured) sits above
        // `close_db` (−45): the gate must stay open and pass it untouched.
        let mut frames: Vec<Vec<i16>> = (0..10).map(|_| frame_at(-20.0)).collect();
        frames.extend((0..60).map(|_| frame_at(-37.0))); // 1.2 s
        frames.push(frame_at(-20.0));
        let (out, open) = run(SquelchGateConfig::default(), &frames);
        assert!(open.iter().all(|&o| o));
        assert_eq!(out[10], frames[10]);
        assert_eq!(out[68], frames[68]);
        assert_eq!(out[69], frames[69]);
    }

    #[test]
    fn close_fades_out_to_silence() {
        let mut frames: Vec<Vec<i16>> = (0..10).map(|_| frame_at(-20.0)).collect();
        frames.extend((0..10).map(|_| frame_at(-60.0)));
        let (out, open) = run(SquelchGateConfig::default(), &frames);
        // Frame 10: first quiet frame, close run starts at sample 0, the
        // 20 ms fade spans it and the gate ends closed.
        assert!(!open[10]);
        let ramp = mags(&out[10]);
        assert!(ramp[0] > ramp[959]);
        assert!(
            ramp.windows(2).all(|w| w[0] >= w[1]),
            "close fade monotonic"
        );
        assert!(ramp[959] < 10, "faded to (near) silence by frame end");
        assert!(!open[11]);
        assert!(out[11..].iter().all(|f| f.iter().all(|&s| s == 0)));
    }

    #[test]
    fn close_fade_cancelled_on_recovery() {
        // A 40 ms close fade spans two frames, so speech returning in the
        // second frame (a 20 ms dip) must cancel the fade; the ramp back
        // to full gain is 40 ms as well and spans the next two frames.
        let cfg = SquelchGateConfig {
            fade_ms: 40,
            ..SquelchGateConfig::default()
        };
        let mut frames: Vec<Vec<i16>> = (0..10).map(|_| frame_at(-20.0)).collect();
        frames.push(frame_at(-60.0)); // first quiet frame, close fade half done
        frames.push(frame_at(-20.0)); // speech returns: cancel + ramp back
        frames.push(frame_at(-20.0)); // ramp back continues
        frames.push(frame_at(-20.0)); // full gain
        let (out, open) = run(cfg, &frames);
        assert!(open.iter().all(|&o| o), "never fully closed");
        // Ramp-back: monotonic over both frames, starting at ~half gain
        // (mid close-fade), reaching full at the end of frame 12.
        let full = frames[11][0] as u32;
        for fi in [11, 12] {
            let ramp = mags(&out[fi]);
            assert!(ramp.windows(2).all(|w| w[0] <= w[1]), "ramp-back monotonic");
        }
        assert!(
            mags(&out[11])[0] < full * 55 / 100,
            "ramp-back starts at ~half gain"
        );
        assert!(
            mags(&out[11])[959] < full,
            "still ramping at end of frame 11"
        );
        assert!(
            full.saturating_sub(mags(&out[12])[959]) <= 1,
            "ramp-back reaches full gain by end of frame 12"
        );
        assert_eq!(out[13], frames[13]);
    }

    #[test]
    fn single_sample_dips_do_not_close() {
        // Live speech with isolated below-threshold samples (a 2 ms fade
        // confirm) must not trip the close: a single quiet sample between
        // loud ones resets the run, so the gate stays open and passes the
        // frame untouched.
        let mut frame = frame_at(-20.0);
        frame[100] = 0; // one silent sample
        frame[400] = 0;
        frame[700] = 0;
        let mut frames: Vec<Vec<i16>> = (0..5).map(|_| frame_at(-20.0)).collect();
        frames.push(frame.clone());
        frames.extend((0..5).map(|_| frame_at(-20.0)));
        let (out, open) = run(SquelchGateConfig::default(), &frames);
        assert!(open.iter().all(|&o| o), "isolated dips must not close");
        assert_eq!(out[5], frame, "frame passed through with the dips intact");
    }

    #[test]
    fn zero_fade_opens_hard() {
        let cfg = SquelchGateConfig {
            fade_ms: 0,
            ..SquelchGateConfig::default()
        };
        let mut frames: Vec<Vec<i16>> = (0..3).map(|_| frame_at(-160.0)).collect();
        frames.extend((0..3).map(|_| frame_at(-20.0)));
        let (out, _) = run(cfg, &frames);
        assert!(out[..3].iter().all(|f| f.iter().all(|&s| s == 0)));
        assert_eq!(out[3], frames[3], "no fade: full gain immediately");
    }

    #[test]
    fn zero_fade_cuts_hard() {
        let cfg = SquelchGateConfig {
            fade_ms: 0,
            ..SquelchGateConfig::default()
        };
        let mut frames: Vec<Vec<i16>> = (0..5).map(|_| frame_at(-20.0)).collect();
        frames.extend((0..5).map(|_| frame_at(-60.0)));
        let (out, open) = run(cfg, &frames);
        assert!(open[..5].iter().all(|&o| o));
        // With no fade, only the confirmation sample and following audio
        // are cut. Samples before the confirmed floor still pass.
        assert!(!open[5]);
        assert_eq!(out[5][..959], frames[5][..959]);
        assert_eq!(out[5][959], 0);
        assert!(out[6..].iter().all(|f| f.iter().all(|&s| s == 0)));
    }

    #[test]
    fn delayed_close_reaches_back_into_previous_frame() {
        // The quiet run starts halfway through frame 5 and is confirmed in
        // frame 6. With a two-frame look-ahead, frame 5 is still editable
        // when the close is detected, so the fade is present in the emitted
        // frame rather than being lost at the frame boundary.
        let cfg = SquelchGateConfig {
            delay_frames: 2,
            ..SquelchGateConfig::default()
        };
        let mut frames: Vec<Vec<i16>> = (0..5).map(|_| frame_at(-20.0)).collect();
        let mut first_quiet = frame_at(-20.0);
        // Keep the confirmed tail close to the threshold. It is still
        // audible enough to make the attenuation measurable.
        first_quiet[FRAME / 2..].fill(frame_at(-46.0)[0]);
        frames.push(first_quiet);
        frames.extend((0..3).map(|_| frame_at(-46.0)));

        let mut gate = SquelchGate::new(cfg);
        let mut out = Vec::new();
        for input in &frames {
            let mut frame = input.clone();
            gate.process_frame(&mut frame);
            out.push(frame);
        }
        out.extend(gate.flush());

        assert_eq!(out.len(), frames.len() + 2);
        // Two frames of latency preserve the original frame positions.
        assert_eq!(out[6], frames[4]);
        let faded = &out[7]; // input frame 5, after the two-frame delay
        assert_eq!(&faded[..FRAME / 2], &frames[5][..FRAME / 2]);
        assert!(
            faded[FRAME / 2].unsigned_abs() > faded[FRAME - 1].unsigned_abs(),
            "the buffered audible tail must fade down"
        );
        assert!(faded[FRAME - 1].unsigned_abs() < frames[5][FRAME - 1].unsigned_abs());
    }

    #[test]
    fn delay_flush_preserves_buffered_tail() {
        let cfg = SquelchGateConfig {
            delay_frames: 2,
            ..SquelchGateConfig::default()
        };
        let inputs: Vec<Vec<i16>> = (0..7).map(|_| frame_at(-20.0)).collect();
        let mut gate = SquelchGate::new(cfg);
        let mut outputs = Vec::new();
        for input in &inputs {
            let mut frame = input.clone();
            gate.process_frame(&mut frame);
            outputs.push(frame);
        }
        outputs.extend(gate.flush());
        // Every input produces one delayed output, and flushing emits the
        // two frames that were still in flight.
        assert_eq!(outputs.len(), inputs.len() + 2);
        assert!(outputs[..2]
            .iter()
            .all(|frame| frame.iter().all(|&s| s == 0)));
    }

    #[test]
    fn for_capture_is_fresh_and_closed() {
        let gate = SquelchGate::new(SquelchGateConfig {
            delay_frames: 0,
            ..SquelchGateConfig::default()
        });
        assert!(!gate.is_open());
        let mut child = gate.for_capture();
        // Fresh gate starts closed: a loud frame opens with a fade from
        // zero; with a 20 ms fade the last sample of the 20 ms frame is
        // within 1 LSB of full.
        let mut f = frame_at(-20.0);
        child.process_frame(&mut f);
        assert_eq!(f[0], 0, "fresh gate must start the fade from mute");
        let last = (f[959] as i32).unsigned_abs();
        let full = frame_at(-20.0)[0] as u32;
        assert!(
            full.saturating_sub(last) <= 1,
            "ramped to full by frame end"
        );
    }
}
