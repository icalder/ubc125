//! Measurement and self-checks over (original, corrected, events).
//!
//! Port of the machine-checkable half of `../ubc125-ml/scripts/clickfilter/checks.py`:
//! pass-through, the post-window residual, the seam step and the per-class
//! profile. These compute numbers and the CLI prints them; the reference rig
//! still owns the markdown report.
//!
//! Every level figure goes through [`peaks`], which converts to float64 *before*
//! taking the absolute value: `-32768` is its own negative in i16 and a
//! corrected click plateau is exactly `-32768`, so measuring in i16 would report
//! a saturated click as a negative peak (that bug is why the rig's whole set of
//! level columns runs through this one helper).

use crate::audio::clickfilter::config::Config;
use crate::audio::clickfilter::constants::{ClickClass, FS};
use crate::audio::clickfilter::filter::{EventRecord, Metrics};
use crate::audio::clickfilter::format::rounded;

/// How long a post-window residual is measured for: 40 ms, the span the rig
/// measures whatever the tail length is.
pub const RESID_SPAN_SAMPLES: i64 = 1920;

/// A (start, end) span in sample positions; `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

impl Span {
    fn new(start: i64, end: i64) -> Self {
        Span { start, end }
    }
}

/// `max |x|` in full-scale units over each span; empty spans are skipped.
pub fn peaks(samples: &[i16], spans: &[Span]) -> Vec<f64> {
    let length = samples.len() as i64;
    spans
        .iter()
        .filter_map(|span| {
            let start = span.start.max(0);
            let end = span.end.min(length);
            if end > start {
                Some(Span::new(start, end))
            } else {
                None
            }
        })
        .map(|span| {
            samples[span.start as usize..span.end as usize]
                .iter()
                .fold(0.0f64, |acc, &sample| acc.max((f64::from(sample)).abs()))
                / FS
        })
        .collect()
}

/// Median of a list of levels, or 0 for an empty list, as the rig reports it.
pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    round4(crate::audio::clickfilter::fill::median(values))
}

/// As [`median`], but says when there was nothing to measure, so a column of
/// zeros cannot be read as "nothing left over".
pub fn median_or_none(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(round4(crate::audio::clickfilter::fill::median(values)))
    }
}

/// dBFS of a stretch of samples, or the floor when the stretch is empty.
pub fn dbfs(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return -120.0;
    }
    let mut sum = 0.0f64;
    for &sample in samples {
        let value = f64::from(sample) / FS;
        sum += value * value;
    }
    20.0 * ((sum / samples.len() as f64).sqrt() + 1e-12).log10()
}

/// The events a correction was actually applied to (window bookkeeping set).
pub fn applied(events: &[EventRecord]) -> Vec<&EventRecord> {
    events.iter().filter(|event| event.applied()).collect()
}

/// Start and end of every applied correction window.
pub fn window_spans(events: &[EventRecord]) -> Vec<Span> {
    events
        .iter()
        .filter_map(|event| match (event.window_start, event.window_end) {
            (Some(start), Some(end)) => Some(Span::new(start, end)),
            _ => None,
        })
        .collect()
}

/// The pass-through result: bit-exactness outside the recorded ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassThrough {
    pub length_match: bool,
    pub samples_changed: i64,
    pub changed_outside_windows: i64,
    pub worst_outside_delta: f64,
    pub median_peak_before: f64,
    pub median_peak_after: f64,
}

/// Non-click audio must be bit-exact, and every change must sit inside a window.
///
/// The protected range is `[window_start - 1, gain_end)`: the `-1` is there
/// because the crossfade gives the very first window sample a (small) weight,
/// and `gain_end` reaches past the window when a recovery ramp was scheduled.
/// The range comes from event bookkeeping, not from what the code touched, so a
/// policy that wrote somewhere no event records would slip through — see "Known
/// limits of the rig" in `../ubc125-ml/docs/prototype.md`.
pub fn pass_through_check(
    original: &[i16],
    corrected: &[i16],
    events: &[EventRecord],
) -> PassThrough {
    let mut touched = vec![false; original.len()];
    for event in events {
        let Some(start) = event.window_start else {
            continue;
        };
        let from = (start - 1).max(0) as usize;
        let cover = event
            .gain_end
            .or(event.window_end)
            .unwrap_or(event.end)
            .min(original.len() as i64) as usize;
        for slot in touched.iter_mut().take(cover).skip(from) {
            *slot = true;
        }
    }
    let length = original.len().min(corrected.len());
    let mut samples_changed = 0i64;
    let mut changed_outside = 0i64;
    let mut worst_outside_delta = 0.0f64;
    for index in 0..length {
        if original[index] == corrected[index] {
            continue;
        }
        samples_changed += 1;
        if !touched[index] {
            changed_outside += 1;
            let delta = (i64::from(corrected[index]) - i64::from(original[index])).abs();
            worst_outside_delta = worst_outside_delta.max(delta as f64);
        }
    }
    samples_changed += (original.len() as i64 - corrected.len() as i64).abs();
    let spans = window_spans(events);
    PassThrough {
        length_match: original.len() == corrected.len(),
        samples_changed,
        changed_outside_windows: changed_outside,
        worst_outside_delta,
        median_peak_before: median(&peaks(original, &spans)),
        median_peak_after: median(&peaks(corrected, &spans)),
    }
}

/// How much of the post-plateau ring-down survives the correction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Residual {
    pub resid_span_samples: i64,
    pub resid_events: usize,
    pub median_resid_before: Option<f64>,
    pub median_resid_after: Option<f64>,
}

/// Median `max |x|` over the 40 ms that starts at `window_end`.
///
/// The in-window peak columns cannot see a recovery tail at all, because the
/// tail only writes past the window. This figure mixes ring-down removed (good)
/// with speech attenuated (bad), so it is a listening axis, not a verdict (Q6).
/// Events whose span runs off the end of the recording are dropped.
pub fn ringdown_residual(original: &[i16], corrected: &[i16], events: &[EventRecord]) -> Residual {
    let ends: Vec<i64> = applied(events)
        .into_iter()
        .map(|event| event.window_end.unwrap_or(event.end))
        .collect();
    let spans: Vec<Span> = ends
        .iter()
        .map(|end| Span::new(*end, *end + RESID_SPAN_SAMPLES))
        .filter(|span| span.end <= original.len() as i64)
        .collect();
    let before = peaks(original, &spans);
    let after = peaks(corrected, &spans);
    Residual {
        resid_span_samples: RESID_SPAN_SAMPLES,
        resid_events: before.len(),
        median_resid_before: median_or_none(&before),
        median_resid_after: median_or_none(&after),
    }
}

/// The size of the step the correction itself makes at the window/ramp seam.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seam {
    pub seam_events: usize,
    pub seam_step_median: Option<f64>,
    pub seam_step_max: Option<f64>,
    pub seam_step_original_median: Option<f64>,
    pub seam_step_original_max: Option<f64>,
}

/// `|x[window_end] - x[window_end - 1]|` in the corrected stream, and in the
/// original at the same place.
///
/// A correction that ends at click level while the ramp starts at ~zero gain
/// makes a step of order one full scale there — a pop made by the filter, at the
/// boundary of the worst click in the recording. Only events with a tail are
/// measured: without a ramp there is nothing to join, and the column must say
/// "no events" rather than imply it measured something.
pub fn seam_steps(original: &[i16], corrected: &[i16], events: &[EventRecord]) -> Seam {
    let mut steps: Vec<f64> = Vec::new();
    let mut originals: Vec<f64> = Vec::new();
    for event in applied(events) {
        if event.tail_samples.unwrap_or(0) <= 0 {
            continue;
        }
        let at = event.window_end.unwrap_or(event.end) as usize;
        if at == 0 || at >= corrected.len() {
            continue;
        }
        steps.push(step_at(corrected, at));
        originals.push(step_at(original, at));
    }
    let (seam_step_median, seam_step_max) = median_and_max(&steps);
    let (seam_step_original_median, seam_step_original_max) = median_and_max(&originals);
    Seam {
        seam_events: steps.len(),
        seam_step_median,
        seam_step_max,
        seam_step_original_median,
        seam_step_original_max,
    }
}

fn step_at(samples: &[i16], at: usize) -> f64 {
    (i64::from(samples[at]) - i64::from(samples[at - 1])).abs() as f64 / FS
}

fn median_and_max(values: &[f64]) -> (Option<f64>, Option<f64>) {
    if values.is_empty() {
        return (None, None);
    }
    (
        median_or_none(values),
        Some(round4(
            values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )),
    )
}

/// One class's level and seam columns.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassRow {
    pub class: ClickClass,
    pub events: usize,
    pub corrected: usize,
    pub policy: String,
    pub tail_samples: i64,
    pub click_before: f64,
    pub click_after: f64,
    pub window_before: f64,
    pub window_after: f64,
    pub guard_before: f64,
    pub guard_after: f64,
    pub resid_before: Option<f64>,
    pub resid_after: Option<f64>,
    pub resid_events: usize,
    pub seam_step_median: Option<f64>,
    pub seam_step_max: Option<f64>,
    pub seam_step_original_max: Option<f64>,
}

/// The level and seam columns split by plateau-length class, in class-name
/// order. The pooled per-session columns hide a class-level failure (F13), which
/// is why the rig reports these and the CLI prints them.
pub fn class_profile(original: &[i16], corrected: &[i16], events: &[EventRecord]) -> Vec<ClassRow> {
    let mut classes: Vec<ClickClass> = ClickClass::ALL
        .into_iter()
        .filter(|class| events.iter().any(|event| event.class == *class))
        .collect();
    classes.sort_by_key(|class| class.as_str().to_string());
    classes
        .into_iter()
        .map(|class| profile_one(original, corrected, events, class))
        .collect()
}

fn profile_one(
    original: &[i16],
    corrected: &[i16],
    events: &[EventRecord],
    class: ClickClass,
) -> ClassRow {
    let group: Vec<EventRecord> = events
        .iter()
        .filter(|event| event.class == class)
        .cloned()
        .collect();
    let group_refs: Vec<&EventRecord> = group.iter().collect();
    let done = applied(&group);
    let plateaus: Vec<Span> = group_refs
        .iter()
        .map(|event| Span::new(event.onset, event.end))
        .collect();
    let spans: Vec<Span> = window_spans(&group);
    let guarded: Vec<Span> = done
        .iter()
        .map(|event| Span::new(event.window_start.unwrap_or(event.onset), event.onset))
        .collect();
    let resid = ringdown_residual(original, corrected, &group);
    let seam = seam_steps(original, corrected, &group);
    let mut policies: Vec<&str> = done
        .iter()
        .map(|event| event.policy.map_or("-", |policy| policy.as_str()))
        .collect();
    policies.sort();
    policies.dedup();
    ClassRow {
        class,
        events: group.len(),
        corrected: done.len(),
        policy: if policies.is_empty() {
            "-".to_string()
        } else {
            policies.join("+")
        },
        tail_samples: done
            .first()
            .and_then(|event| event.tail_samples)
            .unwrap_or(0),
        // The plateau on its own, for every class, corrected or not: a class
        // left uncorrected must not report a zero level.
        click_before: median(&peaks(original, &plateaus)),
        click_after: median(&peaks(corrected, &plateaus)),
        window_before: median(&peaks(original, &spans)),
        window_after: median(&peaks(corrected, &spans)),
        guard_before: median(&peaks(original, &guarded)),
        guard_after: median(&peaks(corrected, &guarded)),
        resid_before: resid.median_resid_before,
        resid_after: resid.median_resid_after,
        resid_events: resid.resid_events,
        seam_step_median: seam.seam_step_median,
        seam_step_max: seam.seam_step_max,
        seam_step_original_max: seam.seam_step_original_max,
    }
}

/// Levels just outside each correction window, to see what it eats.
pub fn add_context_stats(cfg: &Config, samples: &[i16], events: &mut [EventRecord]) {
    for event in events.iter_mut() {
        let start = event.window_start.unwrap_or(event.onset);
        let stop = event.window_end.unwrap_or(event.end);
        let from = (start - cfg.pre()).max(0) as usize;
        event.pre_dbfs = Some(dbfs(&samples[from..start as usize]));
        let to = (stop + cfg.post()).min(samples.len() as i64) as usize;
        event.post_dbfs = Some(dbfs(&samples[stop as usize..to]));
    }
}

/// The reference's `round(x, 4)` kept as a value, so a stored number and the
/// printed column cannot disagree.
fn round4(value: f64) -> f64 {
    rounded(value, 4)
        .parse::<f64>()
        .expect("a formatted number parses back")
}

/// The one-line summary, worded like the rig's so the two runs can be diffed.
pub fn summary_line(
    name: &str,
    seconds: f64,
    wall: f64,
    cfg: &Config,
    metrics: &Metrics,
    checks: &PassThrough,
) -> String {
    format!(
        "{name}: {seconds:.0}s in {wall:.2}s wall ({:.0}x RT) delay {:.1} ms [{} smp] \
         cand={} corr={} skip={} late={} changed-outside={}",
        seconds / wall,
        cfg.delay_ms(),
        cfg.delay(),
        metrics.candidates,
        metrics.corrected,
        metrics.skipped,
        metrics.late_writes,
        checks.changed_outside_windows,
    )
}

/// The per-class lines, formatted like the rig's `print_classes`.
pub fn class_lines(profile: &[ClassRow]) -> Vec<String> {
    fn pair(before: f64, after: f64) -> String {
        format!("{before:.4}->{after:.4}")
    }
    profile
        .iter()
        .map(|p| {
            let resid = match (p.resid_before, p.resid_after) {
                (Some(before), Some(after)) => pair(before, after),
                _ => "-       -".to_string(),
            };
            let seam = match p.seam_step_max {
                Some(value) => format!("{value:.4}"),
                // The rig pads the placeholder to the width of a measured value.
                None => "-     ".to_string(),
            };
            format!(
                "    {:<5} n={:>3} corr={:>3} policy={:>8} tail={:>5} click {} window {} \
                 resid {} seam {}",
                p.class.as_str(),
                p.events,
                p.corrected,
                p.policy,
                p.tail_samples,
                pair(p.click_before, p.click_after),
                pair(p.window_before, p.window_after),
                resid,
                seam,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::clickfilter::constants::{ClickClass, FRAME, Policy};
    use crate::audio::clickfilter::filter::run_filter;

    fn carrier(total: usize) -> Vec<i16> {
        (0..total)
            .map(|i| {
                (0.49 * 32767.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0).sin())
                    as i16
            })
            .collect()
    }

    fn plateau_signal(onset: usize, run_len: usize, total: usize) -> Vec<i16> {
        let mut x = vec![0i16; total];
        x[onset..onset + run_len].fill(-32768);
        x
    }

    /// A slow 20 Hz carrier with one plateau stamped in — the rig's `carrier`
    /// fixture. A silent carrier would make every fill write zeros into a window
    /// that is already zero, so the checks would have nothing to see.
    fn carrier_with_plateau(onset: usize, run_len: usize, total: usize) -> Vec<i16> {
        (0..total)
            .map(|i| {
                if (onset..onset + run_len).contains(&i) {
                    -32768
                } else {
                    (0.35
                        * 32767.0
                        * (2.0 * std::f64::consts::PI * 20.0 * i as f64 / 48000.0).sin())
                        as i16
                }
            })
            .collect()
    }

    #[test]
    fn pass_through_check_is_exact_on_a_clean_stream() {
        let cfg = Config::default();
        let x = carrier(5000);
        let (out, flt) = run_filter(&cfg, &x);
        let checks = pass_through_check(&x, &out, flt.events());
        assert_eq!(checks.changed_outside_windows, 0);
        assert_eq!(checks.samples_changed, 0);
        assert_eq!(checks.worst_outside_delta, 0.0);
        assert!(checks.length_match);
    }

    #[test]
    fn pass_through_check_has_teeth() {
        // A change the guard cannot see is worse than no guard at all.
        let cfg = Config::default();
        let x = carrier(5000);
        let (mut out, flt) = run_filter(&cfg, &x);
        out[300] += 1;
        let checks = pass_through_check(&x, &out, flt.events());
        assert_eq!(checks.samples_changed, 1);
        assert_eq!(checks.changed_outside_windows, 1);
        assert_eq!(checks.worst_outside_delta, 1.0);

        let dirty = carrier_with_plateau(2000, 67, 6 * FRAME);
        let (fixed, flt2) = run_filter(&cfg, &dirty);
        let good = pass_through_check(&dirty, &fixed, flt2.events());
        assert!(
            good.samples_changed > 100,
            "changed {}",
            good.samples_changed
        );
        assert_eq!(good.changed_outside_windows, 0);
        assert!(good.length_match);
    }

    #[test]
    fn protected_range_is_window_start_minus_one_to_gain_end() {
        let cfg = Config::default();
        let dirty = carrier_with_plateau(2000, 67, 6 * FRAME);
        let (fixed, flt) = run_filter(&cfg, &dirty);
        let event = &flt.events()[0];
        let start = event.window_start.unwrap();
        let gain_end = event.gain_end.unwrap();
        let poke = |index: usize| {
            let mut hurt = fixed.clone();
            hurt[index] += 1;
            pass_through_check(&dirty, &hurt, flt.events()).changed_outside_windows
        };
        for (index, want) in [
            (start as usize - 2, 1),
            (start as usize - 1, 0),
            (start as usize, 0),
            (start as usize + 48, 0),
            (gain_end as usize - 1, 0),
            (gain_end as usize, 1),
        ] {
            assert_eq!(poke(index), want, "poke at {index}");
        }
    }

    #[test]
    fn recovery_tail_samples_are_protected() {
        // A 1 ms tail (48 samples): with a long tail the last ramp gain is within
        // half an LSB of unity, so the boundary sample is bit-identical either
        // way and the edge cannot be seen at all.
        let cfg = Config::builder().tail_ms(ClickClass::Short, 1.0).build();
        assert_eq!(cfg.tail_samples(ClickClass::Short), 48);
        let dirty = carrier_with_plateau(2000, 67, 8 * FRAME);
        let (fixed, flt) = run_filter(&cfg, &dirty);
        let event = &flt.events()[0];
        let window_end = event.window_end.unwrap() as usize;
        let gain_end = event.gain_end.unwrap() as usize;
        assert_eq!(gain_end, window_end + 48);
        assert_ne!(&fixed[window_end..gain_end], &dirty[window_end..gain_end]);
        let poke = |index: usize| {
            let mut hurt = fixed.clone();
            hurt[index] += 1;
            pass_through_check(&dirty, &hurt, flt.events()).changed_outside_windows
        };
        assert_eq!(poke(window_end), 0, "first tail sample read as damage");
        assert_eq!(poke(gain_end - 1), 0, "last tail sample read as damage");
        assert_eq!(poke(gain_end), 1, "a change past the tail went unreported");
    }

    #[test]
    fn saturated_click_measures_at_full_scale() {
        let cfg = Config::builder()
            .on_classes(&[ClickClass::Short, ClickClass::Long, ClickClass::Xlong])
            .build();
        let x = plateau_signal(2000, 300, 8 * FRAME);
        let (out, flt) = run_filter(&cfg, &x);
        let checks = pass_through_check(&x, &out, flt.events());
        assert_eq!(checks.median_peak_before, 1.0);
        assert!(checks.median_peak_after < 0.01);
    }

    #[test]
    fn class_profile_separates_the_classes() {
        // One long click and one short click, both followed by the slow
        // excursion F13 measured, plus an uncorrected xlong event.
        let mut x = plateau_then_ringdown(12 * FRAME, 2000, 150);
        x[5000..5067].fill(-32768);
        x[7000..7300].fill(-32768);
        let cfg = Config::builder()
            .policy_override(ClickClass::Long, Policy::Descend)
            .tail_ms(ClickClass::Long, 15.0)
            .build();
        let (out, flt) = run_filter(&cfg, &x);
        assert_eq!(flt.metrics().corrected, 2);
        let profile = class_profile(&x, &out, flt.events());
        let names: Vec<&str> = profile.iter().map(|row| row.class.as_str()).collect();
        assert_eq!(names, vec!["long", "short", "xlong"]);
        let long = row(&profile, ClickClass::Long);
        let short = row(&profile, ClickClass::Short);
        let xlong = row(&profile, ClickClass::Xlong);
        assert_eq!(
            (long.policy.as_str(), short.policy.as_str()),
            ("descend", "interp")
        );
        assert_eq!((long.tail_samples, short.tail_samples), (720, 0));
        for row in [&long, &short] {
            assert_eq!(row.events, 1);
            assert!(row.click_after < row.click_before);
            assert!(row.window_after < row.window_before);
        }
        // A class left uncorrected still reports the level it left in the audio.
        assert_eq!(
            (xlong.events, xlong.corrected, xlong.policy.as_str()),
            (1, 0, "-")
        );
        assert_eq!((xlong.click_before, xlong.click_after), (1.0, 1.0));
        assert!(xlong.resid_before.is_none());
        assert!(xlong.seam_step_max.is_none());
        // The guard band is inside the window but outside the plateau.
        assert!(long.guard_before < long.click_before);
        assert!(!class_lines(&profile).is_empty());
    }

    fn row(profile: &[ClassRow], class: ClickClass) -> ClassRow {
        profile
            .iter()
            .find(|row| row.class == class)
            .cloned()
            .expect("class present")
    }

    /// One saturated plateau followed by a slow excursion at `amp` (F13).
    fn plateau_then_ringdown(total: usize, onset: usize, run_len: usize) -> Vec<i16> {
        (0..total)
            .map(|i| {
                let ring = if i >= onset + run_len {
                    0.6 * (2.0 * std::f64::consts::PI * 60.0 * i as f64 / 48000.0).sin()
                } else {
                    0.0
                };
                if (onset..onset + run_len).contains(&i) {
                    -32768
                } else {
                    (ring * 32767.0) as i16
                }
            })
            .collect()
    }
}
