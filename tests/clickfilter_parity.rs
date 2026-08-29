//! Cross-language parity, pinned without the corpus.
//!
//! `../ubc125-ml/scripts/port-fixtures.py` records the reference rig's own output for a
//! synthetic two-plateau carrier, for the config of record (T3 arm 3) and for the
//! baseline. This test runs the port over the same bytes and compares sample for
//! sample, so a fresh clone can check the claim `../ubc125-ml/docs/prototype.md` makes for the
//! port without `UBC125_ML_DATA` and without Python. The full-corpus comparison
//! is `../ubc125-ml/scripts/port-compare.sh`, and it is the acceptance evidence; this is the
//! regression net that catches a ported arithmetic change the moment it lands.

use ubc125::audio::clickfilter::checks::pass_through_check;
use ubc125::audio::clickfilter::config::Config;
use ubc125::audio::clickfilter::constants::{ClickClass, Policy};
use ubc125::audio::clickfilter::filter::{Decision, run_filter};

const FIXTURE: &str = include_str!("fixtures/python-reference.txt");

/// The rig's sections: hex little-endian i16 streams and `onset:end:class:decision`
/// event lists.
fn section(name: &str) -> String {
    let marker = format!("[{name}]\n");
    let start = FIXTURE.find(&marker).unwrap_or_else(|| {
        panic!("fixture has no [{name}] section; re-run ../ubc125-ml/scripts/port-fixtures.py")
    });
    let rest = &FIXTURE[start + marker.len()..];
    let end = rest.find('\n').unwrap_or(rest.len());
    rest[..end].to_string()
}

fn samples(hex: &str) -> Vec<i16> {
    assert!(
        hex.len().is_multiple_of(4),
        "hex stream must be whole i16 samples"
    );
    (0..hex.len())
        .step_by(4)
        .map(|i| {
            let low = u8::from_str_radix(&hex[i..i + 2], 16).expect("low byte");
            let high = u8::from_str_radix(&hex[i + 2..i + 4], 16).expect("high byte");
            // The rig writes the samples as little-endian bytes, so the first pair
            // is the low byte.
            i16::from_le_bytes([low, high])
        })
        .collect()
}

fn configs() -> Vec<(&'static str, Config)> {
    vec![
        ("baseline", Config::default()),
        (
            "arm3",
            Config::builder()
                .policy(Policy::Interp)
                .policy_override(ClickClass::Long, Policy::Descend)
                .tail_ms(ClickClass::Long, 150.0)
                .build(),
        ),
    ]
}

#[test]
fn the_port_reproduces_the_reference_stream_sample_for_sample() {
    let input = samples(&section("input"));
    assert_eq!(input.len(), 6 * 960, "fixture input length changed");
    for (name, cfg) in configs() {
        let (out, flt) = run_filter(&cfg, &input);
        let want = samples(&section(&format!("{name}.out")));
        assert_eq!(flt.metrics().late_writes, 0, "{name}: an illegal write");
        if out != want {
            let first = out
                .iter()
                .zip(want.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(out.len());
            panic!(
                "{name}: first difference at sample {first}: port {:?}, rig {:?}",
                &out[first..(first + 4).min(out.len())],
                &want[first..(first + 4).min(want.len())]
            );
        }
        assert_eq!(out.len(), want.len(), "{name}: length changed");
        assert_eq!(
            pass_through_check(&input, &out, flt.events()).changed_outside_windows,
            0,
            "{name}: changed outside a window"
        );
    }
}

#[test]
fn the_port_records_the_same_events_as_the_reference() {
    let input = samples(&section("input"));
    for (name, cfg) in configs() {
        let (_, flt) = run_filter(&cfg, &input);
        let want = section(&format!("{name}.events"));
        let got: Vec<String> = flt
            .events()
            .iter()
            .map(|event| {
                format!(
                    "{}:{}:{}:{}",
                    event.onset,
                    event.end,
                    event.class,
                    match event.decision {
                        Decision::Correct => "correct",
                        Decision::PassThrough => "pass-through",
                        Decision::TooLate => "too-late",
                    }
                )
            })
            .collect();
        assert_eq!(got.join(","), want, "{name}: the event list changed");
    }
}
