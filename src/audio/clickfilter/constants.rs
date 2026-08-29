//! Physical constants and the click-class vocabulary shared by every module.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/constants.py`. The numbers here are the numbers
//! the Python reference rig runs with; changing one without re-running the
//! byte comparison against the rig invalidates every listening result quoted in
//! `../ubc125-ml/docs/prototype.md`.

use core::fmt;

/// Full scale in sample units: the reference rig converts i16 to float by
/// dividing by this, and quantizes back by multiplying by it.
pub const FS: f64 = 32_768.0;
/// Sample rate in Hz (mono, S16_LE — non-negotiable rules 1 and 2).
pub const RATE: f64 = 48_000.0;
/// Production frame size in samples, `../ubc125-ml/docs/deployment.md`.
pub const FRAME: usize = 960;
/// Largest magnitude an S16 sample can hold, and the value `np.clip` allows
/// after quantizing (`1.0 - 1/FS` in full-scale units).
pub const SAMPLE_MAX: i16 = i16::MAX;
/// Smallest magnitude an S16 sample can hold; the i16 range is asymmetric, so
/// `-1.0` full scale is representable and `+1.0` is not.
pub const SAMPLE_MIN: i16 = i16::MIN;

/// Plateau-length classes, in run-length order (F2, F10, F12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClickClass {
    Short,
    Long,
    Xlong,
    Other,
}

impl ClickClass {
    /// Every class the vocabulary knows, in run-length order.
    pub const ALL: [ClickClass; 4] = [
        ClickClass::Short,
        ClickClass::Long,
        ClickClass::Xlong,
        ClickClass::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ClickClass::Short => "short",
            ClickClass::Long => "long",
            ClickClass::Xlong => "xlong",
            ClickClass::Other => "other",
        }
    }

    pub fn parse(name: &str) -> Option<ClickClass> {
        ClickClass::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

impl fmt::Display for ClickClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A plateau-length band: `[lo, hi)` samples.
#[derive(Debug, Clone, Copy)]
pub struct ClassBand {
    pub class: ClickClass,
    pub lo: i64,
    pub hi: i64,
}

/// Class bounds in samples at 48 kHz, from the run-length histogram
/// (F2, F10). `Xlong` is classification only: it is off by default in
/// `on_classes`, because F9/F12 read those events as click+speech (Q3). The
/// upper bound is exclusive and equals the default `max_plateau`, so a capped
/// piece (a run of exactly `max_plateau` samples) is always `Other`.
pub const CLASS_BOUNDS: [ClassBand; 3] = [
    ClassBand {
        class: ClickClass::Short,
        lo: 60,
        hi: 100,
    },
    ClassBand {
        class: ClickClass::Long,
        lo: 140,
        hi: 170,
    },
    ClassBand {
        class: ClickClass::Xlong,
        lo: 240,
        hi: 400,
    },
];

/// What a correction writes inside its window (see `fill`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Policy {
    Interp,
    Descend,
    Mute,
    LowBandNull,
}

impl Policy {
    pub const ALL: [Policy; 4] = [
        Policy::Interp,
        Policy::Descend,
        Policy::Mute,
        Policy::LowBandNull,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Interp => "interp",
            Policy::Descend => "descend",
            Policy::Mute => "mute",
            Policy::LowBandNull => "lf-null",
        }
    }

    pub fn parse(name: &str) -> Option<Policy> {
        Policy::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which side of the clip band triggers detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// Only excursions at or below `-clip` (all measured clicks are negative, F1).
    Negative,
    /// Either rail.
    Any,
}

impl Polarity {
    pub fn as_str(self) -> &'static str {
        match self {
            Polarity::Negative => "negative",
            Polarity::Any => "any",
        }
    }

    pub fn parse(name: &str) -> Option<Polarity> {
        match name {
            "negative" => Some(Polarity::Negative),
            "any" => Some(Polarity::Any),
            _ => None,
        }
    }
}

/// Milliseconds to samples, rounded the way the reference rig rounds: half to
/// even, on the value `(ms / 1000) * RATE`.
pub fn ms_to_samples(ms: f64) -> i64 {
    (ms / 1000.0 * RATE).round_ties_even() as i64
}

/// Samples to milliseconds, for reporting.
pub fn samples_to_ms(samples: i64) -> f64 {
    samples as f64 / RATE * 1000.0
}
