// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Serial ports as terminal streams, and the ports the system lists.
//!
//! `serialport` blocks, so as for the local PTY a reader and a writer each
//! run on a thread of their own. The reader wakes every 100 ms to see
//! whether the session was closed; a port unplugged mid-session ends it.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tauminus_app::ports::{
    LaunchError, SerialPorts, TerminalEvent, TerminalInput, TerminalStream, Terminator,
};
use tauminus_charset::Transcoder;
use tauminus_domain::{Parity, SerialFlow, SerialPortInfo, SerialSettings, StopBits};
use tokio::sync::mpsc;
use tracing::debug;

const EVENT_QUEUE: usize = 4;
const INPUT_QUEUE: usize = 64;
const READ_BUFFER: usize = 4096;
/// How often a waiting read looks up to see whether the session closed.
const POLL: Duration = Duration::from_millis(100);
/// Most ports listed; a system with more shows the first ones.
const MAX_PORTS: usize = 256;

/// The system's serial ports through `serialport`. Only a real serial
/// device opens: a character device on Unix, not a terminal of this user's
/// (`/dev/tty`, `/dev/pts/N`), and a `COMn` name on Windows, never a share
/// or a pipe.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeSerialPorts {
    pseudo_terminals: bool,
}

impl NativeSerialPorts {
    /// Also opens pseudo-terminals, as a virtual port pair is; for tests.
    pub fn allowing_pseudo_terminals() -> Self {
        Self {
            pseudo_terminals: true,
        }
    }
}

impl SerialPorts for NativeSerialPorts {
    fn list(&self) -> Vec<SerialPortInfo> {
        let ports = serialport::available_ports().unwrap_or_default();
        ports
            .into_iter()
            .take(MAX_PORTS)
            .map(|port| SerialPortInfo {
                path: port.port_name,
                description: describe(&port.port_type),
            })
            .collect()
    }

    fn open(&self, settings: &SerialSettings) -> Result<TerminalStream, LaunchError> {
        let failed = |source: std::io::Error| LaunchError::Spawn {
            program: settings.path().to_owned(),
            source,
        };
        check_device(settings.path(), self.pseudo_terminals).map_err(failed)?;
        let port = serialport::new(settings.path(), settings.baud())
            .data_bits(data_bits(settings.data_bits()))
            .parity(match settings.parity {
                Parity::None => serialport::Parity::None,
                Parity::Odd => serialport::Parity::Odd,
                Parity::Even => serialport::Parity::Even,
            })
            .stop_bits(match settings.stop_bits {
                StopBits::One => serialport::StopBits::One,
                StopBits::Two => serialport::StopBits::Two,
            })
            .flow_control(match settings.flow {
                SerialFlow::None => serialport::FlowControl::None,
                SerialFlow::Software => serialport::FlowControl::Software,
                SerialFlow::Hardware => serialport::FlowControl::Hardware,
            })
            .timeout(POLL)
            .open()
            .map_err(|e| failed(std::io::Error::other(e)))?;
        let writer = port
            .try_clone()
            .map_err(|e| failed(std::io::Error::other(e)))?;
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
        let closed = Arc::new(AtomicBool::new(false));
        // Made first: if a thread fails to start, dropping it stops the
        // other, and the port is let go.
        let terminator = SerialTerminator(Arc::clone(&closed));
        let encoding = settings.encoding;
        let reading = Arc::clone(&closed);
        thread::Builder::new()
            .name("serial-reader".into())
            .spawn(move || read_loop(port, &events_tx, &reading, Transcoder::new(encoding)))
            .map_err(failed)?;
        let writing = Arc::clone(&closed);
        thread::Builder::new()
            .name("serial-writer".into())
            .spawn(move || write_loop(writer, input_rx, &writing, &Transcoder::new(encoding)))
            .map_err(failed)?;
        Ok(TerminalStream {
            input: input_tx,
            events: events_rx,
            terminator: Box::new(terminator),
        })
    }
}

fn data_bits(bits: u8) -> serialport::DataBits {
    match bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    }
}

fn describe(kind: &serialport::SerialPortType) -> String {
    match kind {
        serialport::SerialPortType::UsbPort(usb) => {
            let name = [usb.manufacturer.as_deref(), usb.product.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            if name.is_empty() {
                format!("USB {:04x}:{:04x}", usb.vid, usb.pid)
            } else {
                format!("USB: {name}")
            }
        }
        serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
        serialport::SerialPortType::PciPort => "PCI".into(),
        serialport::SerialPortType::Unknown => String::new(),
    }
}

/// Closes the port: the reader stops within [`POLL`] and the session ends.
struct SerialTerminator(Arc<AtomicBool>);

impl Terminator for SerialTerminator {
    fn terminate(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Drop for SerialTerminator {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn read_loop(
    mut port: Box<dyn serialport::SerialPort>,
    events: &mpsc::Sender<TerminalEvent>,
    closed: &AtomicBool,
    mut output: Transcoder,
) {
    let mut buffer = [0_u8; READ_BUFFER];
    while !closed.load(Ordering::Acquire) {
        match port.read(&mut buffer) {
            Ok(0) => {}
            Ok(n) => {
                let shown = output.decode(&buffer[..n], false);
                if !shown.is_empty() && events.blocking_send(TerminalEvent::Output(shown)).is_err()
                {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // A hangup with bytes still waiting: what the device sent last
            // is shown before the session ends.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                let waiting = port.bytes_to_read().unwrap_or(0);
                let take = usize::try_from(waiting).unwrap_or(0).min(READ_BUFFER);
                if take > 0
                    && let Ok(n) = port.read(&mut buffer[..take])
                {
                    let shown = output.decode(&buffer[..n], false);
                    let _ = events.blocking_send(TerminalEvent::Output(shown));
                }
                break;
            }
            Err(e) => {
                // Unplugged, or the driver gave up.
                debug!(error = %e, "serial read failed");
                break;
            }
        }
    }
    let rest = output.decode(&[], true);
    if !rest.is_empty() {
        let _ = events.blocking_send(TerminalEvent::Output(rest));
    }
    // A serial line has no exit status.
    let _ = events.blocking_send(TerminalEvent::Exited { code: None });
}

fn write_loop(
    mut port: Box<dyn serialport::SerialPort>,
    mut input: mpsc::Receiver<TerminalInput>,
    closed: &AtomicBool,
    codec: &Transcoder,
) {
    while let Some(message) = input.blocking_recv() {
        let bytes = match message {
            TerminalInput::Text(text) => codec.encode(&text),
            TerminalInput::Data(bytes) => bytes,
            // A serial line carries no window size.
            TerminalInput::Resize(_) => continue,
        };
        if let Err(e) = write_held(&mut *port, &bytes, closed) {
            debug!(error = %e, "serial write failed");
            return;
        }
    }
}

/// Writes all of `bytes`, waiting as long as the device holds the line
/// (XOFF, or CTS low): each wait is one poll, and only a close or a real
/// error gives up. No drain: a held line would block one forever.
fn write_held(
    port: &mut dyn serialport::SerialPort,
    mut bytes: &[u8],
    closed: &AtomicBool,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        if closed.load(Ordering::Acquire) {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        match port.write(bytes) {
            Ok(0) => {}
            Ok(n) => bytes = &bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Refuses what is not a serial device, before anything opens it.
#[cfg(unix)]
fn check_device(path: &str, pseudo_terminals: bool) -> std::io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let refuse = |why: &str| {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            why.to_owned(),
        ))
    };
    let real = std::fs::canonicalize(path)?;
    if !std::fs::metadata(&real)?.file_type().is_char_device() {
        return refuse("not a serial device");
    }
    let name = real.to_string_lossy();
    // This user's own terminals: opening one would take its keystrokes
    // and print the pane's input there.
    let own_terminal = name == "/dev/tty"
        || name == "/dev/console"
        || name.starts_with("/dev/pts/")
        || name
            .strip_prefix("/dev/ttys")
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
        || name
            .strip_prefix("/dev/tty")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if own_terminal && !pseudo_terminals {
        return refuse("a terminal, not a serial port");
    }
    Ok(())
}

/// `COM3` or `\\.\COM3`: nothing that names a share, a pipe, or a disk.
#[cfg(windows)]
fn check_device(path: &str, _pseudo_terminals: bool) -> std::io::Result<()> {
    if is_com_name(path) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a COM port",
        ))
    }
}

/// Whether `path` names a Windows COM port, with or without `\\.\`.
#[cfg_attr(not(windows), allow(dead_code, reason = "tested on every platform"))]
fn is_com_name(path: &str) -> bool {
    let name = path.strip_prefix(r"\\.\").unwrap_or(path);
    name.get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("COM"))
        && name
            .get(3..)
            .is_some_and(|n| (1..=3).contains(&n.len()) && n.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::is_com_name;

    #[test]
    fn only_com_names_pass_on_windows() {
        for good in ["COM1", "com12", r"\\.\COM256"] {
            assert!(is_com_name(good), "{good}");
        }
        for bad in [
            r"\\host\share\x",
            r"\\.\pipe\name",
            r"\\.\PhysicalDrive0",
            "COM",
            "COM1234",
            "COM1:",
            "LPT1",
        ] {
            assert!(!is_com_name(bad), "{bad}");
        }
    }
}
