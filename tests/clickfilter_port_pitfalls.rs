//! The port's test net: the pitfalls the Python suite pins, re-pinned in Rust
//! (`../ubc125-ml/docs/prototype.md`, "Test net and its verification").
//!
//! These are the behaviours that separate a working port from one that merely
//! sounds right: the emission schedule, the rounding and saturation rules, the
//! first-correction-wins overlap policy, the cap split at frame edges, the legal
//! delay floor measured sharp at one sample less, and the tail ramp's continuity
//! across the ring/plan join. All signals are synthetic — the corpus is not a
//! test dependency (`../ubc125-ml/docs/development.md`).

use ubc125::audio::clickfilter::checks::pass_through_check;
use ubc125::audio::clickfilter::config::Config;
use ubc125::audio::clickfilter::constants::{ClickClass, FRAME, Policy};
use ubc125::audio::clickfilter::filter::{ClickFilter, Decision, run_filter};

const PRE: i64 = 96;
const POST: i64 = 480;
const ONSET: usize = 2000;
const RUN_LEN: usize = 67;

/// Zeros with one full-scale plateau — the trigger's designed-in signal.
fn plateau_signal(onset: usize, run_len: usize, total: usize) -> Vec<i16> {
    let mut x = vec![0i16; total];
    x[onset..onset + run_len].fill(-32768);
    x
}

/// A 20 Hz carrier with one plateau stamped in: slow enough that a fill which
/// restores the hidden waveform can be told from one that does not.
fn carrier(total: usize, onset: usize, run_len: usize) -> Vec<i16> {
    (0..total)
        .map(|i| {
            let clean =
                (0.35 * 32767.0 * (2.0 * std::f64::consts::PI * 20.0 * i as f64 / 48000.0).sin())
                    as i16;
            if (onset..onset + run_len).contains(&i) {
                -32768
            } else {
                clean
            }
        })
        .collect()
}

/// A plateau followed by a slow excursion at 0.6 FS: the `off`-class geometry
/// F13 measured, where the audio is still loud tens of ms past the plateau.
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

fn selected_config() -> Config {
    Config::builder()
        .policy(Policy::Interp)
        .policy_override(ClickClass::Long, Policy::Descend)
        .tail_ms(ClickClass::Long, 150.0)
        .build()
}

#[test]
fn emission_schedule_is_zero_then_936_then_frames() {
    // Position p is final once input p + delay has been ingested, so 8 frames
    // emit 0, 936, 960, … and flush drains exactly the delay line. A lockstep
    // implementation or an internal output queue fails this immediately.
    let cfg = selected_config();
    assert_eq!(cfg.delay(), 984);
    let mut filter = ClickFilter::new(&cfg);
    let frame = vec![0i16; FRAME];
    let chunks: Vec<usize> = (0..8).map(|_| filter.process_frame(&frame).len()).collect();
    assert_eq!(chunks, vec![0, 936, 960, 960, 960, 960, 960, 960]);
    assert!(chunks.iter().all(|&c| c <= FRAME));
    assert_eq!(filter.flush().len(), cfg.delay() as usize);
    assert_eq!(
        chunks.iter().sum::<usize>() + cfg.delay() as usize,
        8 * FRAME
    );
    assert_eq!(filter.metrics().samples_out, filter.metrics().samples_in);
}

#[test]
fn flush_drains_the_delay_line_plus_a_ragged_tail() {
    // Offline runs end on a short frame, so flush is the only call that may emit
    // more than a frame: the delay line plus that tail, and nothing may go
    // missing.
    let cfg = Config::default();
    for tail in [1usize, 100, FRAME - 1] {
        let mut filter = ClickFilter::new(&cfg);
        let frame = vec![0i16; FRAME];
        let emitted: usize = (0..4).map(|_| filter.process_frame(&frame).len()).sum();
        let drained = filter.finish(&vec![0i16; tail]).len();
        assert_eq!(drained, cfg.delay() as usize + tail, "tail={tail}");
        assert_eq!(emitted + drained, 4 * FRAME + tail, "tail={tail}");
    }
}

#[test]
fn output_length_equals_input_length_for_arbitrary_lengths() {
    let cfg = Config::default();
    for n in [0usize, 1, 959, 960, 961, 1923] {
        let x: Vec<i16> = (0..n)
            .map(|i| {
                (0.49 * 32767.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0).sin())
                    as i16
            })
            .collect();
        let (out, flt) = run_filter(&cfg, &x);
        assert_eq!(out.len(), n, "n={n}: len {}", out.len());
        assert_eq!(flt.metrics().samples_in, n as i64);
        assert_eq!(flt.metrics().samples_out, n as i64);
    }
}

#[test]
fn a_finish_with_a_full_frame_is_refused() {
    // The production seam never sees a ragged frame, so `finish` takes at most
    // frame-1 samples; anything longer is a caller bug, not a second code path.
    let cfg = Config::default();
    let mut filter = ClickFilter::new(&cfg);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        filter.finish(&vec![0i16; FRAME])
    }));
    assert!(
        result.is_err(),
        "a full frame passed to finish must be refused"
    );
}

#[test]
fn no_plateau_is_bit_exact_pass_through() {
    let cfg = Config::default();
    let x: Vec<i16> = (0..5000)
        .map(|i| {
            (0.49 * 32767.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0).sin())
                as i16
        })
        .collect();
    let (out, flt) = run_filter(&cfg, &x);
    assert_eq!(out, x);
    assert_eq!(flt.metrics().candidates, 0);
    assert_eq!(flt.metrics().corrected, 0);
    assert_eq!(flt.metrics().late_writes, 0);
}

#[test]
fn late_write_accounting() {
    // No late writes at the derived delay; a deliberately too-small delay
    // produces a refused, counted write and leaves the output bit-identical.
    let cfg = Config::default();
    let x = plateau_signal(ONSET, RUN_LEN, 3 * FRAME);
    let (out, flt) = run_filter(&cfg, &x);
    assert_eq!(flt.metrics().corrected, 1);
    assert_eq!(flt.metrics().late_writes, 0);
    assert_eq!(flt.events()[0].decision, Decision::Correct);
    assert_eq!(out.len(), x.len());

    let small = cfg.with_delay(100);
    let y = plateau_signal(1400, RUN_LEN, 3 * FRAME);
    let (out, flt) = run_filter(&small, &y);
    assert_eq!(flt.metrics().late_writes, 1);
    assert_eq!(flt.metrics().corrected, 0);
    assert_eq!(flt.events()[0].decision, Decision::TooLate);
    assert_eq!(out, y);
}

#[test]
fn overlapping_windows_count_and_keep_the_first_correction() {
    // Two 67-sample plateaus 560 samples apart: W1 [1904, 2547) and
    // W2 [2464, 3107) overlap by 83 samples, and each window is corrected as if
    // the other plateau were absent.
    let cfg = Config::default();
    let mut x = plateau_signal(ONSET, RUN_LEN, 6 * FRAME);
    x[2560..2627].fill(-32768);
    let (out, flt) = run_filter(&cfg, &x);
    let m = flt.metrics();
    assert_eq!(
        (m.candidates, m.corrected, m.late_writes, m.overlaps),
        (2, 2, 0, 83)
    );
    assert!(flt.events().iter().all(|e| e.decision == Decision::Correct));

    let solo1 = run_filter(&cfg, &plateau_signal(ONSET, RUN_LEN, 6 * FRAME)).0;
    let solo2 = run_filter(&cfg, &plateau_signal(2560, RUN_LEN, 6 * FRAME)).0;
    assert_eq!(&out[1904..2547], &solo1[1904..2547]);
    assert_eq!(&out[2464..3107], &solo2[2464..3107]);
    for index in 0..x.len() {
        if !(1904..3107).contains(&index) {
            assert_eq!(
                out[index], x[index],
                "changed outside both windows at {index}"
            );
        }
    }
}

#[test]
fn flush_closes_a_run_open_at_end_of_input() {
    let cfg = Config::default();
    let n = 2 * FRAME;
    let x = plateau_signal(n - 67, 67, n); // the run ends exactly at EOF
    let (out, flt) = run_filter(&cfg, &x);
    assert_eq!(out.len(), n);
    assert_eq!(flt.metrics().corrected, 1);
    assert_eq!(flt.metrics().late_writes, 0);
    let event = &flt.events()[0];
    assert_eq!(event.decision, Decision::Correct);
    assert_eq!(event.window_start, Some(n as i64 - 67 - PRE));
    assert_eq!(event.window_end, Some(n as i64));
    assert_eq!(event.tail_samples, Some(0));
    assert_eq!(&out[..n - 67 - PRE as usize], &x[..n - 67 - PRE as usize]);
    assert_ne!(&out[n - 67 - PRE as usize..n], &x[n - 67 - PRE as usize..n]);
}

#[test]
fn a_capped_piece_passes_through_and_the_remainder_is_corrected() {
    let cfg = Config::default();
    let x = plateau_signal(100, 490, 6 * FRAME);
    let (_, flt) = run_filter(&cfg, &x);
    let m = flt.metrics();
    assert_eq!(
        (
            m.candidates,
            m.capped,
            m.corrected,
            m.skipped,
            m.late_writes,
            m.overlaps
        ),
        (2, 1, 1, 1, 0, 0)
    );
    let events: Vec<(i64, i64, &str, Decision)> = flt
        .events()
        .iter()
        .map(|e| (e.onset, e.end, e.class.as_str(), e.decision))
        .collect();
    assert_eq!(
        events,
        vec![
            (100, 500, "other", Decision::PassThrough),
            (500, 590, "short", Decision::Correct),
        ]
    );
}

#[test]
fn output_is_deterministic_for_every_policy() {
    let x = {
        let mut base = plateau_signal(1500, RUN_LEN, 6 * FRAME);
        base[3000..3067].fill(-32768);
        base
    };
    for policy in [Policy::Interp, Policy::Mute, Policy::LowBandNull] {
        let cfg = Config::builder().policy(policy).build();
        let a = run_filter(&cfg, &x).0;
        let b = run_filter(&cfg, &x).0;
        assert_eq!(a, b, "{policy}: a second run differed");
        // A second event must not disturb the first correction.
        let solo = run_filter(&cfg, &plateau_signal(1500, RUN_LEN, 6 * FRAME)).0;
        assert_eq!(
            &a[(1500 - PRE as usize)..(1567 + POST as usize)],
            &solo[(1500 - PRE as usize)..(1567 + POST as usize)],
            "{policy}: the second event altered the first"
        );
    }
}

#[test]
fn legal_delay_floor_is_run_plus_post_plus_pre() {
    // The floor is the longest *corrected* plateau plus post plus pre, not
    // max_plateau plus them, and it is sharp: one sample less is illegal. The
    // geometry puts `end + post` exactly on a frame boundary, which forces the
    // apply to wait a whole frame.
    for (run_len, floor) in [(67usize, 643i64), (152, 728), (169, 745)] {
        assert_eq!(run_len as i64 + POST + PRE, floor);
        let onset =
            ((-(run_len as i64 + POST)).rem_euclid(FRAME as i64) + 5 * FRAME as i64) as usize;
        let x = plateau_signal(onset, run_len, 20 * FRAME);
        for (delay, want_late) in [(floor, 0), (floor - 1, 1)] {
            let cfg = Config::default().with_delay(delay);
            let (out, flt) = run_filter(&cfg, &x);
            assert_eq!(
                flt.metrics().late_writes,
                want_late,
                "run={run_len} delay={delay}"
            );
            assert_eq!(flt.metrics().corrected, 1 - want_late);
            let checks = pass_through_check(&x, &out, flt.events());
            assert_eq!(checks.changed_outside_windows, 0);
            assert!(checks.length_match);
        }
    }
    // The shipped floor is conservative against the class-bound floor by 239.
    assert_eq!(Config::default().delay() - 745, 239);
}

#[test]
fn tight_delay_follows_the_classes_actually_corrected() {
    let cfg = Config::default();
    assert_eq!((cfg.max_correctable_run(), cfg.tight_delay()), (169, 745));
    let on = Config::builder()
        .on_classes(&[ClickClass::Short, ClickClass::Long, ClickClass::Xlong])
        .build();
    assert_eq!((on.max_correctable_run(), on.tight_delay()), (399, 975));
    // 745 and 975 stay legal at exactly one more, and the wider class costs no
    // emitted delay: max_plateau already pays for it.
    assert_eq!(on.delay(), 984);
    let none = Config::builder().on_classes(&[]).build();
    assert_eq!((none.max_correctable_run(), none.tight_delay()), (0, 0));
}

#[test]
fn xlong_tight_floor_is_sharp_at_one_less() {
    let run_len = 399usize;
    let onset = ((-(run_len as i64 + POST)).rem_euclid(FRAME as i64) + 5 * FRAME as i64) as usize;
    let x = plateau_signal(onset, run_len, 20 * FRAME);
    let classes = [ClickClass::Short, ClickClass::Long, ClickClass::Xlong];
    for (delay, want_late) in [(975i64, 0), (974, 1)] {
        let cfg = Config::builder()
            .on_classes(&classes)
            .build()
            .with_delay(delay);
        let (_, flt) = run_filter(&cfg, &x);
        assert_eq!(flt.events()[0].class, ClickClass::Xlong);
        assert_eq!(flt.metrics().late_writes, want_late, "delay={delay}");
        assert_eq!(flt.metrics().corrected, 1 - want_late);
    }
}

#[test]
fn tail_ramp_joins_across_ring_and_gain_plan() {
    // A 20 ms tail is 960 samples and the split falls 333 in, inside the ramp:
    // two independent ramps would step by ~1.0 there.
    let cfg = Config::builder().tail_ms(ClickClass::Short, 20.0).build();
    assert_eq!(cfg.tail_samples(ClickClass::Short), 960);
    let x = {
        let mut base = vec![16000i16; 8 * FRAME];
        base[2000..2067].fill(-32768);
        base
    };
    let (out, flt) = run_filter(&cfg, &x);
    assert_eq!(flt.metrics().corrected, 1);
    let event = &flt.events()[0];
    let stop = event.window_end.unwrap() as usize;
    let gain_end = event.gain_end.unwrap() as usize;
    assert_eq!(gain_end - stop, 960);
    let gains: Vec<f64> = out[stop..gain_end]
        .iter()
        .map(|s| f64::from(*s) / 16000.0)
        .collect();
    let max_step = gains
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f64, f64::max);
    assert!(
        max_step < 0.01,
        "discontinuous tail: max step {max_step:.4}"
    );
    assert!(gains[0] < 0.05);
    assert!(gains[gains.len() - 1] > 0.95);
    assert_eq!(&out[gain_end..], &x[gain_end..]);
    assert_eq!(Config::default().delay(), 984);
}

#[test]
fn a_tail_costs_no_latency() {
    for tail_ms in [0.0, 50.0, 100.0, 150.0] {
        let cfg = Config::builder()
            .tail_ms(ClickClass::Short, tail_ms)
            .tail_ms(ClickClass::Long, tail_ms)
            .tail_ms(ClickClass::Other, tail_ms)
            .build();
        assert_eq!(
            cfg.delay(),
            984,
            "a tail must not cost latency: {tail_ms} ms"
        );
    }
}

#[test]
fn a_per_class_override_changes_exactly_one_class() {
    let cfg = Config::builder()
        .policy(Policy::Interp)
        .policy_override(ClickClass::Long, Policy::Descend)
        .tail_ms(ClickClass::Long, 10.0)
        .build();
    assert_eq!(
        (
            cfg.policy_for(ClickClass::Short),
            cfg.policy_for(ClickClass::Long),
            cfg.policy_for(ClickClass::Xlong)
        ),
        (Policy::Interp, Policy::Descend, Policy::Interp)
    );
    assert_eq!(cfg.policies_used(), vec![Policy::Interp, Policy::Descend]);

    let fixtures = [
        (
            "short",
            plateau_signal(ONSET, RUN_LEN, 6 * FRAME),
            Policy::Interp,
            0i64,
        ),
        (
            "long",
            plateau_then_ringdown(6 * FRAME, ONSET, 150),
            Policy::Descend,
            POST,
        ),
    ];
    for (class, x, want_policy, want_tail) in fixtures {
        let (out, flt) = run_filter(&cfg, &x);
        assert_eq!(flt.events().len(), 1, "{class}");
        let event = &flt.events()[0];
        assert_eq!(event.class.as_str(), class);
        assert_eq!(event.decision, Decision::Correct);
        assert_eq!(event.policy, Some(want_policy));
        assert_eq!(event.tail_samples, Some(want_tail), "{class}");
        assert_eq!(
            pass_through_check(&x, &out, flt.events()).changed_outside_windows,
            0
        );
        let plain = run_filter(&Config::default(), &x).0;
        if class == "short" {
            assert_eq!(out, plain, "an override must leave the other classes alone");
        } else {
            assert_ne!(out, plain, "the overridden class must actually change");
        }
    }
}

#[test]
fn the_policies_differ_on_a_carrier_and_agree_on_silence() {
    let total = 6 * FRAME;
    let slow_dirty = carrier(total, ONSET, RUN_LEN);
    let slow_clean: Vec<i16> = carrier(total, 0, 0);
    let outs: Vec<(Policy, Vec<i16>)> = [Policy::Interp, Policy::Mute, Policy::LowBandNull]
        .into_iter()
        .map(|policy| {
            let cfg = Config::builder().policy(policy).build();
            let (out, flt) = run_filter(&cfg, &slow_dirty);
            assert_eq!(flt.metrics().corrected, 1, "{policy}");
            assert_eq!(flt.metrics().late_writes, 0, "{policy}");
            (policy, out)
        })
        .collect();
    for (a, out_a) in &outs {
        for (b, out_b) in &outs {
            if a != b {
                assert_ne!(out_a, out_b, "{a} and {b} produced identical audio");
            }
        }
    }
    // On silence interp and mute both restore zeros; lf-null does not.
    let quiet = plateau_signal(ONSET, RUN_LEN, total);
    let on_quiet: Vec<(Policy, Vec<i16>)> = outs
        .iter()
        .map(|(policy, _)| {
            let cfg = Config::builder().policy(*policy).build();
            (*policy, run_filter(&cfg, &quiet).0)
        })
        .collect();
    let interp = on_quiet.iter().find(|(p, _)| *p == Policy::Interp).unwrap();
    let mute = on_quiet.iter().find(|(p, _)| *p == Policy::Mute).unwrap();
    let lf = on_quiet
        .iter()
        .find(|(p, _)| *p == Policy::LowBandNull)
        .unwrap();
    assert_eq!(interp.1, mute.1, "interp and mute must agree on silence");
    assert_ne!(interp.1, lf.1, "lf-null does not restore silence");
    let ws = ONSET as i64 - PRE;
    let we = ONSET as i64 + RUN_LEN as i64 + POST;
    for (_, out) in &on_quiet {
        assert_eq!(&out[..ws as usize], &quiet[..ws as usize]);
        assert_eq!(&out[we as usize..], &quiet[we as usize..]);
    }
    // interp restores the slow carrier: nearly a straight line inside the window.
    // Measured on the carrier run, not the silence run above.
    let interp_out = &outs.iter().find(|(p, _)| *p == Policy::Interp).unwrap().1;
    let mute_out = &outs.iter().find(|(p, _)| *p == Policy::Mute).unwrap().1;
    let seg = &interp_out[ONSET..ONSET + RUN_LEN];
    let clean = &slow_clean[ONSET..ONSET + RUN_LEN];
    let rmse = rms_against(seg, clean);
    let mute_rmse = rms_against(&mute_out[ONSET..ONSET + RUN_LEN], clean);
    assert!(
        rmse < 0.5 * mute_rmse,
        "interp {rmse:.4} is not clearly better than mute {mute_rmse:.4}"
    );
}

/// RMSE between two i16 segments, in full-scale units.
fn rms_against(got: &[i16], clean: &[i16]) -> f64 {
    got.iter()
        .zip(clean.iter())
        .map(|(a, b)| {
            let d = (f64::from(*a) - f64::from(*b)) / 32768.0;
            d * d
        })
        .sum::<f64>()
        .sqrt()
}

#[test]
fn a_fill_that_ends_at_zero_hands_over_to_the_ramp() {
    // interp hands over at click level, so it keeps its crossfade — and its seam
    // step is measured at roughly the click's own level. mute and descend reach
    // zero, so they drop the right-edge crossfade and do not step.
    let x = plateau_then_ringdown(12 * FRAME, ONSET, 150);
    let mut steps = Vec::new();
    for policy in [Policy::Interp, Policy::Mute, Policy::Descend] {
        let cfg = Config::builder()
            .policy(policy)
            .tail_ms(ClickClass::Short, 15.0)
            .tail_ms(ClickClass::Long, 15.0)
            .build();
        let (out, flt) = run_filter(&cfg, &x);
        assert_eq!(flt.metrics().corrected, 1, "{policy}");
        let event = &flt.events()[0];
        let at = event.window_end.unwrap() as usize;
        let step = (i64::from(out[at]) - i64::from(out[at - 1])).abs() as f64 / 32768.0;
        steps.push((policy, event.right_edge_ramp.unwrap(), step));
    }
    let original_step = {
        let at = ONSET + 150 + POST as usize;
        (i64::from(x[at]) - i64::from(x[at - 1])).abs() as f64 / 32768.0
    };
    assert!(
        original_step < 0.01,
        "fixture: the click must be smooth across the seam"
    );
    for (policy, right_edge, step) in &steps {
        match policy {
            Policy::Interp => {
                assert!(*right_edge, "interp must keep its right-edge crossfade");
                assert!(
                    *step > 0.3,
                    "the seam step the report exists to catch: {step}"
                );
            }
            _ => {
                assert!(!*right_edge, "{policy} must hand over to the ramp");
                assert!(*step < 0.02, "{policy} still steps at the seam: {step}");
            }
        }
    }
}

#[test]
fn descend_without_a_tail_is_refused() {
    // The bug is a configuration, so the refusal is tested as a configuration.
    let bare = Config::builder().policy(Policy::Descend).try_build();
    assert!(bare.is_err(), "descend with no tail must be refused");
    let overridden = Config::builder()
        .policy(Policy::Interp)
        .policy_override(ClickClass::Long, Policy::Descend)
        .try_build();
    assert!(
        overridden.is_err(),
        "an overridden descend needs a tail too"
    );
    assert!(
        overridden
            .err()
            .unwrap()
            .to_string()
            .contains("needs a recovery tail")
    );
    let ok = Config::builder()
        .policy(Policy::Interp)
        .policy_override(ClickClass::Long, Policy::Descend)
        .tail_ms(ClickClass::Long, 150.0)
        .try_build()
        .expect("descend with a tail is legal");
    assert_eq!(ok.tail_samples(ClickClass::Long), 7200);
    assert_eq!(
        ok.tail_samples(ClickClass::Short),
        0,
        "only that class needs one"
    );
    // A class that is never corrected cannot refuse a tail it will not use.
    let off = Config::builder()
        .policy(Policy::Interp)
        .policy_override(ClickClass::Xlong, Policy::Descend)
        .try_build();
    assert!(
        off.is_ok(),
        "xlong is off, so its descend fill is never built"
    );
}

#[test]
fn max_plateau_below_one_is_rejected() {
    for bad in [0i64, -5] {
        let err = Config::builder().max_plateau(bad).try_build();
        assert!(err.is_err(), "max_plateau={bad} must be refused");
    }
}

#[test]
fn every_delay_number_the_docs_quote() {
    let cfg = Config::default();
    assert_eq!((cfg.min_delay(), cfg.delay()), (984, 984));
    assert!((cfg.delay_ms() - 20.5).abs() < 1e-6);
    assert_eq!(Config::builder().policy(Policy::Mute).build().delay(), 984);
    assert_eq!(
        Config::builder()
            .policy(Policy::LowBandNull)
            .build()
            .delay(),
        1264
    );
    assert!(
        (Config::builder()
            .policy(Policy::LowBandNull)
            .build()
            .delay_ms()
            - 26.333)
            .abs()
            < 2e-3
    );
    assert_eq!(
        Config::builder().delay_ms(1.0).build().delay(),
        984,
        "clamped up"
    );
    assert_eq!(
        Config::builder().delay_ms(50.0).build().delay(),
        2400,
        "honoured"
    );
    assert_eq!(Config::builder().pre_ms(4.0).build().delay(), 984 + 96);
    assert_eq!(Config::builder().post_ms(20.0).build().delay(), 984 + 480);
    assert_eq!(
        Config::builder().max_plateau(800).build().delay(),
        984 + 400
    );
    let arm3 = selected_config();
    assert_eq!(arm3.delay(), 984);
    assert_eq!(arm3.pre(), 96);
    assert_eq!(arm3.post(), 480);
    assert_eq!(arm3.xfade(), 96);
    assert_eq!(arm3.tail_samples(ClickClass::Long), 7200);
    assert_eq!(arm3.context_pad(), 8);
    assert_eq!(arm3.max_correctable_run(), 169);
}
