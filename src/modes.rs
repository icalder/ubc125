use std::io;

use crate::scanner::ScannerClient;

/// Scanner operating mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Monitor (scan) mode — scanner is actively scanning or held.
    Monitor,
    /// Program mode — scanner accepts memory/edit commands.
    Program,
}

/// Manages PRG/EPG mode transitions for the scanner.
///
/// The scanner must be in Program mode to issue memory commands (CIN, SCG,
/// DCH, etc.) and must return to Monitor mode for scanning. This struct
/// tracks the current mode and ensures transitions are done atomically.
pub struct ModeManager {
    current: Mode,
}

impl ModeManager {
    /// Start in Monitor mode (scanner default).
    pub fn new() -> Self {
        Self {
            current: Mode::Monitor,
        }
    }

    /// Enter Program mode if not already there.
    ///
    /// Sends `PRG` to the scanner.
    pub fn ensure_program(&mut self, client: &mut ScannerClient) -> Result<(), io::Error> {
        if self.current == Mode::Program {
            return Ok(());
        }
        client.send_command("PRG")?;
        self.current = Mode::Program;
        Ok(())
    }

    /// Return to Monitor mode if in Program mode.
    ///
    /// Sends `EPG` followed by `KEY,S,P` to resume scanning.
    pub fn ensure_monitor(&mut self, client: &mut ScannerClient) -> Result<(), io::Error> {
        if self.current == Mode::Monitor {
            return Ok(());
        }
        client.send_command("EPG")?;
        client.send_command("KEY,S,P")?;
        self.current = Mode::Monitor;
        Ok(())
    }

    /// Returns the current mode without side effects.
    pub fn current(&self) -> Mode {
        self.current
    }

    /// Check if currently in Program mode (convenience for callers
    /// that only need a boolean).
    pub fn is_prg(&self) -> bool {
        self.current() == Mode::Program
    }
}

impl Default for ModeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_mode_is_monitor() {
        let mgr = ModeManager::new();
        assert_eq!(mgr.current(), Mode::Monitor);
        assert!(!mgr.is_prg());
    }

    #[test]
    fn mode_equality() {
        assert_eq!(Mode::Monitor, Mode::Monitor);
        assert_eq!(Mode::Program, Mode::Program);
        assert_ne!(Mode::Monitor, Mode::Program);
    }
}
