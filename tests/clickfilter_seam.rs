//! Parent-specific seam tests for the de-clicker port (ML-PORT-PLAN.md).
//!
//! The ported net (`clickfilter_port_pitfalls.rs`, `clickfilter_parity.rs`)
//! pins the filter's arithmetic; these pin what the parent adds around it:
//! the T3 config of record, the fixed-960 in-place contract, the leading
//! silence, the ragged flush, `for_capture` hygiene, and the metrics that
//! must stay healthy on the filtered path.

use ubc125::audio::clickfilter::cli::config_tag;
use ubc125::audio::clickfilter::config::Config;
use ubc125::audio::clickfilter::constants::{ClickClass, FRAME, Policy};
use ubc125::audio::clickfilter::filter::run_filter;
use ubc125::audio::filter::PcmFrameFilter;
use ubc125::audio::InPlaceDeClick;

/// The T3 config of record (ML-PORT-PLAN.md, arm 3). Mirrors the wiring in
/// `src/cmd/serve.rs::audio_source`; the tag assertion below pins it, so a
/// change to either side is visible here.
fn record_config() -> Config {
    Config::builder()
        .policy(Policy::Interp)
        .policy_override(ClickClass::Long, Policy::Descend)
        .tail_ms(ClickClass::Long, 150.0)
        .build()
}

#[test]
fn record_config_has_the_t3_derived_values() {
    let cfg = record_config();
    // Fixed 20.5 ms output delay (max_plateau 400 + post 480 + pre 96 + pad 8).
    assert_eq!(cfg.delay(), 984);
    assert_eq!((cfg.pre(), cfg.post(), cfg.xfade()), (96, 480, 96));
    // long: 150 ms = 7200 samples of recovery tail; every other class 0.
    assert_eq!(cfg.tail_samples(ClickClass::Long), 7200);
    for class in [ClickClass::Short, ClickClass::Xlong, ClickClass::Other] {
        assert_eq!(cfg.tail_samples(class), 0, "unexpected tail on {class:?}");
    }
    assert_eq!(cfg.policy_for(ClickClass::Long), Policy::Descend);
    assert_eq!(cfg.policy_for(ClickClass::Short), Policy::Interp);
    assert_eq!(cfg.on_classes(), &[ClickClass::Short, ClickClass::Long]);
    // The artifact tag is the record's identity.
    assert_eq!(
        config_tag(&cfg, ""),
        "interp+long=descend_pre96_post480_xf96_tail0-150-0-0_on-short+long"
    );
}

/// A 200 Hz tone: loud enough to be audio, slow and smooth enough to never
/// trip the 0.98-plateau trigger.
fn tone(total: usize, hz: f64) -> Vec<i16> {
    (0..total)
        .map(|i| {
            (0.35 * 32767.0 * (2.0 * std::f64::consts::PI * hz * i as f64 / 48000.0).sin())
                as i16
        })
        .collect()
}

#[test]
fn leading_silence_through_the_parent_trait_object() {
    // The fixed 984-sample delay, seen through the exact trait the capture
    // pipeline holds: first frame pure silence, second 24 silence + the 936
    // samples the second frame releases (constant input passes through
    // unchanged — the no-plateau bit-exactness is pinned in the ported net).
    let cfg = record_config();
    let mut filter: Box<dyn PcmFrameFilter> = Box::new(InPlaceDeClick::new(&cfg));
    let mut first = vec![7i16; FRAME];
    filter.process_frame(&mut first);
    assert_eq!(first, vec![0i16; FRAME], "the first frame is pure silence");
    let mut second = vec![7i16; FRAME];
    filter.process_frame(&mut second);
    assert_eq!(&second[..24], vec![0i16; 24].as_slice(), "24 more silence");
    assert_eq!(&second[24..], vec![7i16; FRAME - 24].as_slice(), "then the input");
}

#[test]
fn flush_yields_a_full_frame_plus_the_ragged_remainder() {
    // Steady state: the delay line holds exactly 984 samples, so flush emits
    // [960, 24] — the 24-sample tail is what native.rs zero-pads before Opus.
    let cfg = record_config();
    let mut filter = InPlaceDeClick::new(&cfg);
    for _ in 0..8 {
        let mut frame = vec![0i16; FRAME];
        filter.process_frame(&mut frame);
    }
    let held = filter.flush();
    let sizes: Vec<usize> = held.iter().map(|chunk| chunk.len()).collect();
    assert_eq!(
        sizes,
        vec![FRAME, cfg.delay() as usize % FRAME],
        "984 held samples emit as a full frame plus the 24-sample ragged tail"
    );
    assert_eq!(
        held.iter().map(|chunk| chunk.len()).sum::<usize>(),
        cfg.delay() as usize
    );
    // Everything is silence: the input was.
    assert!(held.iter().flatten().all(|&s| s == 0));
    // A second flush holds nothing.
    assert!(filter.flush().is_empty());
    assert_eq!(filter.underruns(), 0);
}

#[test]
fn no_click_tone_passes_through_with_only_the_fixed_delay() {
    // Through the trait object the pipeline holds: a click-free stream is the
    // offline output shifted by exactly the 984-sample delay, bit for bit.
    let cfg = record_config();
    let total = 16 * FRAME;
    let input = tone(total, 200.0);
    let (offline, flt) = run_filter(&cfg, &input);
    assert!(
        flt.events().is_empty(),
        "a 200 Hz tone must not trigger the classifier"
    );
    let mut filter: Box<dyn PcmFrameFilter> = Box::new(InPlaceDeClick::new(&cfg));
    let mut live = Vec::with_capacity(total + cfg.delay() as usize);
    for frame in input.as_chunks::<FRAME>().0 {
        let mut buffer = frame.to_vec();
        filter.process_frame(&mut buffer);
        live.extend(buffer);
    }
    for chunk in filter.flush() {
        live.extend(chunk);
    }
    assert_eq!(live.len(), offline.len() + cfg.delay() as usize, "nothing lost or added");
    assert!(
        live[..cfg.delay() as usize].iter().all(|&s| s == 0),
        "the delay head is silence"
    );
    assert_eq!(&live[cfg.delay() as usize..], offline.as_slice(), "bit-exact after the delay");
}

#[test]
fn for_capture_starts_clean_through_the_trait_object() {
    // The capture pipeline builds one filter per generation via for_capture:
    // no state may leak from a previous generation into the fresh delay line.
    let cfg = record_config();
    let used = InPlaceDeClick::new(&cfg);
    let mut fresh: Box<dyn PcmFrameFilter> = used.for_capture();
    let mut probe = vec![-32768i16; FRAME];
    fresh.process_frame(&mut probe);
    // A fresh filter has seen one frame and owes 984 samples of silence: the
    // whole 960-sample output frame is still silence, none of the -32768 input
    // has been released yet.
    assert_eq!(probe, vec![0i16; FRAME], "first frame of a fresh capture is silence");
}

#[test]
fn metrics_stay_healthy_over_a_dirty_stream() {
    // Short and long clicks at the F2/F10 geometries: the correction must run
    // with no illegal late write and no underrun, and the stream length must
    // be conserved (input + delay head).
    let cfg = record_config();
    let mut input = tone(24 * FRAME, 200.0);
    input[2000..2067].fill(-32768); // short plateau
    input[4000..4150].fill(-32768); // long plateau
    let mut filter = InPlaceDeClick::new(&cfg);
    let mut live = Vec::new();
    for frame in input.as_chunks::<FRAME>().0 {
        let mut buffer = frame.to_vec();
        filter.process_frame(&mut buffer);
        live.extend(buffer);
    }
    for chunk in filter.flush() {
        live.extend(chunk);
    }
    assert_eq!(filter.metrics().late_writes, 0, "an illegal write");
    assert_eq!(filter.underruns(), 0, "the delay line went short");
    assert_eq!(
        live.len(),
        input.len() + cfg.delay() as usize,
        "every input sample emitted exactly once"
    );
    // Both plateaus were classified and corrected (events recorded, windows
    // bounded), so the output actually differs from the pass-through shift.
    let (offline, _) = run_filter(&cfg, &input);
    assert_eq!(offline.len(), input.len());
    assert_ne!(&live[cfg.delay() as usize..], &input[..input.len() - cfg.delay() as usize]);
}
