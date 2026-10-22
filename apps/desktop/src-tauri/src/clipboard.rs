// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The OS clipboard, as text. WebKitGTK refuses `navigator.clipboard` reads,
//! so paste reached no terminal on Linux; the backend reads and writes it on
//! every platform instead.

use std::sync::{Arc, Mutex};

use tauri::State;

use crate::ipc::{CommandError, ErrorCode};

/// Longest text a paste takes, in bytes. The terminal sends a paste in one
/// write; a larger one is a mistake, not a command.
pub(crate) const MAX_TEXT: usize = 8 * 1024 * 1024;

/// One clipboard for the app's lifetime. On X11 and Wayland the copied text
/// lives as long as the clipboard that set it, so it must not be dropped
/// after each copy.
#[derive(Default)]
pub(crate) struct Clipboard(Arc<Mutex<Option<arboard::Clipboard>>>);

impl Clipboard {
    /// Runs `use_it` on the clipboard, opening it on first use, off the
    /// async workers: X11 answers through a round trip to the server.
    async fn with<T: Send + 'static>(
        &self,
        use_it: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error> + Send + 'static,
    ) -> Result<T, CommandError> {
        let shared = Arc::clone(&self.0);
        tauri::async_runtime::spawn_blocking(move || {
            // A panic inside arboard leaves nothing half-written here.
            let mut slot = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(arboard::Clipboard::new().map_err(|e| internal(&e.to_string()))?);
            }
            let clipboard = slot.as_mut().ok_or_else(|| internal("no clipboard"))?;
            let result = use_it(clipboard);
            // A failed clipboard, such as one whose X connection dropped,
            // is opened again on the next use.
            if result.is_err() {
                *slot = None;
            }
            result.map_err(|e| internal(&e.to_string()))
        })
        .await
        .map_err(|e| internal(&e.to_string()))?
    }
}

fn internal(message: &str) -> CommandError {
    CommandError::new(ErrorCode::Internal, format!("clipboard: {message}"))
}

/// What a read returns to the view: empty for a clipboard with no text, and
/// an error for text over `MAX_TEXT`.
pub(crate) fn readable(read: Result<String, arboard::Error>) -> Result<String, arboard::Error> {
    match read {
        Ok(text) if text.len() > MAX_TEXT => Err(arboard::Error::Unknown {
            description: format!("the text is over {} MiB", MAX_TEXT / 1024 / 1024),
        }),
        Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
        other => other,
    }
}

/// The clipboard's text, or "" when it holds none.
#[tauri::command]
#[specta::specta]
pub(crate) async fn clipboard_read_text(
    clipboard: State<'_, Clipboard>,
) -> Result<String, CommandError> {
    clipboard.with(|c| readable(c.get_text())).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn clipboard_write_text(
    clipboard: State<'_, Clipboard>,
    text: String,
) -> Result<(), CommandError> {
    if text.len() > MAX_TEXT {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "clipboard: the text is over 8 MiB",
        ));
    }
    clipboard.with(move |c| c.set_text(text)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_clipboard_reads_as_no_text() {
        assert_eq!(
            readable(Err(arboard::Error::ContentNotAvailable)).unwrap(),
            ""
        );
        assert_eq!(readable(Ok("ls\n".into())).unwrap(), "ls\n");
    }

    #[test]
    fn oversized_text_and_failures_are_errors() {
        assert!(readable(Ok("x".repeat(MAX_TEXT + 1))).is_err());
        assert!(readable(Err(arboard::Error::ClipboardOccupied)).is_err());
    }
}
