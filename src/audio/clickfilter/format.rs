//! Number formatting that reproduces the reference rig's text output.
//!
//! The byte comparison against `../ubc125-ml/scripts/clickfilter/` covers the corrected WAV,
//! the event CSV and the label track. Those are floats rendered by Python
//! (`str(round(x, 4))`, `csv.DictWriter`), so the port has to agree on the text
//! as well as the audio. Two rules do the work:
//!
//!   * Python's `repr` of a float is the shortest decimal that round-trips, and
//!     it always shows a fractional part (`1.0`, not `1`);
//!   * Python's `round` is correctly rounded half-to-even on the *exact* binary
//!     value, which is what Rust's precision-bearing `{:.*}` formatting does.
//!
//! So `round(x, k)` printed by Python equals `x` printed with `k` decimals and
//! trailing zeros trimmed to at least one fractional digit, for any value in the
//! magnitudes this rig reports (no exponent form, no infinities).

/// `str(round(value, places))` as Python would write it.
pub fn rounded(value: f64, places: usize) -> String {
    let text = format!("{:.*}", places, value);
    trim_to_one_fraction(&text)
}

/// `repr(value)` for a Python float: shortest round-trip, always with a
/// fractional part.
pub fn shortest(value: f64) -> String {
    let text = format!("{value}");
    if text.contains('.') || text.contains('e') || text.contains("NaN") {
        text
    } else if text.contains("inf") {
        // Python's json writes `Infinity`/`-Infinity`; nothing here produces them.
        text.replace("inf", "Infinity")
    } else {
        format!("{text}.0")
    }
}

fn trim_to_one_fraction(text: &str) -> String {
    let Some(dot) = text.find('.') else {
        return format!("{text}.0");
    };
    let trimmed = text.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        debug_assert!(trimmed.len() > dot);
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_matches_python_str_of_round() {
        // Each pair is (value, places, `str(round(value, places))` from CPython).
        let cases = [
            (1.0_f64, 4, "1.0"),
            (0.0, 4, "0.0"),
            (0.8718, 4, "0.8718"),
            (32732.0 / 32768.0, 4, "0.9989"),
            (0.03125, 4, "0.0312"),
            (0.00001, 4, "0.0"),
            (-0.00001, 4, "-0.0"),
            (-5.35, 1, "-5.3"),
            (-5.4, 1, "-5.4"),
            (-120.0, 1, "-120.0"),
            (20.5, 3, "20.5"),
        ];
        for (value, places, want) in cases {
            assert_eq!(rounded(value, places), want, "round({value}, {places})");
        }
    }

    #[test]
    fn shortest_keeps_a_fractional_part() {
        assert_eq!(shortest(1.0), "1.0");
        assert_eq!(shortest(-0.0), "-0.0");
        assert_eq!(shortest(0.98), "0.98");
        assert_eq!(shortest(20.500000000000004), "20.500000000000004");
    }
}
