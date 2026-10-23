// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files dropped on the window from the OS. The window event carries
//! their paths; the view takes them through a channel it registers, so it
//! needs no permission to listen to app events in general.

use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use specta::Type;
use tauri::State;
use tauri::ipc::Channel;

/// Most paths passed on from one drop.
const MAX_PATHS: usize = 1_000;

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DroppedFiles {
    pub(crate) paths: Vec<String>,
    /// Where they were dropped, in CSS pixels from the window's top left.
    pub(crate) x: f64,
    pub(crate) y: f64,
}

/// The view's channel for drops, the latest one registered.
#[derive(Default)]
pub(crate) struct Drops(Mutex<Option<Channel<DroppedFiles>>>);

impl Drops {
    /// Passes a drop on, if the view is listening.
    pub(crate) fn dropped(&self, paths: &[std::path::PathBuf], x: f64, y: f64) {
        let channel = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(channel) = channel.as_ref() {
            let paths = paths
                .iter()
                .take(MAX_PATHS)
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let _ = channel.send(DroppedFiles { paths, x, y });
        }
    }
}

/// Sends each later drop of files on the window to `on_drop`.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn file_drops(drops: State<'_, Drops>, on_drop: Channel<DroppedFiles>) {
    *drops.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(on_drop);
}
