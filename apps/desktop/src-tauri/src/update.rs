// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Updates (ADR 0033): the release's `latest.json`, over HTTPS, names a
//! newer version and its bundle; the bundle installs only when its
//! minisign signature checks against the key in `tauri.conf.json`, and
//! only when the user asks. The webview reaches the updater only through
//! these two commands.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use specta::Type;
use tauri::State;
use tauri::ipc::Channel;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::ipc::{CommandError, ErrorCode};

/// Characters of release notes shown; the release page has the rest.
const MAX_NOTES: usize = 4000;
/// Characters of a version or date shown.
const MAX_FIELD: usize = 64;
/// How long asking for `latest.json` may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a download may go without a byte before it is given up.
const STALL: Duration = Duration::from_secs(60);
/// How often the download is looked at for a stall.
const WATCH: Duration = Duration::from_secs(5);

/// The update the user was last told of, kept for them to install.
#[derive(Default)]
pub(crate) struct Updates(Mutex<Option<Update>>);

/// A newer version, as the user sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateView {
    pub(crate) version: String,
    pub(crate) current: String,
    pub(crate) date: Option<String>,
    pub(crate) notes: String,
}

/// Text from the release, which its author wrote: control characters out
/// but line ends, cut to `max` characters.
fn shown(text: &str, max: usize) -> String {
    let mut out: String = text
        .chars()
        .filter(|&c| c == '\n' || !(c.is_control() || crate::hosts::is_bidi_control(c)))
        .take(max + 1)
        .collect();
    if out.chars().count() > max {
        out.pop();
        out.push('…');
    }
    out
}

/// Whether the bundle comes from where the app's own releases are: a
/// `latest.json` someone edited cannot point the download elsewhere.
/// Debug builds may point at a local server to test with.
fn trusted_source(url: &tauri::Url) -> bool {
    cfg!(debug_assertions) || (url.scheme() == "https" && url.host_str() == Some("github.com"))
}

fn view(update: &Update) -> UpdateView {
    UpdateView {
        version: shown(&update.version, MAX_FIELD),
        current: shown(&update.current_version, MAX_FIELD),
        date: update.date.map(|d| shown(&d.to_string(), MAX_FIELD)),
        notes: shown(update.body.as_deref().unwrap_or_default(), MAX_NOTES),
    }
}

fn failed(error: &tauri_plugin_updater::Error) -> CommandError {
    CommandError::new(ErrorCode::ConnectFailed, error.to_string())
}

/// Asks the release for a newer version. `None` when this one is the
/// newest.
#[tauri::command]
#[specta::specta]
pub(crate) async fn update_check<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    updates: State<'_, Updates>,
) -> Result<Option<UpdateView>, CommandError> {
    let found = app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .map_err(|e| failed(&e))?
        .check()
        .await
        .map_err(|e| failed(&e))?;
    if let Some(update) = &found
        && !trusted_source(&update.download_url)
    {
        return Err(CommandError::new(
            ErrorCode::ConnectFailed,
            format!(
                "the update names a download outside the project's releases: {}",
                shown(update.download_url.as_str(), 200)
            ),
        ));
    }
    let shown = found.as_ref().map(view);
    *updates
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = found;
    Ok(shown)
}

/// Downloads the update last found, checks its signature, installs it,
/// and starts the new version. Windows' installer ends this process
/// itself.
#[tauri::command]
#[specta::specta]
pub(crate) async fn update_install<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    updates: State<'_, Updates>,
    on_progress: Channel<u8>,
) -> Result<(), CommandError> {
    let found = updates
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(update) = found else {
        return Err(CommandError::new(
            ErrorCode::NotFound,
            "no update to install: check again",
        ));
    };
    tracing::info!(version = %update.version, "installing an update");
    let (update, bytes) = download(update, on_progress).await?;
    // A `.deb` or `.rpm` waits on a password dialog: off the async workers.
    tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| failed(&e))?;
    app.restart();
}

/// Downloads `update`, telling `progress` each whole percent; given up
/// once no byte has come for [`STALL`], however long the whole takes.
async fn download(
    update: Update,
    progress: Channel<u8>,
) -> Result<(Update, Vec<u8>), CommandError> {
    let started = Instant::now();
    let elapsed = |at: Instant| u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Milliseconds after `started` that the last bytes came.
    let last = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&last);
    let task = tauri::async_runtime::spawn(async move {
        let (mut got, mut told) = (0u64, 0u8);
        // The signature is checked on these bytes, and these bytes install.
        let bytes = update
            .download(
                |chunk, total| {
                    seen.store(elapsed(started), Ordering::Relaxed);
                    got += chunk as u64;
                    if let Some(total) = total.filter(|t| *t > 0) {
                        let percent = u8::try_from((got * 100 / total).min(100)).unwrap_or(100);
                        if percent > told {
                            told = percent;
                            let _ = progress.send(percent);
                        }
                    }
                },
                || {},
            )
            .await;
        bytes.map(|bytes| (update, bytes))
    });
    loop {
        tokio::time::sleep(WATCH).await;
        if task.inner().is_finished() {
            break;
        }
        let quiet = elapsed(started).saturating_sub(last.load(Ordering::Relaxed));
        if quiet > u64::try_from(STALL.as_millis()).unwrap_or(u64::MAX) {
            task.abort();
            return Err(CommandError::new(
                ErrorCode::ConnectFailed,
                format!("the download stopped: no data for {} s", STALL.as_secs()),
            ));
        }
    }
    task.await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| failed(&e))
}

#[cfg(test)]
mod tests {
    use super::shown;

    #[test]
    fn release_text_keeps_its_lines_and_loses_what_could_mislead() {
        assert_eq!(shown("fix\n\u{1b}[31mred\u{202e}x", 100), "fix\n[31mredx");
        assert_eq!(shown("abcdef", 3), "abc…");
        assert_eq!(shown("abc", 3), "abc");
        // Only what is kept counts toward the cut.
        assert_eq!(shown("abc\u{1b}", 3), "abc");
    }

    #[test]
    fn a_download_comes_from_the_project_s_releases() {
        let url = |s: &str| tauri::Url::parse(s).unwrap();
        assert!(super::trusted_source(&url(
            "https://github.com/KKloudTarus/tauminus/releases/download/v1/x.AppImage"
        )));
        // Debug builds test against a local server; release builds never.
        let local = super::trusted_source(&url("http://127.0.0.1:8099/x"));
        assert_eq!(local, cfg!(debug_assertions));
    }
}
