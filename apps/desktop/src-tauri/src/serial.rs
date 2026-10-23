// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Serial ports over IPC: what is attached, and a session on one.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::SerialSessions;
use tauminus_domain::{BAUD_RATES, Encoding, Parity, SerialFlow, SerialSettings, StopBits};
use tauri::State;
use tauri::ipc::Channel;

use crate::hosts::EncodingSpec;
use crate::ipc::{ChannelSink, CommandError, ErrorCode, OutputChunk, SessionExit, SessionHandle};

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct SerialPortView {
    pub(crate) path: String,
    /// What the system says the port is; empty when it says nothing.
    pub(crate) description: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SerialPortsView {
    pub(crate) ports: Vec<SerialPortView>,
    /// The speeds a session may use.
    pub(crate) baud_rates: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ParitySpec {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FlowSpec {
    None,
    /// XON/XOFF.
    Software,
    /// RTS/CTS.
    Hardware,
}

/// A port and its line settings, as the view sends them.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SerialSpec {
    pub(crate) path: String,
    pub(crate) baud: u32,
    pub(crate) data_bits: u8,
    pub(crate) parity: ParitySpec,
    /// 1 or 2.
    pub(crate) stop_bits: u8,
    pub(crate) flow: FlowSpec,
    #[serde(default)]
    pub(crate) encoding: Option<EncodingSpec>,
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

pub(crate) fn settings(spec: &SerialSpec) -> Result<SerialSettings, CommandError> {
    let mut settings =
        SerialSettings::new(&spec.path, spec.baud, spec.data_bits).map_err(invalid)?;
    settings.parity = match spec.parity {
        ParitySpec::None => Parity::None,
        ParitySpec::Odd => Parity::Odd,
        ParitySpec::Even => Parity::Even,
    };
    settings.stop_bits = match spec.stop_bits {
        1 => StopBits::One,
        2 => StopBits::Two,
        other => return Err(invalid(format!("{other} stop bits; a port uses 1 or 2"))),
    };
    settings.flow = match spec.flow {
        FlowSpec::None => SerialFlow::None,
        FlowSpec::Software => SerialFlow::Software,
        FlowSpec::Hardware => SerialFlow::Hardware,
    };
    settings.encoding = spec.encoding.map_or(Encoding::Utf8, Encoding::from);
    Ok(settings)
}

/// The ports attached now, and the speeds offered.
#[tauri::command]
#[specta::specta]
pub(crate) async fn serial_ports(
    serial: State<'_, SerialSessions>,
) -> Result<SerialPortsView, CommandError> {
    let serial = serial.inner().clone();
    // Listing reads the system's device tree; keep it off async workers.
    let ports = tauri::async_runtime::spawn_blocking(move || serial.list())
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(SerialPortsView {
        ports: ports
            .into_iter()
            .map(|p| SerialPortView {
                path: p.path,
                description: p.description,
            })
            .collect(),
        baud_rates: BAUD_RATES.to_vec(),
    })
}

/// Opens a serial port in a pane. Output arrives on `on_output`, and
/// `on_exit` fires when the port closes or is unplugged.
#[tauri::command]
#[specta::specta]
pub(crate) async fn serial_open(
    serial: State<'_, SerialSessions>,
    spec: SerialSpec,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
) -> Result<SessionHandle, CommandError> {
    let settings = settings(&spec)?;
    let serial = serial.inner().clone();
    let sink = ChannelSink {
        output: on_output,
        exit: on_exit,
    };
    // Opening a port blocks on the driver.
    let id = tauri::async_runtime::spawn_blocking(move || serial.open(&settings, sink))
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))??;
    SessionHandle::try_from(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SerialSpec {
        SerialSpec {
            path: "/dev/ttyUSB0".into(),
            baud: 115_200,
            data_bits: 8,
            parity: ParitySpec::None,
            stop_bits: 1,
            flow: FlowSpec::None,
            encoding: None,
        }
    }

    #[test]
    fn a_spec_becomes_settings_or_says_why_not() {
        let mut even = spec();
        even.parity = ParitySpec::Even;
        even.stop_bits = 2;
        even.data_bits = 7;
        even.encoding = Some(EncodingSpec::Koi8R);
        let parsed = settings(&even).unwrap();
        assert_eq!(parsed.describe(), "/dev/ttyUSB0 115200 7E2");
        assert_eq!(parsed.encoding, Encoding::Koi8R);
        for bad in [
            SerialSpec {
                stop_bits: 3,
                ..spec()
            },
            SerialSpec {
                baud: 12_345,
                ..spec()
            },
            SerialSpec {
                path: String::new(),
                ..spec()
            },
        ] {
            assert_eq!(settings(&bad).unwrap_err().code, ErrorCode::InvalidArgument);
        }
    }
}
