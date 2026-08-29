//! Candidate detection: causal saturated-plateau runs, split into classes.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/detect.py`. The clip threshold is applied in
//! sample units (`clip * FS`), which selects exactly the same samples as
//! `|x / FS| >= clip` on the reference's float64 arrays.

use crate::audio::clickfilter::config::Config;
use crate::audio::clickfilter::constants::{CLASS_BOUNDS, ClickClass, FS, Polarity};

/// One saturated run: onset sample, exclusive end sample, capped flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub onset: i64,
    pub end: i64,
    pub run_len: i64,
    pub capped: bool,
}

impl Candidate {
    fn uncapped(onset: i64, end: i64) -> Self {
        Candidate {
            onset,
            end,
            run_len: end - onset,
            capped: false,
        }
    }

    fn capped(onset: i64, end: i64) -> Self {
        Candidate {
            onset,
            end,
            run_len: end - onset,
            capped: true,
        }
    }
}

/// Causal detector for saturated plateaus; one candidate per closed run.
///
/// Detection is bounded: a run closes when a sample leaves the clip band, or
/// when it reaches `max_plateau` samples, so it never waits on an unbounded
/// excursion. A run still open at end of input is closed by [`close_open`].
pub struct PlateauTrigger {
    polarity: Polarity,
    min_run: i64,
    max_plateau: i64,
    /// The clip band edge in sample units.
    clip: f64,
    open: Option<i64>,
}

impl PlateauTrigger {
    pub fn new(cfg: &Config) -> Self {
        PlateauTrigger {
            polarity: cfg.polarity(),
            min_run: cfg.min_run(),
            max_plateau: cfg.max_plateau(),
            clip: cfg.clip() * FS,
            open: None,
        }
    }

    /// Feed one frame of raw i16 samples that starts at absolute `base`.
    pub fn feed(&mut self, samples: &[i16], base: i64) -> Vec<Candidate> {
        let mut found: Vec<Candidate> = Vec::new();
        for (offset, &sample) in samples.iter().enumerate() {
            let position = base + offset as i64;
            if self.in_band(sample) {
                if self.open.is_none() {
                    self.open = Some(position);
                }
            } else if self.open.is_some() {
                found.extend(self.close_capped(position));
            }
        }
        // A run open at the frame edge that has already reached the cap is
        // closed at the cap so detection never waits on an unbounded excursion.
        // The clipped samples beyond the cap stay open and continue as the
        // remainder of the same physical run, so the split boundaries do not
        // depend on where the frame edges fall.
        let frame_end = base + samples.len() as i64;
        while let Some(onset) = self.open {
            if frame_end - onset < self.max_plateau {
                break;
            }
            found.push(self.split_capped(onset + self.max_plateau));
        }
        found.retain(|c| c.run_len >= self.min_run);
        found
    }

    /// Close a run still open at the end of input, splitting at the cap.
    pub fn close_open(&mut self, end: i64) -> Vec<Candidate> {
        if self.open.is_none() {
            return Vec::new();
        }
        let mut cands = self.close_capped(end);
        cands.retain(|c| c.run_len >= self.min_run);
        cands
    }

    fn in_band(&self, sample: i16) -> bool {
        let value = f64::from(sample);
        if self.polarity == Polarity::Negative {
            value <= -self.clip
        } else {
            value <= -self.clip || value >= self.clip
        }
    }

    /// Close the open run at `end`, splitting it at the plateau cap first.
    fn close_capped(&mut self, end: i64) -> Vec<Candidate> {
        let mut onset = self.open.take().expect("close_capped needs an open run");
        let mut out: Vec<Candidate> = Vec::new();
        while end - onset >= self.max_plateau {
            out.push(Candidate::capped(onset, onset + self.max_plateau));
            onset += self.max_plateau;
        }
        if end > onset {
            out.push(Candidate::uncapped(onset, end));
        }
        out
    }

    /// Close the open run at the cap, leaving `cutoff` onwards still open.
    fn split_capped(&mut self, cutoff: i64) -> Candidate {
        let onset = self.open.expect("split_capped needs an open run");
        self.open = Some(cutoff);
        Candidate::capped(onset, cutoff)
    }
}

/// Split a candidate by plateau length: return (class, will-correct?).
pub fn classify(candidate: &Candidate, cfg: &Config) -> (ClickClass, bool) {
    let class = CLASS_BOUNDS
        .into_iter()
        .find(|band| band.lo <= candidate.run_len && candidate.run_len < band.hi)
        .map_or(ClickClass::Other, |band| band.class);
    (class, cfg.is_on(class))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::clickfilter::constants::FRAME;

    fn plateau_signal(onset: usize, run_len: usize, total: usize) -> Vec<i16> {
        let mut x = vec![0i16; total];
        x[onset..onset + run_len].fill(-32768);
        x
    }

    fn triples(cands: &[Candidate]) -> Vec<(i64, i64, bool)> {
        cands.iter().map(|c| (c.onset, c.end, c.capped)).collect()
    }

    fn feed_framed(signal: &[i16], cfg: &Config) -> Vec<Candidate> {
        let mut trig = PlateauTrigger::new(cfg);
        let mut out = Vec::new();
        let whole = signal.len() - signal.len() % FRAME;
        for (frame, chunk) in signal[..whole].chunks(FRAME).enumerate() {
            out.extend(trig.feed(chunk, frame as i64 * FRAME as i64));
        }
        if whole < signal.len() {
            out.extend(trig.feed(&signal[whole..], whole as i64));
        }
        out.extend(trig.close_open(signal.len() as i64));
        out
    }

    fn feed_whole(signal: &[i16], cfg: &Config) -> Vec<Candidate> {
        let mut trig = PlateauTrigger::new(cfg);
        let mut out = trig.feed(signal, 0);
        out.extend(trig.close_open(signal.len() as i64));
        out
    }

    #[test]
    fn plateau_crossing_a_frame_boundary_yields_one_candidate() {
        let cfg = Config::default();
        let x = plateau_signal(950, 60, 3 * FRAME);
        let cands = feed_framed(&x, &cfg);
        assert_eq!(
            cands
                .iter()
                .map(|c| (c.onset, c.end, c.run_len, c.capped))
                .collect::<Vec<_>>(),
            vec![(950, 1010, 60, false)]
        );
    }

    #[test]
    fn negative_polarity_ignores_an_equal_positive_excursion() {
        let mut x = vec![0i16; 3 * FRAME];
        x[1000..1067].fill(32767);
        x[2000..2067].fill(-32768);
        let neg = feed_framed(&x, &Config::default());
        assert_eq!(
            neg.iter().map(|c| (c.onset, c.end)).collect::<Vec<_>>(),
            vec![(2000, 2067)]
        );
        let any = feed_framed(&x, &Config::builder().polarity(Polarity::Any).build());
        assert_eq!(
            any.iter().map(|c| (c.onset, c.end)).collect::<Vec<_>>(),
            vec![(1000, 1067), (2000, 2067)]
        );
    }

    #[test]
    fn cap_split_is_independent_of_frame_alignment() {
        let cfg = Config::default();
        let cases = [
            (560, 490, vec![(560, 960, true), (960, 1050, false)]),
            (
                100,
                1000,
                vec![(100, 500, true), (500, 900, true), (900, 1100, false)],
            ),
            (
                500,
                1000,
                vec![(500, 900, true), (900, 1300, true), (1300, 1500, false)],
            ),
            (
                860,
                1000,
                vec![(860, 1260, true), (1260, 1660, true), (1660, 1860, false)],
            ),
            (
                1900,
                980,
                vec![(1900, 2300, true), (2300, 2700, true), (2700, 2880, false)],
            ),
        ];
        for (onset, run_len, want) in cases {
            let x = plateau_signal(onset, run_len, 6 * FRAME);
            assert_eq!(
                triples(&feed_framed(&x, &cfg)),
                want,
                "onset={onset} framed"
            );
            assert_eq!(
                triples(&feed_whole(&x, &cfg)),
                want,
                "onset={onset} whole-buffer"
            );
        }
    }

    #[test]
    fn cap_flag_is_inclusive_at_max_plateau() {
        let cfg = Config::default();
        for (run_len, capped) in [(399, false), (400, true), (401, true)] {
            let x = plateau_signal(100, run_len, 6 * FRAME);
            let got = feed_framed(&x, &cfg);
            assert_eq!(
                triples(&got)[..1],
                [(100i64, 100 + run_len.min(400) as i64, capped)]
            );
        }
    }

    #[test]
    fn min_run_gates_every_path() {
        let x = plateau_signal(100, 4, FRAME);
        for (min_run, want) in [(3, vec![(100, 104)]), (4, vec![(100, 104)]), (5, vec![])] {
            let cfg = Config::builder().min_run(min_run).build();
            let got = feed_framed(&x, &cfg);
            assert_eq!(
                got.iter().map(|c| (c.onset, c.end)).collect::<Vec<_>>(),
                want,
                "min_run={min_run}"
            );
        }
        // A 2-sample clipped tail at end of input is not a candidate.
        let short = plateau_signal(2 * FRAME - 2, 2, 2 * FRAME);
        assert!(feed_framed(&short, &Config::default()).is_empty());
        let long = plateau_signal(2 * FRAME - 5, 5, 2 * FRAME);
        let got = feed_framed(&long, &Config::default());
        assert_eq!(
            got.iter().map(|c| (c.onset, c.end)).collect::<Vec<_>>(),
            vec![(2 * FRAME as i64 - 5, 2 * FRAME as i64)]
        );
    }

    #[test]
    fn detection_latency_is_bounded_by_max_plateau() {
        // A run still clipping at the frame edge must be released at the cap
        // while it is open, not when it finally closes.
        let cfg = Config::default();
        let x = plateau_signal(100, 1100, 4 * FRAME);
        let mut trig = PlateauTrigger::new(&cfg);
        let mut per_frame = Vec::new();
        for frame in x.chunks(FRAME) {
            let base = (per_frame.len() * FRAME) as i64;
            per_frame.push(triples(&trig.feed(frame, base)));
        }
        assert_eq!(per_frame[0], vec![(100, 500, true), (500, 900, true)]);
        assert_eq!(per_frame[1], vec![(900, 1200, false)]);
        assert_eq!(per_frame[2], vec![]);
        assert_eq!(per_frame[3], vec![]);
        assert!(triples(&trig.close_open(x.len() as i64)).is_empty());
    }

    #[test]
    fn run_open_at_eof_goes_through_the_cap() {
        let cfg = Config::default();
        let x = plateau_signal(100, 900, 2 * FRAME + 400);
        assert_eq!(
            triples(&feed_framed(&x, &cfg)),
            vec![(100, 500, true), (500, 900, true), (900, 1000, false)]
        );
    }

    #[test]
    fn class_bounds_match_the_run_length_histogram() {
        let cfg = Config::default();
        let table: [(i64, ClickClass); 16] = [
            (59, ClickClass::Other),
            (60, ClickClass::Short),
            (99, ClickClass::Short),
            (100, ClickClass::Other),
            (139, ClickClass::Other),
            (140, ClickClass::Long),
            (169, ClickClass::Long),
            (170, ClickClass::Other),
            (176, ClickClass::Other),
            (236, ClickClass::Other),
            (239, ClickClass::Other),
            (240, ClickClass::Xlong),
            (270, ClickClass::Xlong),
            (307, ClickClass::Xlong),
            (399, ClickClass::Xlong),
            (400, ClickClass::Other),
        ];
        for (run_len, want) in table {
            let cand = Candidate {
                onset: 0,
                end: run_len,
                run_len,
                capped: false,
            };
            let (class, correct) = classify(&cand, &cfg);
            assert_eq!(class, want, "run_len={run_len}");
            assert_eq!(
                correct,
                matches!(want, ClickClass::Short | ClickClass::Long),
                "run_len={run_len}"
            );
        }
        // A capped piece is `other` even with xlong switched on.
        let xlong_on = Config::builder()
            .on_classes(&[ClickClass::Short, ClickClass::Long, ClickClass::Xlong])
            .build();
        let capped = Candidate {
            onset: 0,
            end: 400,
            run_len: 400,
            capped: true,
        };
        assert_eq!(classify(&capped, &xlong_on), (ClickClass::Other, false));
        // The decision follows on_classes, not the class name.
        let other_only = Config::builder().on_classes(&[ClickClass::Other]).build();
        assert_eq!(
            classify(&Candidate::uncapped(0, 176), &other_only),
            (ClickClass::Other, true)
        );
        assert_eq!(
            classify(&Candidate::uncapped(0, 67), &other_only),
            (ClickClass::Short, false)
        );
    }
}
