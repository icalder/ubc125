use std::fmt;
use std::str::FromStr;

use crate::constants::*;

/// Frequency stored as the scanner's 8-digit raw string (e.g. "01239750").
///
/// Displays as `MHz.KHz` with leading zeros stripped from MHz but KHz
/// preserved as-is (matching original scanner formatting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frequency(u32);

impl Frequency {
    /// Create from the numeric value of the 8-digit raw string.
    ///
    /// E.g. `01239750` → stored as `1_239_750`, displayed as `123.9750`
    #[allow(dead_code)]
    pub fn from_raw(raw: u32) -> Self {
        debug_assert!(raw < 100_000_000, "raw frequency must be < 100000000");
        Self(raw)
    }

    /// Return true if this represents an empty/invalid frequency.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Convert user-entered frequency string to the scanner's 8-digit raw
    /// format, then wrap as a `Frequency`.
    ///
    /// Accepts several input styles:
    /// - `"123.9750"` — MHz.KHz form (pads/truncates each part to 4 digits)
    /// - `"88.1"`     — KHz right-padded to 4 digits
    /// - `"01239750"` — already 8 digits (raw form)
    /// - `"1239750"`  — 7 digits (left-padded to 8)
    /// - `"123"`      — short MHz (treated as MHz with zero KHz)
    ///
    /// Returns `None` for non-numeric characters, multiple dots, or empty
    /// input.
    pub fn from_user_input(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Validate: only digits and at most one dot.
        if s.matches('.').count() > 1 {
            return None;
        }
        if !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }

        let raw = if s.contains('.') {
            let parts: Vec<&str> = s.split('.').collect();
            let mut mhz = parts[0].to_string();
            let mut khz = parts.get(1).map(|v| v.to_string()).unwrap_or_default();

            // Reject input like "." where both parts are empty.
            if mhz.is_empty() && khz.is_empty() {
                return None;
            }

            while mhz.len() < MHZ_DIGITS {
                mhz.insert(0, '0');
            }
            if mhz.len() > MHZ_DIGITS {
                mhz.truncate(MHZ_DIGITS);
            }
            while khz.len() < KHZ_DIGITS {
                khz.push('0');
            }
            if khz.len() > KHZ_DIGITS {
                khz.truncate(KHZ_DIGITS);
            }
            format!("{}{}", mhz, khz)
        } else if s.len() >= 7 {
            let mut f = s.to_string();
            while f.len() < FREQUENCY_DIGITS {
                f.insert(0, '0');
            }
            if f.len() > FREQUENCY_DIGITS {
                f.truncate(FREQUENCY_DIGITS);
            }
            f
        } else {
            let mut mhz = s.to_string();
            while mhz.len() < MHZ_DIGITS {
                mhz.insert(0, '0');
            }
            format!("{}0000", mhz)
        };

        let n = raw.parse::<u32>().ok()?;
        if n >= 100_000_000 {
            return None;
        }
        Some(Self(n))
    }

    /// Return the 8-digit zero-padded raw string (e.g. "01239750").
    pub fn to_raw(self) -> String {
        format!("{:08}", self.0)
    }
}

impl FromStr for Frequency {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != FREQUENCY_DIGITS || !s.chars().all(|c| c.is_ascii_digit()) {
            return Err(());
        }
        s.parse::<u32>().map(Self).map_err(|_| ())
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("");
        }
        let raw = format!("{:08}", self.0);
        let mhz = &raw[0..4].trim_start_matches('0');
        let mhz = if mhz.is_empty() { "0" } else { mhz };
        let khz = &raw[4..8];
        write!(f, "{}.{}", mhz, khz)
    }
}

/// Modulation type supported by the scanner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Modulation {
    #[default]
    Auto,
    Am,
    Fm,
    Nfm,
    /// Unknown value from scanner
    Other(String),
}

impl FromStr for Modulation {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_uppercase().as_str() {
            "AUTO" => Ok(Self::Auto),
            "AM" => Ok(Self::Am),
            "FM" => Ok(Self::Fm),
            "NFM" => Ok(Self::Nfm),
            other if !other.is_empty() => Ok(Self::Other(other.to_string())),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Modulation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("Auto"),
            Self::Am => f.write_str("AM"),
            Self::Fm => f.write_str("FM"),
            Self::Nfm => f.write_str("NFM"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// Validated channel index (1–500).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChannelIndex(u32);

impl ChannelIndex {
    /// Create a channel index, validating the range 1–500.
    pub fn new(idx: u32) -> Option<Self> {
        if (1..=MAX_CHANNELS).contains(&idx) {
            Some(Self(idx))
        } else {
            None
        }
    }

    /// Return the inner value.
    pub fn get(&self) -> u32 {
        self.0
    }

    /// Derive the bank number (1–10) from a channel index.
    pub fn bank(&self) -> u32 {
        ((self.0 - 1) / CHANNELS_PER_BANK) + 1
    }
}

impl fmt::Display for ChannelIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Bank enable/disable mask (10 banks, index 0 = Bank 1).
///
/// `true` means the bank is enabled for scanning; `false` means locked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankMask([bool; NUM_BANKS]);

impl BankMask {
    /// Default: all banks enabled.
    pub fn new() -> Self {
        Self([true; NUM_BANKS])
    }

    /// Parse from an SCG response string (e.g. "SCG,0101010101").
    /// Strips the "SCG," prefix if present.
    /// `0` = enabled (valid), `1` = disabled (locked out).
    pub fn from_scanner_response(s: &str) -> Self {
        let mut banks = [true; NUM_BANKS];
        let mask = s.trim().strip_prefix("SCG,").unwrap_or(s.trim());
        if mask.len() >= NUM_BANKS {
            for (i, c) in mask.chars().take(NUM_BANKS).enumerate() {
                banks[i] = c == '0';
            }
        }
        Self(banks)
    }

    /// Format as an SCG command string (e.g. "SCG,0101010101").
    pub fn to_scanner_command(&self) -> String {
        let mut s = String::from("SCG,");
        for &b in &self.0 {
            s.push(if b { '0' } else { '1' });
        }
        s
    }

    /// Check if a bank (1-indexed) is enabled.
    ///
    /// Returns `false` for invalid bank numbers (0, 11+, etc.) instead
    /// of panicking.
    #[allow(dead_code)]
    pub fn is_enabled(&self, bank: u32) -> bool {
        if bank == 0 {
            return false;
        }
        let idx = (bank - 1) as usize;
        idx < NUM_BANKS && self.0[idx]
    }

    /// Toggle a bank (1-indexed).
    ///
    /// Does nothing for invalid bank numbers (0, 11+, etc.) instead
    /// of panicking.
    pub fn toggle(&mut self, bank: u32) {
        if bank == 0 {
            return;
        }
        let idx = (bank - 1) as usize;
        if idx < NUM_BANKS {
            self.0[idx] = !self.0[idx];
        }
    }

    /// Iterate over bank enabled states (index 0 = Bank 1).
    pub fn iter(&self) -> impl Iterator<Item = (usize, bool)> {
        self.0.iter().enumerate().map(|(i, &b)| (i, b))
    }
}

impl Default for BankMask {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed response from the GLG (scan status) command.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct ScanStatus {
    /// Parsed frequency
    pub frequency: Frequency,
    /// Modulation type
    pub modulation: Modulation,
    /// True when signal is detected (squelch open)
    pub signal_detected: bool,
    /// Channel name
    pub channel_name: String,
    /// Bank number (1–10), or `None` if not available.
    pub bank: Option<u32>,
    /// Channel index (1–500), or `None` if not available.
    pub channel_index: Option<ChannelIndex>,
    /// Raw scanner response for debugging
    pub raw: String,
}

impl ScanStatus {
    /// Parse a GLG response string.
    ///
    /// Format: `GLG,[Freq],[Modulation],,[Signal Status],,,[Channel Name],[Squelch State],[Mute State],,[Channel Index],`
    ///
    /// Returns `None` if the response is malformed, the frequency is
    /// invalid, or required fields are missing.
    pub fn parse_glg(response: &str) -> Option<Self> {
        let parts: Vec<&str> = response.split(',').collect();
        if parts.len() < GLG_MIN_FIELDS || parts[0] != "GLG" {
            return None;
        }

        // Frequency: must be a valid 8-digit string.
        let frequency: Frequency = parts[GLG_FREQ_IDX].parse().ok()?;

        // Modulation: fall back to Other if parse fails.
        let modulation = parts[GLG_MOD_IDX]
            .parse::<Modulation>()
            .unwrap_or_else(|_| Modulation::Other(parts[GLG_MOD_IDX].to_string()));

        let signal_detected = parts
            .get(GLG_SQUELCH_STATE_IDX)
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        let channel_name = parts
            .get(GLG_CHANNEL_NAME_IDX)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        // Channel index: required field. Reject the response if it is
        // missing or out of range so we don't overwrite valid data
        // with blank/default values.
        let channel_index = parts
            .get(GLG_CHANNEL_INDEX_IDX)
            .and_then(|s| s.trim().parse::<u32>().ok())
            .and_then(ChannelIndex::new)?;

        let bank = Some(channel_index.bank());

        Some(Self {
            frequency,
            modulation,
            signal_detected,
            channel_name,
            bank,
            channel_index: Some(channel_index),
            raw: response.to_string(),
        })
    }

    /// Display helper for the bank number.
    pub fn bank_display(&self) -> String {
        self.bank.map(|b| b.to_string()).unwrap_or_else(|| "-".to_string())
    }
}

/// A channel entry parsed from a CIN response.
#[derive(Clone, Debug)]
pub struct Channel {
    /// Channel index (1–500)
    pub index: ChannelIndex,
    /// Channel name
    pub name: String,
    /// Frequency
    pub frequency: Frequency,
    /// Modulation type
    pub modulation: Modulation,
}

impl Channel {
    /// Parse a CIN response string.
    ///
    /// Format: `CIN,[INDEX],[NAME],[FRQ],[MOD],...`
    ///
    /// Returns `None` if the response is not a valid CIN line.
    pub fn parse_cin(response: &str) -> Option<Self> {
        let parts: Vec<&str> = response.split(',').collect();
        if parts.len() < CIN_MIN_FIELDS || parts[0] != "CIN" {
            return None;
        }

        let idx = parts[CIN_INDEX_IDX].parse::<u32>().ok()?;
        let channel_index = ChannelIndex::new(idx)?;

        let name = parts[CIN_NAME_IDX].to_string();

        let frequency: Frequency = parts[CIN_FREQ_IDX].parse().ok()?;

        let modulation = parts[CIN_MOD_IDX]
            .parse::<Modulation>()
            .unwrap_or(Modulation::Other(parts[CIN_MOD_IDX].to_string()));

        Some(Self {
            index: channel_index,
            name,
            frequency,
            modulation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Frequency ----

    #[test]
    fn frequency_from_str_valid() {
        let f = "01239750".parse::<Frequency>().unwrap();
        assert!(!f.is_empty());
        // Stored as numeric value of the 8-digit string (leading zero lost)
        assert_eq!(f.0, 1_239_750);
    }

    #[test]
    fn frequency_display_standard() {
        let f = "01239750".parse::<Frequency>().unwrap();
        assert_eq!(f.to_string(), "123.9750");
    }

    #[test]
    fn frequency_display_low_mhz() {
        let f = "00881000".parse::<Frequency>().unwrap();
        assert_eq!(f.to_string(), "88.1000");
    }

    #[test]
    fn frequency_display_zero_khz() {
        // 01500000 -> 150.0000
        let f = "01500000".parse::<Frequency>().unwrap();
        assert_eq!(f.to_string(), "150.0000");
    }

    #[test]
    fn frequency_display_empty() {
        let f = "00000000".parse::<Frequency>().unwrap();
        assert!(f.is_empty());
        assert_eq!(f.to_string(), "");
    }

    #[test]
    fn frequency_from_str_invalid_length() {
        assert!("1239750".parse::<Frequency>().is_err());
    }

    #[test]
    fn frequency_to_raw() {
        let f = "01239750".parse::<Frequency>().unwrap();
        assert_eq!(f.to_raw(), "01239750");

        let f = "00881000".parse::<Frequency>().unwrap();
        assert_eq!(f.to_raw(), "00881000");

        let f = "00000000".parse::<Frequency>().unwrap();
        assert_eq!(f.to_raw(), "00000000");
    }

    // ---- Frequency::from_user_input ----

    #[test]
    fn frequency_from_user_input_with_dot() {
        let f = Frequency::from_user_input("123.9750").unwrap();
        assert_eq!(f.to_raw(), "01239750");
        let f = Frequency::from_user_input("88.1").unwrap();
        assert_eq!(f.to_raw(), "00881000");
        let f = Frequency::from_user_input("0.1").unwrap();
        assert_eq!(f.to_raw(), "00001000");
    }

    #[test]
    fn frequency_from_user_input_raw_format() {
        let f = Frequency::from_user_input("01239750").unwrap();
        assert_eq!(f.to_raw(), "01239750");
        let f = Frequency::from_user_input("1239750").unwrap();
        assert_eq!(f.to_raw(), "01239750");
    }

    #[test]
    fn frequency_from_user_input_short_mhz() {
        let f = Frequency::from_user_input("123").unwrap();
        assert_eq!(f.to_raw(), "01230000");
    }

    #[test]
    fn frequency_from_user_input_empty() {
        assert!(Frequency::from_user_input("").is_none());
    }

    #[test]
    fn frequency_from_user_input_invalid() {
        // Non-numeric characters
        assert!(Frequency::from_user_input("abc").is_none());
        // Multiple dots
        assert!(Frequency::from_user_input("12.34.56").is_none());
        // Letters mixed with digits
        assert!(Frequency::from_user_input("12.34a").is_none());
        assert!(Frequency::from_user_input("12a").is_none());
        // Only a dot
        assert!(Frequency::from_user_input(".").is_none());
    }

    // ---- Modulation ----

    #[test]
    fn modulation_from_str_known() {
        assert_eq!("AM".parse::<Modulation>().unwrap(), Modulation::Am);
        assert_eq!("FM".parse::<Modulation>().unwrap(), Modulation::Fm);
        assert_eq!("NFM".parse::<Modulation>().unwrap(), Modulation::Nfm);
        assert_eq!("Auto".parse::<Modulation>().unwrap(), Modulation::Auto);
    }

    #[test]
    fn modulation_display() {
        assert_eq!(Modulation::Am.to_string(), "AM");
        assert_eq!(Modulation::Fm.to_string(), "FM");
        assert_eq!(Modulation::Auto.to_string(), "Auto");
    }

    // ---- ChannelIndex ----

    #[test]
    fn channel_index_valid_range() {
        assert!(ChannelIndex::new(1).is_some());
        assert!(ChannelIndex::new(500).is_some());
        assert!(ChannelIndex::new(250).is_some());
    }

    #[test]
    fn channel_index_out_of_range() {
        assert!(ChannelIndex::new(0).is_none());
        assert!(ChannelIndex::new(501).is_none());
    }

    #[test]
    fn channel_index_bank_calculation() {
        assert_eq!(ChannelIndex::new(1).unwrap().bank(), 1);
        assert_eq!(ChannelIndex::new(50).unwrap().bank(), 1);
        assert_eq!(ChannelIndex::new(51).unwrap().bank(), 2);
        assert_eq!(ChannelIndex::new(500).unwrap().bank(), 10);
    }

    // ---- BankMask ----

    #[test]
    fn bank_mask_default_all_enabled() {
        let mask = BankMask::new();
        for i in 0..10 {
            assert!(mask.is_enabled((i + 1) as u32));
        }
    }

    #[test]
    fn bank_mask_from_scanner_response() {
        // 0=enabled, 1=disabled
        let mask = BankMask::from_scanner_response("0101010101");
        assert!(mask.is_enabled(1)); // '0'
        assert!(!mask.is_enabled(2)); // '1'
        assert!(mask.is_enabled(3));
        assert!(!mask.is_enabled(4));
    }

    #[test]
    fn bank_mask_from_scanner_response_with_prefix() {
        // Full scanner response includes "SCG," prefix
        let mask = BankMask::from_scanner_response("SCG,0101010101");
        assert!(mask.is_enabled(1)); // '0'
        assert!(!mask.is_enabled(2)); // '1'
        assert!(mask.is_enabled(3));
        assert!(!mask.is_enabled(4));
        assert!(mask.is_enabled(5));
        assert!(!mask.is_enabled(6));
    }

    #[test]
    fn bank_mask_to_scanner_command() {
        let mut mask = BankMask::new();
        // Toggle even banks (2, 4, 6, 8, 10) for alternating pattern
        mask.toggle(2);
        mask.toggle(4);
        mask.toggle(6);
        mask.toggle(8);
        mask.toggle(10);
        assert_eq!(mask.to_scanner_command(), "SCG,0101010101");
    }

    #[test]
    fn bank_mask_toggle() {
        let mut mask = BankMask::new();
        assert!(mask.is_enabled(1));
        mask.toggle(1);
        assert!(!mask.is_enabled(1));
        mask.toggle(1);
        assert!(mask.is_enabled(1));
    }

    #[test]
    fn bank_mask_invalid_bank_zero() {
        let mask = BankMask::new();
        assert!(!mask.is_enabled(0));
    }

    #[test]
    fn bank_mask_invalid_bank_eleven() {
        let mask = BankMask::new();
        assert!(!mask.is_enabled(11));
    }

    #[test]
    fn bank_mask_invalid_bank_max() {
        let mask = BankMask::new();
        assert!(!mask.is_enabled(u32::MAX));
    }

    #[test]
    fn bank_mask_toggle_invalid_does_not_panic() {
        let mut mask = BankMask::new();
        mask.toggle(0);
        mask.toggle(11);
        mask.toggle(u32::MAX);
        // No panic — all banks should remain unchanged.
        for i in 0..10 {
            assert!(mask.is_enabled((i + 1) as u32));
        }
    }

    // ---- ScanStatus ----

    #[test]
    fn scan_status_parse_glg_standard() {
        let status =
            ScanStatus::parse_glg("GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,").expect("should parse");
        assert_eq!(status.frequency.to_string(), "123.9750");
        assert_eq!(status.bank, Some(2));
        assert_eq!(status.channel_name, "BHX RADAR");
        assert!(matches!(status.modulation, Modulation::Am));
        assert!(status.signal_detected);
        assert_eq!(status.channel_index.map(|i| i.get()), Some(52));
    }

    #[test]
    fn scan_status_parse_glg_low_frequency() {
        let status =
            ScanStatus::parse_glg("GLG,00881000,FM,,0,,,BBC R2,1,0,,1,").expect("should parse");
        assert_eq!(status.frequency.to_string(), "88.1000");
        assert_eq!(status.bank, Some(1));
        assert_eq!(status.channel_name, "BBC R2");
        assert!(matches!(status.modulation, Modulation::Fm));
    }

    #[test]
    fn scan_status_parse_glg_no_signal() {
        let status =
            ScanStatus::parse_glg("GLG,01239750,AM,,0,,,QUIET,0,0,,52,").expect("should parse");
        assert!(!status.signal_detected);
    }

    #[test]
    fn scan_status_parse_glg_invalid_prefix() {
        assert!(ScanStatus::parse_glg("GARBAGE").is_none());
    }

    #[test]
    fn scan_status_parse_glg_too_few_fields() {
        assert!(ScanStatus::parse_glg("GLG,short").is_none());
    }

    #[test]
    fn scan_status_parse_glg_invalid_frequency() {
        // Not a valid 8-digit frequency
        let resp = "GLG,not-a-frequency,,,,,,channel,,0,,52,";
        assert!(ScanStatus::parse_glg(resp).is_none());
    }

    #[test]
    fn scan_status_parse_glg_missing_channel_index() {
        // Fewer fields than required by GLG_CHANNEL_INDEX_IDX
        let resp = "GLG,01239750,AM,,0,,,BHX RADAR,1,0,,";
        assert!(ScanStatus::parse_glg(resp).is_none());
    }

    #[test]
    fn scan_status_bank_display() {
        let status =
            ScanStatus::parse_glg("GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,").unwrap();
        assert_eq!(status.bank_display(), "2");

        // Default status has no bank.
        let default = ScanStatus::default();
        assert_eq!(default.bank_display(), "-");
    }

    // ---- Channel ----

    #[test]
    fn channel_parse_cin_valid() {
        let ch = Channel::parse_cin("CIN,52,BHX RADAR,01239750,AM,0,0,0,0").expect("should parse");
        assert_eq!(ch.index.get(), 52);
        assert_eq!(ch.name, "BHX RADAR");
        assert_eq!(ch.frequency.to_string(), "123.9750");
        assert!(matches!(ch.modulation, Modulation::Am));
    }

    #[test]
    fn channel_parse_cin_empty_frequency() {
        let ch = Channel::parse_cin("CIN,1,EMPTY,00000000,FM,0,0,0,0").expect("should parse");
        assert!(ch.frequency.is_empty());
        assert_eq!(ch.frequency.to_string(), "");
    }

    #[test]
    fn channel_parse_cin_invalid_prefix() {
        assert!(Channel::parse_cin("GARBAGE").is_none());
    }

    #[test]
    fn channel_parse_cin_invalid_index() {
        assert!(Channel::parse_cin("CIN,0,NAME,01239750,AM").is_none()); // idx 0 invalid
        assert!(Channel::parse_cin("CIN,501,NAME,01239750,AM").is_none()); // idx 501 invalid
    }
}
