// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync over IPC (ADR 0030). The passphrase goes in and is wiped; the key
//! and a remote's address stay in the vault, and the view sees the address
//! without any token in it. A host another device changed waits for the
//! user: every use checks [`Gate`] after reading the host.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::{Secret, SyncError, SyncTarget};
use tauminus_app::sync::{Report, Setup, Status};
use tauminus_app::trust::{Fingerprint, Review, Trust, TrustError};
use tauminus_app::{SnippetBook, SyncRunError, SyncService, SyncServiceError};
use tauminus_domain::{HostId, Inventory, Library};
use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::Notify;
use zeroize::Zeroize as _;

use crate::hosts::Hosts;
use crate::ipc::{CommandError, ErrorCode};

impl From<SyncError> for CommandError {
    fn from(error: SyncError) -> Self {
        let code = match &error {
            SyncError::Target(_) => ErrorCode::ConnectFailed,
            SyncError::Sealed(_) => ErrorCode::WrongPassword,
            SyncError::Newer(_) => ErrorCode::Unsupported,
            SyncError::Invalid(_) => ErrorCode::StoreFailed,
        };
        Self::new(code, error.to_string())
    }
}

impl From<SyncServiceError> for CommandError {
    fn from(error: SyncServiceError) -> Self {
        match error {
            SyncServiceError::Vault(error) => error.into(),
            SyncServiceError::Sync(error) | SyncServiceError::Run(SyncRunError::Sync(error)) => {
                error.into()
            }
            SyncServiceError::Off => Self::new(ErrorCode::NotFound, error.to_string()),
            SyncServiceError::On => Self::new(ErrorCode::InvalidArgument, error.to_string()),
            SyncServiceError::Running | SyncServiceError::Run(SyncRunError::Busy) => {
                Self::new(ErrorCode::Busy, error.to_string())
            }
            SyncServiceError::Run(error) => Self::new(ErrorCode::StoreFailed, error.to_string()),
        }
    }
}

impl From<TrustError> for CommandError {
    fn from(error: TrustError) -> Self {
        let code = match &error {
            TrustError::NeedsReview(_) => ErrorCode::NeedsReview,
            TrustError::Hosts(_) => ErrorCode::NotFound,
            TrustError::Store(_) => ErrorCode::StoreFailed,
        };
        Self::new(code, error.to_string())
    }
}

/// Checks a saved host against the sync review list before it is used.
#[derive(Clone)]
pub(crate) struct Gate {
    /// The list unreadable: no saved host is used.
    trust: Result<Arc<Trust>, String>,
    snippets: Option<Arc<SnippetBook>>,
}

impl Gate {
    pub(crate) fn new(
        trust: Result<Arc<Trust>, String>,
        snippets: Option<Arc<SnippetBook>>,
    ) -> Self {
        Self { trust, snippets }
    }

    fn trust(&self) -> Result<&Arc<Trust>, CommandError> {
        self.trust
            .as_ref()
            .map_err(|reason| CommandError::new(ErrorCode::StoreFailed, reason.clone()))
    }

    /// Whether a running session on host `id` may follow its settings as
    /// saved now; `false` on any doubt, for the caller to take the safe
    /// value instead.
    pub(crate) async fn passes(&self, hosts: &Hosts, id: &HostId) -> bool {
        self.check(hosts, id).await.is_ok()
    }

    /// What a local edit's rebase reads of the other book, taken before
    /// the edit: a sync saving there after it then leaves the hosts it
    /// touched held. `None` without the review list.
    pub(crate) async fn library_before(&self) -> Option<Library> {
        self.trust.as_ref().ok()?;
        Some(match &self.snippets {
            Some(book) => book.snapshot().await,
            None => Library::default(),
        })
    }

    /// As [`Gate::library_before`], for a snippet edit.
    pub(crate) async fn inventory_before(&self, hosts: &Hosts) -> Option<Inventory> {
        self.trust.as_ref().ok()?;
        Some(hosts.book().ok()?.snapshot().await)
    }

    /// A local edit of the hosts, `before` read under the book's lock and
    /// `library` before the edit: hosts the user accepted stay accepted
    /// as edited.
    pub(crate) async fn hosts_edited(
        &self,
        library: Option<Library>,
        before: &Inventory,
        after: &Inventory,
    ) {
        let (Ok(trust), Some(library)) = (&self.trust, library) else {
            return;
        };
        if let Err(error) = trust.rebase((before, &library), (after, &library)).await {
            // The hosts stay held, which is safe; the user accepts again.
            tracing::warn!(%error, "accepted hosts were not moved along with an edit");
        }
    }

    /// A local edit of the snippets, as [`Gate::hosts_edited`].
    pub(crate) async fn snippets_edited(
        &self,
        inventory: Option<Inventory>,
        before: &Library,
        after: &Library,
    ) {
        let (Ok(trust), Some(inventory)) = (&self.trust, inventory) else {
            return;
        };
        if let Err(error) = trust
            .rebase((&inventory, before), (&inventory, after))
            .await
        {
            tracing::warn!(%error, "accepted hosts were not moved along with an edit");
        }
    }

    /// Call it after reading what the use acts on.
    pub(crate) async fn check(&self, hosts: &Hosts, id: &HostId) -> Result<(), CommandError> {
        Ok(self
            .trust()?
            .check(hosts.book()?, self.snippets.as_deref(), id)
            .await?)
    }
}

/// Sync, when the saved lists and the review list could be read; and
/// where its runs are told.
pub(crate) struct SyncState {
    service: Result<Arc<SyncService>, String>,
    pub(crate) kick: Arc<Notify>,
    pub(crate) channel: Arc<Mutex<Option<Channel<SyncStatusView>>>>,
}

impl SyncState {
    pub(crate) fn new(service: Result<Arc<SyncService>, String>) -> Self {
        Self {
            service,
            kick: Arc::new(Notify::new()),
            channel: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn service(&self) -> Result<&Arc<SyncService>, CommandError> {
        self.service
            .as_ref()
            .map_err(|reason| CommandError::new(ErrorCode::StoreFailed, reason.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum SetupSpec {
    Locked,
    Off,
    On { git: bool, shown: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClashView {
    pub(crate) record: String,
    pub(crate) field: String,
    pub(crate) kept: String,
    pub(crate) lost: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportView {
    /// Milliseconds since the Unix epoch.
    pub(crate) at: f64,
    /// The saved hosts or snippets changed while it ran.
    pub(crate) changed: bool,
    pub(crate) error: Option<String>,
    pub(crate) clashes: Vec<ClashView>,
    pub(crate) repairs: Vec<String>,
    pub(crate) notices: Vec<String>,
}

impl From<Report> for ReportView {
    fn from(report: Report) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "ms fit in f64 until year 287396"
        )]
        let at = report
            .at
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_millis() as f64);
        match report.outcome {
            Ok(summary) => Self {
                at,
                changed: report.changed,
                error: None,
                clashes: summary
                    .clashes
                    .into_iter()
                    .map(|c| ClashView {
                        record: c.record,
                        field: c.field,
                        kept: c.kept,
                        lost: c.lost,
                    })
                    .collect(),
                repairs: summary.repairs,
                notices: summary.notices,
            },
            Err(message) => Self {
                at,
                changed: report.changed,
                error: Some(message),
                clashes: Vec::new(),
                repairs: Vec::new(),
                notices: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncStatusView {
    pub(crate) setup: SetupSpec,
    pub(crate) running: bool,
    pub(crate) last: Option<ReportView>,
}

impl From<Status> for SyncStatusView {
    fn from(status: Status) -> Self {
        Self {
            setup: match status.setup {
                Setup::Locked => SetupSpec::Locked,
                Setup::Off => SetupSpec::Off,
                Setup::On { git, shown } => SetupSpec::On { git, shown },
            },
            running: status.running,
            last: status.last.map(ReportView::from),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TargetKind {
    Folder,
    Git,
}

/// Where to sync, as the user typed or chose it.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TargetInput {
    pub(crate) kind: TargetKind,
    pub(crate) address: String,
}

impl TargetInput {
    /// Wipes the address as it came: a remote's may hold a token.
    fn into_target(mut self) -> SyncTarget {
        let address = self.address.trim().to_owned();
        self.address.zeroize();
        match self.kind {
            TargetKind::Folder => SyncTarget::Folder(address),
            TargetKind::Git => SyncTarget::Git(Secret::new(address)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExposedView {
    /// 0 for the host, then each group up to the top.
    pub(crate) level: u32,
    pub(crate) place: String,
    pub(crate) field: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewView {
    pub(crate) host: String,
    pub(crate) label: String,
    pub(crate) new: bool,
    pub(crate) lines: Vec<ExposedView>,
    pub(crate) startup: String,
    /// Sent back to accept exactly what was shown.
    pub(crate) fingerprint: String,
}

impl From<Review> for ReviewView {
    fn from(review: Review) -> Self {
        Self {
            host: review.host.to_string(),
            label: review.label,
            new: review.new,
            lines: review
                .lines
                .into_iter()
                .map(|line| ExposedView {
                    level: u32::try_from(line.level).unwrap_or(u32::MAX),
                    place: line.place,
                    field: line.field.to_owned(),
                    value: line.value,
                })
                .collect(),
            startup: review.startup,
            fingerprint: review.fingerprint.0.iter().fold(
                String::with_capacity(64),
                |mut hex, b| {
                    let _ = write!(hex, "{b:02x}");
                    hex
                },
            ),
        }
    }
}

fn fingerprint(text: &str) -> Result<Fingerprint, CommandError> {
    let invalid = || CommandError::new(ErrorCode::InvalidArgument, "not a fingerprint");
    if text.len() != 64 || !text.is_ascii() {
        return Err(invalid());
    }
    let mut out = [0; 32];
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    for (byte, pair) in out.iter_mut().zip(pairs) {
        let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
    }
    Ok(Fingerprint(out))
}

fn host_id(id: &str) -> Result<HostId, CommandError> {
    HostId::parse(id).map_err(|e| CommandError::new(ErrorCode::InvalidArgument, e.to_string()))
}

/// Takes the passphrase into a wiped buffer.
fn passphrase(mut text: String) -> Secret {
    let secret = Secret::new(text.clone());
    text.zeroize();
    secret
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_status(
    sync: State<'_, SyncState>,
) -> Result<SyncStatusView, CommandError> {
    Ok(sync.service()?.status().await.into())
}

/// Where each scheduled run's status goes; called once.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn sync_listen(sync: State<'_, SyncState>, on_status: Channel<SyncStatusView>) {
    *sync.channel.lock().unwrap_or_else(PoisonError::into_inner) = Some(on_status);
}

/// Starts sync on an empty folder or remote, with a new key wrapped there
/// under `passphrase`.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_create(
    sync: State<'_, SyncState>,
    target: TargetInput,
    passphrase: String,
) -> Result<SyncStatusView, CommandError> {
    let secret = self::passphrase(passphrase);
    let service = sync.service()?;
    service.create(target.into_target(), secret).await?;
    sync.kick.notify_one();
    Ok(service.status().await.into())
}

/// Joins the sync a folder or remote holds, with its passphrase.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_join(
    sync: State<'_, SyncState>,
    target: TargetInput,
    passphrase: String,
) -> Result<SyncStatusView, CommandError> {
    let secret = self::passphrase(passphrase);
    let service = sync.service()?;
    service.join(target.into_target(), secret).await?;
    sync.kick.notify_one();
    Ok(service.status().await.into())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_now(sync: State<'_, SyncState>) -> Result<SyncStatusView, CommandError> {
    let service = sync.service()?;
    service.run().await?;
    Ok(service.status().await.into())
}

/// Stops syncing on this device. The target and the other devices keep
/// everything.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_leave(sync: State<'_, SyncState>) -> Result<SyncStatusView, CommandError> {
    let service = sync.service()?;
    service.leave().await?;
    Ok(service.status().await.into())
}

/// Hosts shown at once: each review carries its settings and startup
/// text, and a device that just joined may have thousands.
pub(crate) const REVIEW_PAGE: usize = 20;
/// Most hosts accepted in one call.
const MAX_ACCEPT: usize = 1000;

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewsView {
    /// The first hosts waiting, at most a page.
    pub(crate) reviews: Vec<ReviewView>,
    /// Hosts noted in all; a few may turn out approved at the next sync.
    pub(crate) noted: u32,
}

/// A host as the user saw it.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SeenInput {
    pub(crate) host: String,
    pub(crate) fingerprint: String,
}

/// The first page of hosts another device changed, waiting for the user.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_reviews(
    gate: State<'_, Gate>,
    hosts: State<'_, Hosts>,
) -> Result<ReviewsView, CommandError> {
    let pending = gate
        .trust()?
        .pending(hosts.book()?, gate.snippets.as_deref(), REVIEW_PAGE)
        .await?;
    Ok(ReviewsView {
        reviews: pending.reviews.into_iter().map(ReviewView::from).collect(),
        noted: u32::try_from(pending.noted).unwrap_or(u32::MAX),
    })
}

/// Lets each host be used as the user saw it. Returns the hosts that
/// changed since they were shown; those still wait.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_review_accept(
    gate: State<'_, Gate>,
    hosts: State<'_, Hosts>,
    seen: Vec<SeenInput>,
) -> Result<Vec<String>, CommandError> {
    if seen.len() > MAX_ACCEPT {
        return Err(CommandError::new(
            ErrorCode::LimitReached,
            format!("at most {MAX_ACCEPT} hosts at once"),
        ));
    }
    let seen = seen
        .iter()
        .map(|s| Ok((host_id(&s.host)?, fingerprint(&s.fingerprint)?)))
        .collect::<Result<Vec<_>, CommandError>>()?;
    let stale = gate
        .trust()?
        .accept(hosts.book()?, gate.snippets.as_deref(), &seen)
        .await?;
    Ok(stale.iter().map(ToString::to_string).collect())
}

/// Opens the system's folder dialog; `None` when nothing was chosen.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_choose_folder<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<String>, CommandError> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .file()
            .set_title("Sync folder")
            .blocking_pick_folder()
    })
    .await
    .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(picked
        .and_then(|path| path.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned()))
}

/// Runs syncs on their schedule for the app's life, telling the view.
pub(crate) fn schedule(state: &SyncState) {
    let Ok(service) = state.service.as_ref() else {
        return;
    };
    let channel = Arc::clone(&state.channel);
    let kick = Arc::clone(&state.kick);
    tauri::async_runtime::spawn(Arc::clone(service).keep_in_sync(kick, move |status| {
        if let Some(channel) = channel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            // The view may be gone; it asks again when it comes back.
            let _ = channel.send(SyncStatusView::from(status));
        }
    }));
}

#[cfg(test)]
mod tests;
