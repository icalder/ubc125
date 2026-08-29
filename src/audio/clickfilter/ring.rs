//! PCM ring buffer: the single store the filter reads, decorates and emits from.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/ring.py`. Positions are absolute sample indices;
//! a slot is written once at push time and may be decorated (gain, replacement
//! blend) until it is emitted.

use crate::audio::clickfilter::constants::FS;

/// Highest full-scale value `take` allows: `1.0 - 1/FS`, i.e. `i16::MAX`.
const FULL_SCALE_MAX: f64 = 1.0 - 1.0 / FS;
/// Lowest full-scale value `take` allows, i.e. `i16::MIN`.
const FULL_SCALE_MIN: f64 = -1.0;

/// Fixed-capacity PCM slots: originals, per-slot gain, replacement blend.
pub struct PcmRing {
    capacity: usize,
    orig: Vec<i16>,
    gain: Vec<f64>,
    repl: Vec<f64>,
    has_repl: Vec<bool>,
}

/// One contiguous stretch of slots for a possibly wrapping range.
struct Slice {
    index: usize,
    count: usize,
    source: usize,
}

impl PcmRing {
    pub fn new(capacity: usize) -> Self {
        PcmRing {
            capacity,
            orig: vec![0; capacity],
            gain: vec![1.0; capacity],
            repl: vec![0.0; capacity],
            has_repl: vec![false; capacity],
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Store `samples` at absolute position `start`, clearing every decoration.
    ///
    /// A slot must not keep a previous occupant's replacement or gain, or an old
    /// click's correction is applied to fresh audio.
    pub fn push(&mut self, start: i64, samples: &[i16]) {
        for slice in self.slices(start, samples.len()) {
            let to = slice.index + slice.count;
            let from = slice.source;
            self.orig[slice.index..to].copy_from_slice(&samples[from..from + slice.count]);
            self.gain[slice.index..to].fill(1.0);
            self.has_repl[slice.index..to].fill(false);
        }
    }

    /// Original samples over `[start, start+n)` as full-scale floats.
    pub fn read(&self, start: i64, n: usize) -> Vec<f64> {
        let mut out = vec![0.0; n];
        for slice in self.slices(start, n) {
            let to = slice.index + slice.count;
            for (dst, &src) in out
                .iter_mut()
                .skip(slice.source)
                .zip(self.orig[slice.index..to].iter())
            {
                *dst = f64::from(src) / FS;
            }
        }
        out
    }

    /// Multiply the per-slot gain by `curve`, position by position.
    pub fn scale_gains(&mut self, start: i64, curve: &[f64]) {
        for slice in self.slices(start, curve.len()) {
            for offset in 0..slice.count {
                let index = slice.index + offset;
                self.gain[index] *= curve[slice.source + offset];
            }
        }
    }

    /// Mix `values` into the slots using cosine weights in [0, 1].
    ///
    /// Where an earlier correction already replaced a slot, that replacement is
    /// kept (first correction wins). Returns how many slots overlapped.
    pub fn blend_replacement(&mut self, start: i64, values: &[f64], weights: &[f64]) -> usize {
        assert_eq!(
            values.len(),
            weights.len(),
            "blend needs one weight per value"
        );
        let mut overlapped = 0;
        for slice in self.slices(start, values.len()) {
            for offset in 0..slice.count {
                let index = slice.index + offset;
                let source = slice.source + offset;
                let slot = f64::from(self.orig[index]) / FS;
                let weight = weights[source];
                if self.has_repl[index] {
                    overlapped += 1;
                } else {
                    self.repl[index] = slot * (1.0 - weight) + values[source] * weight;
                }
                self.has_repl[index] = true;
            }
        }
        overlapped
    }

    /// Final i16 samples over `[start, start+n)`, saturated to the i16 range.
    ///
    /// Rounding is half-to-even (`np.rint` in the reference) and the clamp
    /// happens before quantizing, so a replacement saturates instead of wrapping.
    pub fn take(&self, start: i64, n: usize) -> Vec<i16> {
        let mut out = vec![0i16; n];
        for slice in self.slices(start, n) {
            for offset in 0..slice.count {
                let index = slice.index + offset;
                let base = if self.has_repl[index] {
                    self.repl[index]
                } else {
                    f64::from(self.orig[index]) / FS
                };
                let shaped = base * self.gain[index];
                out[slice.source + offset] = quantize(shaped);
            }
        }
        out
    }

    /// `(index, count, source)` for a possibly wrapping range.
    fn slices(&self, start: i64, n: usize) -> Vec<Slice> {
        if n == 0 {
            return Vec::new();
        }
        assert!(
            n <= self.capacity,
            "range of {n} samples exceeds ring capacity {}",
            self.capacity
        );
        // Python's `%` floors, so a negative start wraps the same way a
        // positive one does; `rem_euclid` is that operator.
        let first = start.rem_euclid(self.capacity as i64) as usize;
        let last = first + n;
        if last <= self.capacity {
            vec![Slice {
                index: first,
                count: n,
                source: 0,
            }]
        } else {
            let head = self.capacity - first;
            vec![
                Slice {
                    index: first,
                    count: head,
                    source: 0,
                },
                Slice {
                    index: 0,
                    count: last - self.capacity,
                    source: head,
                },
            ]
        }
    }
}

/// Clamp to full scale, then quantize half-to-even — the ring's one rounding.
fn quantize(full_scale: f64) -> i16 {
    let clamped = full_scale.clamp(FULL_SCALE_MIN, FULL_SCALE_MAX);
    (clamped * FS).round_ties_even() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_read_write_take() {
        // The exact expectations of test_wrap_read_write_take in the rig's suite.
        let mut r = PcmRing::new(7);
        r.push(0, &[10, 20, 30]);
        r.push(3, &[40, 50, 60, 70]);
        r.push(7, &[80, 90]); // wraps into slots 0, 1
        assert_eq!(
            r.read(2, 7),
            [30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0]
                .iter()
                .map(|v| v / FS)
                .collect::<Vec<f64>>()
        );
        r.scale_gains(5, &[2.0, 0.5, 1.0]);
        assert_eq!(r.blend_replacement(6, &[0.1, -0.2], &[1.0, 1.0]), 0);
        assert_eq!(r.take(2, 7), vec![30, 40, 50, 120, 1638, -6554, 90]);
    }

    #[test]
    fn repush_clears_slot_decorations() {
        let mut r = PcmRing::new(8);
        r.push(0, &[0; 8]);
        r.blend_replacement(1, &[0.5, 0.5], &[1.0, 1.0]);
        r.scale_gains(4, &[0.5, 0.5]);
        r.push(0, &[7000; 8]);
        assert_eq!(r.take(0, 8), vec![7000; 8]);
    }

    #[test]
    fn take_rounds_half_to_even() {
        // 100.5 -> 100, 101.5 -> 102, -100.5 -> -100, -101.5 -> -102: the four
        // values that separate rint from floor and from truncation.
        let vals: Vec<f64> = [100.25, 100.5, 100.75, 101.5, 102.5, -100.5, -101.5]
            .iter()
            .map(|v| v / FS)
            .collect();
        let weights = vec![1.0; vals.len()];
        let mut r = PcmRing::new(16);
        r.push(0, &[0; 8]);
        r.blend_replacement(0, &vals, &weights);
        assert_eq!(
            r.take(0, vals.len()),
            vec![100, 100, 101, 102, 102, -100, -102]
        );
    }

    #[test]
    fn take_saturates_without_wrapping() {
        // 1.0 must not become -32768.
        let mut r = PcmRing::new(16);
        r.push(0, &[0; 8]);
        r.blend_replacement(0, &[5.0, -5.0, 1.0, -1.0], &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(r.take(0, 4), vec![32767, -32768, 32767, -32768]);
    }

    #[test]
    fn overlap_keeps_the_first_replacement() {
        let mut r = PcmRing::new(16);
        r.push(0, &[1000; 8]);
        assert_eq!(r.blend_replacement(2, &[0.5, 0.5], &[1.0, 1.0]), 0);
        assert_eq!(r.blend_replacement(3, &[-0.25, -0.25], &[1.0, 1.0]), 1);
        assert_eq!(r.take(2, 3), vec![16384, 16384, -8192]);
    }

    #[test]
    #[should_panic(expected = "exceeds ring capacity")]
    fn read_wider_than_the_ring_is_refused() {
        let r = PcmRing::new(7);
        r.read(0, 8);
    }
}
