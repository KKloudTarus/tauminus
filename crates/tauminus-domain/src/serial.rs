// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! How a serial port is opened: its path and line settings.

use thiserror::Error;

use crate::ssh::Encoding;

/// Longest port path accepted.
const MAX_PATH: usize = 256;
/// The speeds offered: the standard ones every UART driver knows.
pub const BAUD_RATES: [u32; 15] = [
    300, 1200, 2400, 4800, 9600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 500_000,
    921_600, 1_000_000, 2_000_000,
];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SerialError {
    #[error("a port path is 1 to {MAX_PATH} characters with no control characters")]
    Path,
    #[error("{0} baud is not offered; choose one of the standard speeds")]
    Baud(u32),
    #[error("a character is 5 to 8 data bits")]
    DataBits,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum StopBits {
    #[default]
    One,
    Two,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SerialFlow {
    #[default]
    None,
    /// XON/XOFF.
    Software,
    /// RTS/CTS.
    Hardware,
}

/// A port and how to talk on it: 115200 8N1 with no flow control unless
/// set otherwise, which is what most consoles use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialSettings {
    path: String,
    baud: u32,
    data_bits: u8,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow: SerialFlow,
    pub encoding: Encoding,
}

impl SerialSettings {
    /// Checks the path and speed. O(n) in the path.
    pub fn new(path: &str, baud: u32, data_bits: u8) -> Result<Self, SerialError> {
        if path.is_empty() || path.chars().count() > MAX_PATH || path.chars().any(char::is_control)
        {
            return Err(SerialError::Path);
        }
        if !BAUD_RATES.contains(&baud) {
            return Err(SerialError::Baud(baud));
        }
        if !(5..=8).contains(&data_bits) {
            return Err(SerialError::DataBits);
        }
        Ok(Self {
            path: path.to_owned(),
            baud,
            data_bits,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow: SerialFlow::None,
            encoding: Encoding::Utf8,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn baud(&self) -> u32 {
        self.baud
    }

    pub fn data_bits(&self) -> u8 {
        self.data_bits
    }

    /// As consoles write it: `/dev/ttyUSB0 115200 8N1`.
    pub fn describe(&self) -> String {
        let parity = match self.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        };
        let stop = match self.stop_bits {
            StopBits::One => 1,
            StopBits::Two => 2,
        };
        format!(
            "{} {} {}{parity}{stop}",
            self.path, self.baud, self.data_bits
        )
    }
}

/// A port the system lists, and what it says the port is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialPortInfo {
    pub path: String,
    /// "USB: FTDI FT232R", "Bluetooth", or empty when the system says nothing.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_needs_a_path_and_a_standard_speed() {
        let settings = SerialSettings::new("/dev/ttyUSB0", 115_200, 8).unwrap();
        assert_eq!(settings.describe(), "/dev/ttyUSB0 115200 8N1");
        assert_eq!(SerialSettings::new("", 9600, 8), Err(SerialError::Path));
        assert_eq!(
            SerialSettings::new("COM3\n", 9600, 8),
            Err(SerialError::Path)
        );
        assert_eq!(
            SerialSettings::new("COM3", 1234, 8),
            Err(SerialError::Baud(1234))
        );
        assert_eq!(
            SerialSettings::new("COM3", 9600, 9),
            Err(SerialError::DataBits)
        );
        let mut seven = SerialSettings::new("COM3", 9600, 7).unwrap();
        seven.parity = Parity::Even;
        seven.stop_bits = StopBits::Two;
        assert_eq!(seven.describe(), "COM3 9600 7E2");
    }
}
