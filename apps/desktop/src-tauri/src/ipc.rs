// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Wire types between the webview and the use cases. Domain types stay free of
//! serde and specta; these types translate at the boundary.

use serde::{Deserialize, Serialize};
use specta::Type;
use specta::datatype::DataType;
use tauminus_app::SessionError;
use tauminus_app::ports::{OutputSink, SinkClosed};
use tauminus_domain::{SessionId, TerminalSize};
use tauri::ipc::{Channel, InvokeResponseBody, IpcResponse};

/// One batch of terminal output. Sent as raw bytes, so JavaScript receives an
/// `ArrayBuffer` rather than a JSON array of numbers.
#[derive(Debug)]
pub(crate) struct OutputChunk(pub(crate) Vec<u8>);

impl IpcResponse for OutputChunk {
    fn body(self) -> tauri::Result<InvokeResponseBody> {
        Ok(InvokeResponseBody::Raw(self.0))
    }
}

impl Type for OutputChunk {
    fn definition(_: &mut specta::Types) -> DataType {
        DataType::Reference(specta_typescript::define("ArrayBuffer"))
    }
}

/// Sent once when a session ends.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionExit {
    /// `null` when the process ended without an exit status, e.g. by signal.
    pub(crate) exit_code: Option<u32>,
}

/// Session id as the webview sees it. A `u32` keeps it a plain JavaScript
/// number; `u64` would need `bigint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub(crate) struct SessionHandle(u32);

impl TryFrom<SessionId> for SessionHandle {
    type Error = CommandError;

    fn try_from(id: SessionId) -> Result<Self, Self::Error> {
        u32::try_from(id.get())
            .map(Self)
            .map_err(|_| CommandError::new(ErrorCode::Internal, "session id space exhausted"))
    }
}

impl From<SessionHandle> for SessionId {
    fn from(handle: SessionHandle) -> Self {
        Self::new(u64::from(handle.0))
    }
}

/// Grid size from the webview, validated before it reaches the use case.
#[derive(Debug, Clone, Copy, Deserialize, Type)]
pub(crate) struct GridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl TryFrom<GridSize> for TerminalSize {
    type Error = CommandError;

    fn try_from(size: GridSize) -> Result<Self, Self::Error> {
        Self::new(size.cols, size.rows)
            .map_err(|e| CommandError::new(ErrorCode::InvalidArgument, e.to_string()))
    }
}

/// Stable error codes the view can branch on. Messages are for logs and may
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCode {
    InvalidArgument,
    NotFound,
    LimitReached,
    LaunchFailed,
    /// Could not reach the host: DNS, refused, timeout, proxy, or network.
    ConnectFailed,
    /// The host key differs from the one on record. Never offered for trust.
    HostKeyChanged,
    HostKeyRevoked,
    AuthFailed,
    /// The user declined a prompt or closed it.
    Cancelled,
    ProtocolError,
    Internal,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct CommandError {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
}

impl CommandError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<SessionError> for CommandError {
    fn from(error: SessionError) -> Self {
        let code = match &error {
            SessionError::NotFound(_) => ErrorCode::NotFound,
            SessionError::LimitReached { .. } => ErrorCode::LimitReached,
            SessionError::Launch(_) => ErrorCode::LaunchFailed,
        };
        // Include the source chain: "failed to start session: failed to start
        // /bin/zsh: No such file or directory".
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        Self::new(code, message)
    }
}

/// Delivers a session's output and exit to the webview over two IPC channels.
pub(crate) struct ChannelSink {
    pub(crate) output: Channel<OutputChunk>,
    pub(crate) exit: Channel<SessionExit>,
}

impl std::fmt::Debug for ChannelSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSink")
            .field("output", &self.output.id())
            .field("exit", &self.exit.id())
            .finish()
    }
}

impl OutputSink for ChannelSink {
    fn send_output(&self, batch: Vec<u8>) -> Result<(), SinkClosed> {
        self.output.send(OutputChunk(batch)).map_err(|_| SinkClosed)
    }

    fn send_exit(&self, code: Option<u32>) {
        // The view may already be gone; nothing else can be told.
        let _ = self.exit.send(SessionExit { exit_code: code });
    }
}

#[cfg(test)]
mod tests {
    use tauminus_app::ports::LaunchError;

    use super::*;

    #[test]
    fn handle_round_trips_through_session_id() {
        let handle = SessionHandle::try_from(SessionId::new(7)).unwrap();
        assert_eq!(SessionId::from(handle), SessionId::new(7));
    }

    #[test]
    fn handle_rejects_ids_beyond_u32() {
        let error = SessionHandle::try_from(SessionId::new(u64::from(u32::MAX) + 1)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
    }

    #[test]
    fn grid_size_is_validated() {
        assert!(TerminalSize::try_from(GridSize { cols: 80, rows: 24 }).is_ok());
        let error = TerminalSize::try_from(GridSize { cols: 0, rows: 24 }).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn session_errors_map_to_stable_codes() {
        let cases = [
            (
                SessionError::NotFound(SessionId::new(1)),
                ErrorCode::NotFound,
            ),
            (
                SessionError::LimitReached { max: 64 },
                ErrorCode::LimitReached,
            ),
            (
                SessionError::Launch(LaunchError::Io(std::io::Error::other("boom"))),
                ErrorCode::LaunchFailed,
            ),
        ];
        for (error, code) in cases {
            assert_eq!(CommandError::from(error).code, code);
        }
    }

    #[test]
    fn launch_error_message_includes_the_cause() {
        let error = SessionError::Launch(LaunchError::Spawn {
            program: "/bin/zsh".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
        let message = CommandError::from(error).message;
        assert!(
            message.starts_with("failed to start session: failed to start /bin/zsh"),
            "{message}"
        );
    }

    #[test]
    fn error_serializes_with_snake_case_code() {
        let json = json_body(&CommandError::new(ErrorCode::LimitReached, "x"));
        assert!(json.contains("\"limit_reached\""), "{json}");
    }

    fn json_body(error: &CommandError) -> String {
        IpcResponse::body(error.clone())
            .ok()
            .and_then(|body| match body {
                InvokeResponseBody::Json(json) => Some(json),
                InvokeResponseBody::Raw(_) => None,
            })
            .expect("json body")
    }
}
