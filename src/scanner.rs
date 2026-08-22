use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::constants::{
    CHANNELS_PER_BANK, MAX_CHANNELS, MAX_LEVEL, NUM_BANKS, PORT_TIMEOUT_MS, READ_TIMEOUT_MS,
};
use crate::modes::Mode;
use crate::types::{BankMask, Channel, ChannelIndex, ScanStatus};

/// Byte-level link to the scanner.
///
/// Abstracts the serial port so the command layer can be tested against a
/// scripted transport. `Send` so a `ScannerClient` can be shared across
/// threads (e.g. behind an `Arc<Mutex<..>>` in the gRPC server).
pub trait Transport: Send {
    /// Write raw bytes to the scanner.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;

    /// Read a single byte, or return an error with
    /// [`io::ErrorKind::TimedOut`] when no data arrives within the port
    /// timeout.
    fn read_byte(&mut self) -> io::Result<u8>;
}

/// Production transport backed by a serial port.
pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
}

impl Transport for SerialTransport {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.port.write_all(buf)
    }

    fn read_byte(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.port.read_exact(&mut buf).map(|_| buf[0])
    }
}

/// Errors from scanner communication and command validation.
#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    #[error("serial I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("timed out waiting for response to {command} (partial: {partial:?})")]
    Timeout { command: String, partial: String },

    #[error("unexpected response to {command}: {got}")]
    UnexpectedResponse { command: String, got: String },

    #[error("volume level must be 0..={MAX_LEVEL}")]
    InvalidVolume(u8),

    #[error("squelch level must be 0..={MAX_LEVEL}")]
    InvalidSquelch(u8),

    #[error("channel index must be 1..={MAX_CHANNELS}")]
    InvalidChannelIndex(u32),

    #[error("bank number must be 1..={NUM_BANKS}")]
    InvalidBank(u32),
}

/// Scanner negative-acknowledgement tokens (see SCANNER-COMMANDS.md).
const ERROR_REPLIES: [&str; 2] = ["ERR", "NG"];

/// Serial command client for the UBC125XLT scanner.
///
/// Owns the transport and the scanner's program-mode state. Prefer the
/// typed methods (`get_status`, `set_banks`, ...); [`Self::send_command`]
/// is the raw escape hatch still used by the console.
pub struct ScannerClient {
    transport: Box<dyn Transport>,
    mode: Mode,
}

impl ScannerClient {
    /// Open the scanner at `device_path` (115200 baud).
    pub fn new(device_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let port = serialport::new(device_path, 115_200)
            .timeout(Duration::from_millis(PORT_TIMEOUT_MS))
            .open()?;
        // A failed clear is not fatal (the port may not support it), but
        // stale bytes could corrupt the first exchange, so log it.
        if let Err(e) = port.clear(serialport::ClearBuffer::All) {
            tracing::warn!("failed to clear serial buffers on startup: {e}");
        }
        Ok(Self::with_transport(Box::new(SerialTransport { port })))
    }

    /// Wrap an arbitrary transport (a scripted mock in tests).
    pub fn with_transport(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            mode: Mode::Monitor,
        }
    }

    /// The scanner's current mode as tracked by this client.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Enter program mode (`PRG`) if not already in it.
    pub fn ensure_program(&mut self) -> Result<(), ScannerError> {
        if self.mode == Mode::Program {
            return Ok(());
        }
        self.send_command("PRG")?;
        self.mode = Mode::Program;
        Ok(())
    }

    /// Return to monitor mode (`EPG` + `KEY,S,P`) if in program mode.
    pub fn ensure_monitor(&mut self) -> Result<(), ScannerError> {
        if self.mode == Mode::Monitor {
            return Ok(());
        }
        self.send_command("EPG")?;
        self.send_command("KEY,S,P")?;
        self.mode = Mode::Monitor;
        Ok(())
    }

    // -- raw protocol -----------------------------------------------------

    /// Send a command and read the full response line.
    ///
    /// `cmd` is suffixed with `\r`. The response is read until `\r`, with
    /// `\n` stripped and whitespace trimmed.
    ///
    /// On read timeout, the data accumulated so far (usually empty) is
    /// returned rather than an error. This matches the scanner's
    /// fire-and-forget action commands (PRG, EPG, KEY, DCH), which may not
    /// reply. New code should prefer the typed methods, which treat
    /// timeouts as errors.
    pub fn send_command(&mut self, cmd: &str) -> Result<String, ScannerError> {
        match self.exchange(cmd) {
            Ok(response) => Ok(response),
            Err(ScannerError::Timeout { partial, .. }) => Ok(partial),
            Err(e) => Err(e),
        }
    }

    /// Strict variant of [`Self::send_command`]: a read timeout is an
    /// error carrying any partial data.
    fn exchange(&mut self, cmd: &str) -> Result<String, ScannerError> {
        let mut bytes = String::from(cmd);
        bytes.push('\r');
        self.transport.write_all(bytes.as_bytes())?;

        let mut response = String::new();
        let start = Instant::now();
        let timeout = Duration::from_millis(READ_TIMEOUT_MS);
        loop {
            if start.elapsed() > timeout {
                return Err(ScannerError::Timeout {
                    command: cmd.to_string(),
                    partial: response,
                });
            }
            match self.transport.read_byte() {
                Ok(b) => {
                    let c = b as char;
                    if c == '\r' {
                        break;
                    }
                    if c != '\n' {
                        response.push(c);
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(ScannerError::Io(e)),
            }
        }
        Ok(response.trim().to_string())
    }

    /// Send an action command that may not produce a response.
    ///
    /// A read timeout is tolerated (the action was still issued). An
    /// `ERR` or `NG` reply is an error.
    fn send_action(&mut self, cmd: &str) -> Result<(), ScannerError> {
        match self.exchange(cmd) {
            Ok(response) if ERROR_REPLIES.contains(&response.as_str()) => {
                Err(ScannerError::UnexpectedResponse {
                    command: cmd.to_string(),
                    got: response,
                })
            }
            Ok(_) => Ok(()),
            Err(ScannerError::Timeout { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Validate that `response` looks like a reply to `cmd` (same command
    /// prefix, ignoring parameters).
    fn check_reply(cmd: &str, response: &str) -> Result<(), ScannerError> {
        // `split` always yields at least one item ("" for an empty cmd).
        let prefix = cmd.split(',').next().unwrap();
        if response.starts_with(prefix) {
            Ok(())
        } else {
            Err(ScannerError::UnexpectedResponse {
                command: cmd.to_string(),
                got: response.to_string(),
            })
        }
    }

    /// Run `f` in program mode. If this call entered program mode, it
    /// returns to monitor mode afterwards; if the client was already in
    /// program mode (e.g. the console browsing a bank tab), it stays
    /// there so batches of channel operations avoid repeated mode
    /// round-trips. A failure to return to monitor mode is logged but the
    /// original result is preserved.
    fn with_program_mode<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ScannerError>,
    ) -> Result<T, ScannerError> {
        let entered = self.mode != Mode::Program;
        self.ensure_program()?;
        let result = f(self);
        if entered && let Err(e) = self.ensure_monitor() {
            tracing::warn!("failed to return to monitor mode: {e}");
        }
        result
    }

    // -- system info ------------------------------------------------------

    /// Get the model string (`MDL`).
    pub fn get_model(&mut self) -> Result<String, ScannerError> {
        let response = self.exchange("MDL")?;
        Self::check_reply("MDL", &response)?;
        Ok(response)
    }

    /// Get the firmware version string (`VER`).
    pub fn get_firmware_version(&mut self) -> Result<String, ScannerError> {
        let response = self.exchange("VER")?;
        Self::check_reply("VER", &response)?;
        Ok(response)
    }

    // -- audio --------------------------------------------------------------

    /// Get the current volume setting (`VOL`), as the raw response.
    pub fn get_volume(&mut self) -> Result<String, ScannerError> {
        let response = self.exchange("VOL")?;
        Self::check_reply("VOL", &response)?;
        Ok(response)
    }

    /// Set the volume (0–15).
    pub fn set_volume(&mut self, level: u8) -> Result<(), ScannerError> {
        if level > MAX_LEVEL {
            return Err(ScannerError::InvalidVolume(level));
        }
        let cmd = format!("VOL,{level}");
        let response = self.exchange(&cmd)?;
        Self::check_reply(&cmd, &response)
    }

    /// Get the current squelch setting (`SQL`), as the raw response.
    pub fn get_squelch(&mut self) -> Result<String, ScannerError> {
        let response = self.exchange("SQL")?;
        Self::check_reply("SQL", &response)?;
        Ok(response)
    }

    /// Set the squelch level (0–15).
    pub fn set_squelch(&mut self, level: u8) -> Result<(), ScannerError> {
        if level > MAX_LEVEL {
            return Err(ScannerError::InvalidSquelch(level));
        }
        let cmd = format!("SQL,{level}");
        let response = self.exchange(&cmd)?;
        Self::check_reply(&cmd, &response)
    }

    // -- scan status --------------------------------------------------------

    /// Get the current scan status (`GLG`).
    pub fn get_status(&mut self) -> Result<ScanStatus, ScannerError> {
        let response = self.exchange("GLG")?;
        ScanStatus::parse_glg(&response).ok_or_else(|| ScannerError::UnexpectedResponse {
            command: "GLG".to_string(),
            got: response,
        })
    }

    /// Start scanning (simulates pressing the SCAN key).
    pub fn start_scan(&mut self) -> Result<(), ScannerError> {
        self.send_action("KEY,S,P")
    }

    /// Toggle scan hold (simulates pressing the HOLD key).
    pub fn hold_scan(&mut self) -> Result<(), ScannerError> {
        self.send_action("KEY,H,P")
    }

    // -- banks ---------------------------------------------------------------

    /// Get the bank enable mask (`SCG`).
    pub fn get_banks(&mut self) -> Result<BankMask, ScannerError> {
        self.with_program_mode(|client| {
            let response = client.exchange("SCG")?;
            Self::check_reply("SCG", &response)?;
            BankMask::from_scanner_response(&response).ok_or_else(|| {
                ScannerError::UnexpectedResponse {
                    command: "SCG".to_string(),
                    got: response,
                }
            })
        })
    }

    /// Set the bank enable mask (`SCG,##########`).
    pub fn set_banks(&mut self, mask: &BankMask) -> Result<(), ScannerError> {
        let cmd = mask.to_scanner_command();
        self.with_program_mode(|client| {
            let response = client.exchange(&cmd)?;
            Self::check_reply(&cmd, &response)
        })
    }

    // -- channels --------------------------------------------------------------

    fn validate_index(index: u32) -> Result<(), ScannerError> {
        if ChannelIndex::new(index).is_none() {
            Err(ScannerError::InvalidChannelIndex(index))
        } else {
            Ok(())
        }
    }

    /// Get a channel's info (`CIN,[INDEX]`).
    pub fn get_channel(&mut self, index: u32) -> Result<Channel, ScannerError> {
        Self::validate_index(index)?;
        let cmd = format!("CIN,{index}");
        self.with_program_mode(|client| {
            let response = client.exchange(&cmd)?;
            Channel::parse_cin(&response).ok_or_else(|| ScannerError::UnexpectedResponse {
                command: cmd.clone(),
                got: response,
            })
        })
    }

    /// Set a channel's info (`CIN,[INDEX],[NAME],[FRQ],[MOD],0,0,0,0`).
    pub fn set_channel(&mut self, channel: &Channel) -> Result<(), ScannerError> {
        Self::validate_index(channel.index.get())?;
        let cmd = format!(
            "CIN,{},{},{},{},0,0,0,0",
            channel.index,
            channel.name,
            channel.frequency.to_raw(),
            channel.modulation
        );
        self.with_program_mode(|client| {
            let response = client.exchange(&cmd)?;
            Self::check_reply(&cmd, &response)
        })
    }

    /// Delete a channel (`DCH,[INDEX]`).
    pub fn delete_channel(&mut self, index: u32) -> Result<(), ScannerError> {
        Self::validate_index(index)?;
        let cmd = format!("DCH,{index}");
        self.with_program_mode(|client| client.send_action(&cmd))
    }

    fn validate_bank(bank: u32) -> Result<(), ScannerError> {
        if (1..=NUM_BANKS as u32).contains(&bank) {
            Ok(())
        } else {
            Err(ScannerError::InvalidBank(bank))
        }
    }

    /// Get all non-empty channels in a bank (1–10).
    ///
    /// One program-mode session for the whole batch: a single `PRG`, up to
    /// 50 `CIN` reads, then a single return to monitor mode. Slots that
    /// fail to read (empty channels time out or reply oddly) are skipped
    /// rather than failing the whole batch — the caller fills in the
    /// missing rows.
    pub fn get_bank_channels(&mut self, bank: u32) -> Result<Vec<Channel>, ScannerError> {
        Self::validate_bank(bank)?;
        let first = (bank - 1) * CHANNELS_PER_BANK + 1;
        let last = bank * CHANNELS_PER_BANK;
        self.with_program_mode(|client| {
            let mut channels = Vec::new();
            for index in first..=last {
                match client.get_channel(index) {
                    Ok(channel) if !channel.frequency.is_empty() => channels.push(channel),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("batch fetch: channel {index} skipped: {e}"),
                }
            }
            Ok(channels)
        })
    }
}

/// Scripted transport for tests: returns queued responses (each
/// terminated with `\r`) in order, then times out. Records every command
/// written.
#[cfg(test)]
pub(crate) mod mock {
    use super::{ScannerClient, Transport};
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};

    pub struct MockTransport {
        responses: VecDeque<Vec<u8>>,
        pub written: Arc<Mutex<Vec<String>>>,
    }

    impl Transport for MockTransport {
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            self.written
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(buf).into_owned());
            Ok(())
        }

        fn read_byte(&mut self) -> io::Result<u8> {
            let mut chunk = self
                .responses
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no canned data"))?;
            let b = chunk.remove(0);
            if !chunk.is_empty() {
                self.responses.push_front(chunk);
            }
            Ok(b)
        }
    }

    /// A valid GLG response used across test modules.
    pub const GLG_OK: &str = "GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,";

    pub fn mock_client(responses: &[&str]) -> (ScannerClient, Arc<Mutex<Vec<String>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let responses = responses
            .iter()
            .map(|r| format!("{r}\r").into_bytes())
            .collect();
        (
            ScannerClient::with_transport(Box::new(MockTransport {
                responses,
                written: written.clone(),
            })),
            written,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::mock::*;
    use super::*;

    const CIN_OK: &str = "CIN,52,BHX RADAR,01239750,AM,0,0,0,0";

    // -- raw protocol --

    #[test]
    fn send_command_strips_newlines_and_trims() {
        let (mut client, written) = mock_client(&["GLG,123\n  "]);
        let resp = client.send_command("GLG").unwrap();
        assert_eq!(resp, "GLG,123");
        assert_eq!(*written.lock().unwrap(), vec!["GLG\r".to_string()]);
    }

    // -- system info --

    #[test]
    fn get_model_sends_mdl() {
        let (mut client, written) = mock_client(&["MDL,UBC125XLT"]);
        assert_eq!(client.get_model().unwrap(), "MDL,UBC125XLT");
        assert_eq!(*written.lock().unwrap(), vec!["MDL\r".to_string()]);
    }

    #[test]
    fn get_firmware_version_sends_ver() {
        let (mut client, written) = mock_client(&["VER,Version 2.02"]);
        assert_eq!(client.get_firmware_version().unwrap(), "VER,Version 2.02");
        assert_eq!(*written.lock().unwrap(), vec!["VER\r".to_string()]);
    }

    // -- status --

    #[test]
    fn get_status_parses_glg() {
        let (mut client, written) = mock_client(&[GLG_OK]);
        let status = client.get_status().unwrap();
        assert_eq!(status.channel_name, "BHX RADAR");
        assert_eq!(status.bank, Some(2));
        assert!(status.signal_detected);
        assert_eq!(*written.lock().unwrap(), vec!["GLG\r".to_string()]);
    }

    #[test]
    fn get_status_timeout_is_error() {
        let (mut client, _) = mock_client(&[]);
        assert!(matches!(
            client.get_status(),
            Err(ScannerError::Timeout { .. })
        ));
    }

    #[test]
    fn get_status_garbage_is_error() {
        let (mut client, _) = mock_client(&["GARBAGE"]);
        assert!(matches!(
            client.get_status(),
            Err(ScannerError::UnexpectedResponse { .. })
        ));
    }

    #[test]
    fn get_status_err_reply_is_error() {
        let (mut client, _) = mock_client(&["ERR"]);
        assert!(matches!(
            client.get_status(),
            Err(ScannerError::UnexpectedResponse { .. })
        ));
    }

    #[test]
    fn start_scan_no_response_is_ok() {
        let (mut client, written) = mock_client(&[]);
        client.start_scan().unwrap();
        assert_eq!(*written.lock().unwrap(), vec!["KEY,S,P\r".to_string()]);
    }

    #[test]
    fn start_scan_ng_is_error() {
        let (mut client, _) = mock_client(&["NG"]);
        assert!(matches!(
            client.start_scan(),
            Err(ScannerError::UnexpectedResponse { .. })
        ));
    }

    #[test]
    fn hold_scan_sends_key() {
        let (mut client, written) = mock_client(&["KEY"]);
        client.hold_scan().unwrap();
        assert_eq!(*written.lock().unwrap(), vec!["KEY,H,P\r".to_string()]);
    }

    // -- audio --

    #[test]
    fn set_volume_ok() {
        let (mut client, written) = mock_client(&["VOL,12"]);
        client.set_volume(12).unwrap();
        assert_eq!(*written.lock().unwrap(), vec!["VOL,12\r".to_string()]);
    }

    #[test]
    fn set_volume_rejects_over_max_without_port_access() {
        let (mut client, written) = mock_client(&[]);
        assert!(matches!(
            client.set_volume(16),
            Err(ScannerError::InvalidVolume(16))
        ));
        assert!(written.lock().unwrap().is_empty());
    }

    #[test]
    fn set_squelch_rejects_over_max_without_port_access() {
        let (mut client, written) = mock_client(&[]);
        assert!(matches!(
            client.set_squelch(16),
            Err(ScannerError::InvalidSquelch(16))
        ));
        assert!(written.lock().unwrap().is_empty());
    }

    // -- banks --

    #[test]
    fn get_banks_round_trip() {
        let (mut client, written) = mock_client(&["PRG", "SCG,0101010101", "EPG", "KEY"]);
        let mask = client.get_banks().unwrap();
        assert!(mask.is_enabled(1));
        assert!(!mask.is_enabled(2));
        assert_eq!(
            *written.lock().unwrap(),
            vec!["PRG\r", "SCG\r", "EPG\r", "KEY,S,P\r"]
        );
    }

    #[test]
    fn set_banks_program_mode_sequence() {
        let mask = BankMask::from_scanner_response("SCG,0101010101").unwrap();
        let (mut client, written) = mock_client(&["PRG", "SCG,0101010101", "EPG", "KEY"]);
        client.set_banks(&mask).unwrap();
        assert_eq!(
            *written.lock().unwrap(),
            vec!["PRG\r", "SCG,0101010101\r", "EPG\r", "KEY,S,P\r"]
        );
    }

    #[test]
    fn set_banks_failure_still_returns_to_monitor() {
        let mask = BankMask::from_scanner_response("SCG,0101010101").unwrap();
        // SCG command is rejected with NG.
        let (mut client, written) = mock_client(&["PRG", "NG", "EPG", "KEY"]);
        let err = client.set_banks(&mask).unwrap_err();
        assert!(matches!(err, ScannerError::UnexpectedResponse { .. }));
        assert_eq!(
            *written.lock().unwrap(),
            vec!["PRG\r", "SCG,0101010101\r", "EPG\r", "KEY,S,P\r"]
        );
        assert_eq!(client.mode(), Mode::Monitor);
    }

    #[test]
    fn op_in_program_mode_stays_in_program() {
        let (mut client, written) = mock_client(&["PRG", CIN_OK]);
        client.ensure_program().unwrap();
        client.get_channel(52).unwrap();
        assert_eq!(*written.lock().unwrap(), vec!["PRG\r", "CIN,52\r"]);
        assert_eq!(client.mode(), Mode::Program);
    }

    #[test]
    fn ensure_program_no_duplicate_prg() {
        let (mut client, written) = mock_client(&["PRG"]);
        client.ensure_program().unwrap();
        client.ensure_program().unwrap();
        assert_eq!(*written.lock().unwrap(), vec!["PRG\r".to_string()]);
        assert_eq!(client.mode(), Mode::Program);
    }

    // -- channels --

    #[test]
    fn get_channel_sequence_and_parse() {
        let (mut client, written) = mock_client(&["PRG", CIN_OK, "EPG", "KEY"]);
        let channel = client.get_channel(52).unwrap();
        assert_eq!(channel.name, "BHX RADAR");
        assert_eq!(channel.index.get(), 52);
        assert_eq!(
            *written.lock().unwrap(),
            vec!["PRG\r", "CIN,52\r", "EPG\r", "KEY,S,P\r"]
        );
    }

    #[test]
    fn get_channel_invalid_index_without_port_access() {
        let (mut client, written) = mock_client(&[]);
        assert!(matches!(
            client.get_channel(0),
            Err(ScannerError::InvalidChannelIndex(0))
        ));
        assert!(matches!(
            client.get_channel(501),
            Err(ScannerError::InvalidChannelIndex(501))
        ));
        assert!(written.lock().unwrap().is_empty());
    }

    #[test]
    fn set_channel_command_bytes() {
        let channel = Channel::parse_cin(CIN_OK).unwrap();
        let (mut client, written) = mock_client(&["PRG", CIN_OK, "EPG", "KEY"]);
        client.set_channel(&channel).unwrap();
        let mut cin_cmd = CIN_OK.to_string();
        cin_cmd.push('\r');
        assert_eq!(
            *written.lock().unwrap(),
            vec![
                "PRG\r".to_string(),
                cin_cmd,
                "EPG\r".to_string(),
                "KEY,S,P\r".to_string()
            ]
        );
    }

    #[test]
    fn delete_channel_sequence() {
        let (mut client, written) = mock_client(&["PRG", "DCH,52", "EPG", "KEY"]);
        client.delete_channel(52).unwrap();
        assert_eq!(
            *written.lock().unwrap(),
            vec!["PRG\r", "DCH,52\r", "EPG\r", "KEY,S,P\r"]
        );
    }

    #[test]
    fn delete_channel_ng_is_error_but_returns_to_monitor() {
        let (mut client, written) = mock_client(&["PRG", "NG", "EPG", "KEY"]);
        let err = client.delete_channel(52).unwrap_err();
        assert!(matches!(err, ScannerError::UnexpectedResponse { .. }));
        assert_eq!(
            *written.lock().unwrap(),
            vec!["PRG\r", "DCH,52\r", "EPG\r", "KEY,S,P\r"]
        );
    }

    // -- get_bank_channels --

    fn cin_response(index: u32) -> String {
        format!("CIN,{index},NAME{index},01239750,AM,0,0,0,0")
    }

    #[test]
    fn get_bank_channels_sends_one_prg_and_fifty_cins() {
        let responses: Vec<String> = std::iter::once("PRG".to_string())
            .chain((1..=50).map(cin_response))
            .collect();
        let (mut client, written) =
            mock_client(&responses.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let channels = client.get_bank_channels(1).unwrap();
        assert_eq!(channels.len(), 50);
        assert_eq!(channels[0].index.get(), 1);
        assert_eq!(channels[49].index.get(), 50);
        let written = written.lock().unwrap();
        // Exactly one mode transition for the whole batch.
        assert_eq!(written[0], "PRG\r");
        assert_eq!(written[1], "CIN,1\r");
        assert_eq!(written[50], "CIN,50\r");
        assert_eq!(written[51], "EPG\r");
        assert_eq!(written[52], "KEY,S,P\r");
        assert_eq!(written.len(), 53);
    }

    #[test]
    fn get_bank_channels_second_bank_indexes() {
        let responses: Vec<String> = std::iter::once("PRG".to_string())
            .chain((51..=100).map(cin_response))
            .collect();
        let (mut client, written) =
            mock_client(&responses.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let channels = client.get_bank_channels(2).unwrap();
        assert_eq!(channels.len(), 50);
        assert_eq!(channels[0].index.get(), 51);
        assert_eq!(channels[49].index.get(), 100);
        let written = written.lock().unwrap();
        assert_eq!(written[1], "CIN,51\r");
        assert_eq!(written[50], "CIN,100\r");
    }

    #[test]
    fn get_bank_channels_skips_empty_and_failed_slots() {
        // Slot 1: empty frequency (skipped). Slot 3: ok. Every other slot
        // replies garbage (unparseable, skipped).
        let responses: Vec<String> = (1..=50)
            .map(|i| match i {
                1 => "CIN,1,EMPTY,00000000,FM,0,0,0,0".to_string(),
                3 => cin_response(3),
                _ => "GARBAGE".to_string(),
            })
            .collect();
        let (mut client, _) =
            mock_client(&responses.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let channels = client.get_bank_channels(1).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].index.get(), 3);
    }

    #[test]
    fn get_bank_channels_invalid_bank_without_port_access() {
        let (mut client, written) = mock_client(&[]);
        for bank in [0u32, 11, 99] {
            assert!(matches!(
                client.get_bank_channels(bank),
                Err(ScannerError::InvalidBank(b)) if b == bank
            ));
        }
        assert!(written.lock().unwrap().is_empty());
    }

    #[test]
    fn get_bank_channels_stays_in_program_mode_if_already_there() {
        let responses: Vec<String> = std::iter::once("PRG".to_string())
            .chain((1..=50).map(cin_response))
            .collect();
        let (mut client, written) =
            mock_client(&responses.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        client.ensure_program().unwrap();
        client.get_bank_channels(1).unwrap();
        let written = written.lock().unwrap();
        // One PRG (from ensure_program), no EPG afterwards.
        assert_eq!(written[0], "PRG\r");
        assert!(!written.iter().any(|w| w == "EPG\r"));
        assert_eq!(client.mode(), Mode::Program);
    }
}
