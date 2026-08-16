/// Scanner operating mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Monitor (scan) mode — scanner is actively scanning or held.
    Monitor,
    /// Program mode — scanner accepts memory/edit commands.
    Program,
}

/// Note: the scanner must be in Program mode to issue memory commands
/// (CIN, SCG, DCH, ...). Mode state is tracked by
/// [`ScannerClient`](crate::scanner::ScannerClient), which sends `PRG`/`EPG`
/// as part of its typed operations.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_equality() {
        assert_eq!(Mode::Monitor, Mode::Monitor);
        assert_eq!(Mode::Program, Mode::Program);
        assert_ne!(Mode::Monitor, Mode::Program);
    }
}
