use serialport::SerialPort;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::constants::{MAX_LEVEL, PORT_TIMEOUT_MS, READ_TIMEOUT_MS};

pub struct ScannerClient {
    port: Box<dyn SerialPort>,
}

impl ScannerClient {
    pub fn new(device_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let port = serialport::new(device_path, 115_200)
            .timeout(Duration::from_millis(PORT_TIMEOUT_MS))
            .open()?;

        // Clear buffer
        let _ = port.clear(serialport::ClearBuffer::All);

        Ok(Self { port })
    }

    pub fn send_command(&mut self, cmd: &str) -> Result<String, io::Error> {
        let mut command = String::from(cmd);
        command.push('\r');
        self.port.write_all(command.as_bytes())?;

        let mut response = String::new();
        let mut buf = [0u8; 1];
        let start = Instant::now();
        let timeout = Duration::from_millis(READ_TIMEOUT_MS);

        loop {
            if start.elapsed() > timeout {
                // Timeout: return whatever has been accumulated so far.
                break;
            }
            match self.port.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let c = buf[0] as char;
                    if c == '\r' {
                        break;
                    }
                    if c != '\n' {
                        response.push(c);
                    }
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
        }
        Ok(response.trim().to_string())
    }

    pub fn get_volume(&mut self) -> Result<String, io::Error> {
        self.send_command("VOL")
    }

    pub fn set_volume(&mut self, level: u8) -> Result<String, io::Error> {
        if level > MAX_LEVEL {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Volume level must be between 0 and 15",
            ));
        }
        self.send_command(&format!("VOL,{}", level))
    }

    pub fn get_squelch(&mut self) -> Result<String, io::Error> {
        self.send_command("SQL")
    }

    pub fn set_squelch(&mut self, level: u8) -> Result<String, io::Error> {
        if level > MAX_LEVEL {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Squelch level must be between 0 and 15",
            ));
        }
        self.send_command(&format!("SQL,{}", level))
    }
}
