//! Correction targets: what a policy writes into a detected window.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/fill.py`. All policies share the same window
//! and crossfade machinery (see [`super::filter`]), so a listening comparison is
//! fair and adding a policy is a new type plus one entry in [`make_fill`]
//! rather than an edit to the filter.
//!
//! Two attributes tell the filter how a fill must be joined to the audio around
//! it:
//!
//!   * `pad` — context samples the fill reads from before its window.
//!     [`Config::context_pad`](super::config::Config::context_pad) takes the
//!     maximum over the classes a run corrects, and that figure sizes both the
//!     delay floor and the ring.
//!   * `ends_at_zero` — whether the last replacement sample is ~zero. When a
//!     recovery tail is active, a fill that ends at zero drops its right-edge
//!     crossfade: fading it back to the original there un-does the correction
//!     over the last `xfade` samples and leaves a full-scale step into the
//!     ramp's near-zero start.

use crate::audio::clickfilter::config::Config;
use crate::audio::clickfilter::constants::{FS, Policy, RATE};

/// Median samples each side of the window that an anchor is taken over.
pub const ANCHOR: usize = 4;
/// Taps in the `lf-null` low-pass.
pub const LF_TAPS: usize = 64;

/// Whether a correction window's right edge fades back to the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightEdge {
    /// Fade out over `xfade` samples (the reference's `right=True`).
    Fade,
    /// Hold at 1 up to the last sample (`right=False`): used when a fill reaches
    /// zero at `window_end` and a recovery ramp takes over there.
    Hold,
}

/// What a fill promises the filter, kept beside the fill that promises it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillMeta {
    pub pad: i64,
    pub ends_at_zero: bool,
}

/// The pad and zero-handover rule of each policy. One place, so `Config` can
/// size the delay floor without building a fill.
pub fn fill_meta(policy: Policy) -> FillMeta {
    let meta = match policy {
        Policy::Interp => FillMeta {
            pad: 8,
            ends_at_zero: false,
        },
        Policy::Descend => FillMeta {
            pad: 8,
            ends_at_zero: true,
        },
        Policy::Mute => FillMeta {
            pad: 8,
            ends_at_zero: true,
        },
        // lf-null leaves the excursion, so it cannot end at zero.
        Policy::LowBandNull => FillMeta {
            pad: 288,
            ends_at_zero: false,
        },
    };
    debug_assert!(
        meta.pad >= ANCHOR as i64,
        "a fill needs room for its anchors"
    );
    meta
}

/// A replacement segment for one correction window.
pub trait Fill: Send + Sync {
    /// Context samples read from before the window.
    fn pad(&self) -> i64;

    /// Whether the last replacement sample is ~zero.
    fn ends_at_zero(&self) -> bool;

    /// The replacement for the window, `w1 - pad` samples long.
    ///
    /// `ctx` covers `[window_start - pad, window_end + pad)` as full-scale
    /// floats, so the window itself starts at `ctx[pad]` and ends at `ctx[w1]`
    /// (exclusive).
    fn build(&self, ctx: &[f64], pad: usize, w1: usize) -> Vec<f64>;
}

/// The fill that corrects a policy, built from the configuration.
pub fn make_fill(policy: Policy, cfg: &Config) -> Box<dyn Fill> {
    match policy {
        Policy::Interp => Box::new(InterpFill),
        Policy::Descend => Box::new(DescendFill),
        Policy::Mute => Box::new(MuteFill),
        Policy::LowBandNull => Box::new(LowBandNullFill::new(cfg.lf_cut())),
    }
}

/// Straight line between anchors taken from just outside the window.
pub struct InterpFill;

impl Fill for InterpFill {
    fn pad(&self) -> i64 {
        fill_meta(Policy::Interp).pad
    }

    fn ends_at_zero(&self) -> bool {
        fill_meta(Policy::Interp).ends_at_zero
    }

    fn build(&self, ctx: &[f64], pad: usize, w1: usize) -> Vec<f64> {
        line_between(near_anchor(ctx, pad), far_anchor(ctx, w1), w1 - pad)
    }
}

/// Straight line from the near anchor to zero at `window_end`.
///
/// The `interp` chord is measured (../ubc125-ml/docs/prototype.md) to sit *on* the `off`
/// class click, because a 10 ms `post` ends mid-swing of one slow excursion
/// (F13) and both anchors land inside it. Zero is the one value the recovery
/// ramp can continue from without a step, so it replaces the far anchor.
/// `Config` refuses this fill without a recovery tail.
pub struct DescendFill;

impl Fill for DescendFill {
    fn pad(&self) -> i64 {
        fill_meta(Policy::Descend).pad
    }

    fn ends_at_zero(&self) -> bool {
        fill_meta(Policy::Descend).ends_at_zero
    }

    fn build(&self, ctx: &[f64], pad: usize, w1: usize) -> Vec<f64> {
        line_between(near_anchor(ctx, pad), 0.0, w1 - pad)
    }
}

/// Silence.
pub struct MuteFill;

impl Fill for MuteFill {
    fn pad(&self) -> i64 {
        fill_meta(Policy::Mute).pad
    }

    fn ends_at_zero(&self) -> bool {
        fill_meta(Policy::Mute).ends_at_zero
    }

    fn build(&self, _ctx: &[f64], pad: usize, w1: usize) -> Vec<f64> {
        vec![0.0; w1 - pad]
    }
}

/// Original minus its own low-frequency component, inside the window.
///
/// A causal 64-tap FIR low-pass is subtracted from the window, so the
/// low-frequency thump cancels and the higher-frequency content the click was
/// covering survives (F4). The pad before the window is read into the ring and
/// paid for in the delay, but — as in the reference — it is *not* fed to the
/// filter, which starts cold at the window; see the `lf-null` notes in
/// `../ubc125-ml/docs/prototype.md`.
pub struct LowBandNullFill {
    taps: Vec<f64>,
    zi: Vec<f64>,
}

impl LowBandNullFill {
    pub fn new(cut_hz: f64) -> Self {
        let taps = firwin_lowpass(LF_TAPS, cut_hz, RATE);
        let zi = step_state(&taps);
        LowBandNullFill { taps, zi }
    }
}

impl Fill for LowBandNullFill {
    fn pad(&self) -> i64 {
        fill_meta(Policy::LowBandNull).pad
    }

    fn ends_at_zero(&self) -> bool {
        fill_meta(Policy::LowBandNull).ends_at_zero
    }

    fn build(&self, ctx: &[f64], pad: usize, w1: usize) -> Vec<f64> {
        let seg = &ctx[pad..];
        let low = self.low_pass(seg);
        let span = w1 - pad;
        (0..span).map(|i| ctx[pad + i] - low[i]).collect()
    }
}

impl LowBandNullFill {
    /// Transposed direct-form II with the reference's step-response state.
    fn low_pass(&self, seg: &[f64]) -> Vec<f64> {
        assert!(!seg.is_empty(), "lf-null needs at least one window sample");
        let scale = seg[0];
        let mut state: Vec<f64> = self.zi.iter().map(|z| z * scale).collect();
        let last = self.taps.len() - 1;
        let mut out = Vec::with_capacity(seg.len());
        for &sample in seg {
            out.push(self.taps[0] * sample + state[0]);
            for k in 0..last - 1 {
                state[k] = self.taps[k + 1] * sample + state[k + 1];
            }
            state[last - 1] = self.taps[last] * sample;
        }
        out
    }
}

/// Weights that blend a replacement in and out over `ramp` samples.
pub fn cosine_blend(n: usize, ramp: i64, right: RightEdge) -> Vec<f64> {
    let mut w = vec![1.0; n];
    let ramp = (ramp as usize).min(n / 2);
    if ramp > 0 {
        let edge = raised_edge(ramp);
        w[..ramp].copy_from_slice(&edge);
        if right == RightEdge::Fade {
            let start = n - ramp;
            for (i, value) in edge.iter().rev().enumerate() {
                w[start + i] = *value;
            }
        }
    }
    w
}

/// Raised-cosine gains from just above zero up to 1.0, for the ring-down tail.
pub fn ramp_to_unity(n: usize) -> Vec<f64> {
    raised_edge(n)
}

/// `n` raised-cosine gains, `(0, 1)`, rising with `n` — the blend edge shape.
fn raised_edge(n: usize) -> Vec<f64> {
    let divisor = (n + 1) as f64;
    (1..=n)
        .map(|i| 0.5 * (1.0 - (((i as f64) * std::f64::consts::PI) / divisor).cos()))
        .collect()
}

/// `n` values running from `near` to `far`, reaching `far` one sample past the
/// last written index. `far` therefore describes `window_end`, the first sample
/// the replacement does not write, which is what makes a zero-valued `far` join
/// the recovery ramp.
pub fn line_between(near: f64, far: f64, n: usize) -> Vec<f64> {
    let divisor = (n + 1) as f64;
    let chord = far - near;
    (1..=n)
        .map(|i| near + chord * ((i as f64) / divisor))
        .collect()
}

/// Median of the `ANCHOR` original samples just before the window.
pub fn near_anchor(ctx: &[f64], pad: usize) -> f64 {
    median(&ctx[pad - ANCHOR..pad])
}

/// Median of the `ANCHOR` original samples just after the window.
pub fn far_anchor(ctx: &[f64], w1: usize) -> f64 {
    median(&ctx[w1..w1 + ANCHOR])
}

/// Median of a short stretch of full-scale samples.
pub fn median(values: &[f64]) -> f64 {
    assert!(!values.is_empty(), "median of no samples is undefined");
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a sample stream"));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// scipy `firwin(numtaps, cutoff, fs=RATE)` for a low pass, tap for tap.
///
/// The operation order is the one that reproduces scipy 1.18 bit for bit:
/// `fc * sinc(fc * m)` windowed by a symmetric Hamming window, then divided by
/// the pairwise sum of the windowed impulse response. A test pins every tap
/// against the values the reference rig gets from scipy.
pub fn firwin_lowpass(numtaps: usize, cutoff_hz: f64, sample_rate: f64) -> Vec<f64> {
    let nyquist = 0.5 * sample_rate;
    let fc = cutoff_hz / nyquist;
    let alpha = 0.5 * (numtaps as f64 - 1.0);
    let window = hamming_symmetric(numtaps);
    let mut h = vec![0.0; numtaps];
    for i in 0..numtaps {
        let m = (i as f64) - alpha;
        h[i] = fc * sinc(fc * m) * window[i];
    }
    let scale = pairwise_sum(&h);
    h.iter().map(|value| value / scale).collect()
}

/// `scipy.signal.windows.hamming(n, sym=True)`, tap for tap.
fn hamming_symmetric(n: usize) -> Vec<f64> {
    let pi = std::f64::consts::PI;
    let step = (2.0 * pi) / (n - 1) as f64;
    let alpha = 0.54;
    let beta = 1.0 - alpha;
    (0..n)
        .map(|i| {
            let facet = if i + 1 == n {
                pi
            } else {
                (i as f64) * step - pi
            };
            alpha + beta * facet.cos()
        })
        .collect()
}

/// `np.sinc`: the normalized sinc, `sin(pi x) / (pi x)`.
fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let y = std::f64::consts::PI * x;
    y.sin() / y
}

/// numpy's pairwise sum for a float64 stretch below its 128-element blocksize:
/// eight running partials, a fixed combination tree, then the leftover elements
/// added back to front.
fn pairwise_sum(values: &[f64]) -> f64 {
    let n = values.len();
    if n < 8 {
        return values.iter().fold(0.0, |acc, v| acc + v);
    }
    let mut s = [0.0f64; 8];
    s.copy_from_slice(&values[..8]);
    let blocks = n / 8;
    for block in 1..blocks {
        for k in 0..8 {
            s[k] += values[block * 8 + k];
        }
    }
    let (a, b, c, d) = (s[0] + s[4], s[1] + s[5], s[2] + s[6], s[3] + s[7]);
    let mut sum = (a + c) + (b + d);
    let tail = blocks * 8;
    for index in (tail..n).rev() {
        sum += values[index];
    }
    sum
}

/// `scipy.signal.lfilter_zi(b, [1.0])`: the state that holds a step response
/// steady, i.e. the reverse cumulative sum of the taps after the first.
fn step_state(taps: &[f64]) -> Vec<f64> {
    let last = taps.len() - 1;
    let mut zi = vec![0.0; last];
    let mut acc = taps[last];
    zi[last - 1] = acc;
    for k in (0..last - 1).rev() {
        acc += taps[k + 1];
        zi[k] = acc;
    }
    zi
}

/// i16 samples as full-scale floats, the conversion every level column uses.
pub fn full_scale(sample: i16) -> f64 {
    f64::from(sample) / FS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firwin_reproduces_scipy_bit_for_bit() {
        // Reference taps: the bit pattern of every value of
        // `scipy.signal.firwin(64, 180.0, fs=48000)`, printed in the dev shell
        // (scipy 1.18.0, numpy 2.5.1) with
        //   struct.unpack('<Q', struct.pack('<d', float(v)))[0]
        // so the ported design is pinned to bits, not to a decimal.
        const EXPECTED: [u64; 64] = [
            0x3f61c3bf6806ee2eu64,
            0x3f62619d3b8009f6u64,
            0x3f6405d6d5a8c99eu64,
            0x3f66b0c76c9fdbefu64,
            0x3f6a5fd783ad4ab2u64,
            0x3f6f0d7d275689cbu64,
            0x3f7258a27e74403du64,
            0x3f759ff20d5e3d74u64,
            0x3f7955a94175a4fcu64,
            0x3f7d7176ff70ae5au64,
            0x3f80f4e93febfff0u64,
            0x3f835a0a43588248u64,
            0x3f85e24a0dd640c3u64,
            0x3f8887633846d4a4u64,
            0x3f8b42b0cc93af1cu64,
            0x3f8e0d40d7887616u64,
            0x3f906ff3fccc092bu64,
            0x3f91d9aade21dfacu64,
            0x3f934014b79c984bu64,
            0x3f949f83a72c92e7u64,
            0x3f95f456d85c9f1bu64,
            0x3f973b05002f5a9cu64,
            0x3f9870269d72c836u64,
            0x3f99907fde43da47u64,
            0x3f9a990a0c7ee5b9u64,
            0x3f9b86fc643d6775u64,
            0x3f9c57d43940f0e8u64,
            0x3f9d095c5240feafu64,
            0x3f9d99b363776016u64,
            0x3f9e075194751981u64,
            0x3f9e510d0037cb6du64,
            0x3f9e761d219c4d24u64,
            0x3f9e761d219c4d24u64,
            0x3f9e510d0037cb6cu64,
            0x3f9e075194751981u64,
            0x3f9d99b363776016u64,
            0x3f9d095c5240feafu64,
            0x3f9c57d43940f0e8u64,
            0x3f9b86fc643d6774u64,
            0x3f9a990a0c7ee5b9u64,
            0x3f99907fde43da46u64,
            0x3f9870269d72c835u64,
            0x3f973b05002f5a98u64,
            0x3f95f456d85c9f1bu64,
            0x3f949f83a72c92e4u64,
            0x3f934014b79c984bu64,
            0x3f91d9aade21dfaau64,
            0x3f906ff3fccc092bu64,
            0x3f8e0d40d7887613u64,
            0x3f8b42b0cc93af16u64,
            0x3f8887633846d4a2u64,
            0x3f85e24a0dd640beu64,
            0x3f835a0a43588248u64,
            0x3f80f4e93febffedu64,
            0x3f7d7176ff70ae5au64,
            0x3f7955a94175a4f7u64,
            0x3f759ff20d5e3d6eu64,
            0x3f7258a27e744038u64,
            0x3f6f0d7d275689c1u64,
            0x3f6a5fd783ad4ab2u64,
            0x3f66b0c76c9fdbefu64,
            0x3f6405d6d5a8c99eu64,
            0x3f62619d3b8009f6u64,
            0x3f61c3bf6806ee2eu64,
        ];
        let got = firwin_lowpass(64, 180.0, RATE);
        let bits: Vec<u64> = got.iter().map(|v| v.to_bits()).collect();
        assert_eq!(bits, EXPECTED, "the low-pass taps moved off scipy's");
        assert!((got.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn step_state_reproduces_lfilter_zi_bit_for_bit() {
        // scipy.signal.lfilter_zi(firwin(64, 180.0, fs=48000), [1.0]).
        const EXPECTED: [u64; 63] = [
            0x3fefee3c4097f912u64,
            0x3fefdbdaa35c7908u64,
            0x3fefc7d4cc86d03eu64,
            0x3fefb124051a3062u64,
            0x3fef96c42d968317u64,
            0x3fef77b6b06f2c8du64,
            0x3fef53056b72440du64,
            0x3fef27c587578792u64,
            0x3feef51a34d49c48u64,
            0x3feeba3746d5baebu64,
            0x3fee7663a1d60aebu64,
            0x3fee28fb78c8a8e2u64,
            0x3fedd17250914fdfu64,
            0x3fed6f54c3b0348cu64,
            0x3fed024a007de5d0u64,
            0x3fec8a14fd1fc3f8u64,
            0x3fec06955d3963afu64,
            0x3feb77c8064854b2u64,
            0x3feaddc7608b6ff0u64,
            0x3fea38cb43520b59u64,
            0x3fe989288c8f2660u64,
            0x3fe8cf50648dab8bu64,
            0x3fe80bcf2fa21549u64,
            0x3fe73f4b30aff677u64,
            0x3fe66a82e04bff49u64,
            0x3fe58e4afd2a140du64,
            0x3fe4ab8c5b600c86u64,
            0x3fe3c34178ce0491u64,
            0x3fe2d673ddb24990u64,
            0x3fe1e639510ea0c4u64,
            0x3fe0f3b0e90ce269u64,
            0x3fdfffffffffffffu64,
            0x3fde189e2de63b2du64,
            0x3fdc338d5de2be76u64,
            0x3fda5318449b6cdeu64,
            0x3fd8797d0e63f6ddu64,
            0x3fd6a8e7493fe6f2u64,
            0x3fd4e36a05abd7e4u64,
            0x3fd32afa3f68016du64,
            0x3fd181699ea01311u64,
            0x3fcfd0c34177aad9u64,
            0x3fccc2be6dc951d2u64,
            0x3fc9db5dcdc3667fu64,
            0x3fc71cd2f2b7d29cu64,
            0x3fc488e27dd24040u64,
            0x3fc220dfe6dead37u64,
            0x3fbfcb551634e283u64,
            0x3fbbaf581701e038u64,
            0x3fb7edaffc10d176u64,
            0x3fb48559e27e5b93u64,
            0x3fb1746d7b7580ffu64,
            0x3fad7048737571ceu64,
            0x3fa899c5e29f513cu64,
            0x3fa45c8b92a45141u64,
            0x3fa0ae5cb2b63b76u64,
            0x3f9b074f150f0daeu64,
            0x3f959f5291b77e52u64,
            0x3f910929f21a6e44u64,
            0x3f8a4ef49a5f3a18u64,
            0x3f83b6feb973e76cu64,
            0x3f7c1599bc97e0e1u64,
            0x3f7212ae51c37c12u64,
            0x3f61c3bf6806ee2eu64,
        ];
        let taps = firwin_lowpass(64, 180.0, RATE);
        let bits: Vec<u64> = step_state(&taps).iter().map(|v| v.to_bits()).collect();
        assert_eq!(bits, EXPECTED, "the step state moved off lfilter_zi's");
    }

    #[test]
    fn every_fill_agrees_with_the_policy_table() {
        let cfg = Config::default();
        for policy in Policy::ALL {
            let fill = make_fill(policy, &cfg);
            assert_eq!(
                FillMeta {
                    pad: fill.pad(),
                    ends_at_zero: fill.ends_at_zero()
                },
                fill_meta(policy),
                "{policy} disagrees with fill_meta"
            );
        }
    }

    #[test]
    fn line_between_reaches_far_one_sample_past_the_end() {
        let line = line_between(1.0, 3.0, 4);
        assert_eq!(line.len(), 4);
        assert_eq!(line[0], 1.0 + 2.0 * (1.0 / 5.0));
        assert!(line[3] < 3.0);
        assert_eq!(line_between(0.5, 0.0, 3)[2], 0.5 * (1.0 - 3.0 / 4.0));
    }

    #[test]
    fn cosine_blend_holds_or_fades_the_right_edge() {
        let faded = cosine_blend(10, 2, RightEdge::Fade);
        assert_eq!(faded[0], faded[9]);
        assert!(faded[0] < 1.0 && faded[2] == 1.0);
        let held = cosine_blend(10, 2, RightEdge::Hold);
        assert_eq!(held[9], 1.0);
        assert_eq!(&held[..2], &faded[..2]);
        // The ramp is clamped to half the window, as in the reference: a 6-sample
        // window with a 96-sample ramp is all edge, and Hold keeps the right half.
        let clamped = cosine_blend(6, 96, RightEdge::Fade);
        assert!(clamped[0] < 1.0 && clamped[3] < 1.0);
        assert_eq!(cosine_blend(6, 96, RightEdge::Hold)[3], 1.0);
    }

    #[test]
    fn ramp_to_unity_rises_without_reaching_the_ends() {
        let ramp = ramp_to_unity(48);
        assert!(ramp[0] > 0.0 && ramp[0] < 0.01);
        assert_eq!(ramp.len(), 48);
        assert!(ramp[47] < 1.0);
        // The blind spot recorded in ../ubc125-ml/docs/prototype.md: from a 402-sample ramp on,
        // the last gain is within half an LSB of unity *once quantized onto
        // full-scale material*, so the final sample of a long tail is
        // bit-identical to leaving it alone and no test can see gain_end there.
        let long = ramp_to_unity(7200);
        assert!(long[7199] > 0.99999, "last gain {}", long[7199]);
        assert_eq!((long[7199] * 32767.0).round_ties_even() as i16, 32767);
    }

    #[test]
    fn anchors_are_medians_outside_the_window() {
        let pad = 8;
        let mut ctx = vec![0.0; 24];
        for (i, v) in ctx.iter_mut().enumerate() {
            *v = i as f64;
        }
        assert_eq!(near_anchor(&ctx, pad), 5.5);
        assert_eq!(far_anchor(&ctx, pad + 8), 17.5);
        // A spike in the anchor region moves a median far less than a mean:
        // of these four samples the mean would be 254.5, the median 6.5. The
        // rig's own fixture makes the same point at filter level with an
        // outlier two samples before the window.
        ctx[4] = 1000.0;
        assert_eq!(near_anchor(&ctx, pad), 6.5);
    }

    #[test]
    fn median_handles_both_parities() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[3.0, 1.0, 2.0, 0.0]), 1.5);
    }
}
