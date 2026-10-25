// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from cloud accounts (ADR 0032): the provider's CLI lists them,
//! the user sees what an import would do, and accepts it. The listing
//! the user saw is kept here; accepting imports that one, planned again
//! against the hosts as saved then.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::cloud::{Address, CloudInventory, CloudRequest, Listing, Plan, Provider};
use tauminus_app::{HostBookError, ImportReport};
use tauminus_cloud::Clis;
use tauri::State;

use crate::hosts::{Hosts, ImportResult, import_result};
use crate::ipc::{CommandError, ErrorCode};

/// Entries each list of a preview shows; the rest are counted.
const MAX_SHOWN: usize = 500;

/// The provider asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CloudProvider {
    Ec2,
    Lightsail,
    DigitalOcean,
    Azure,
    Gcp,
}

/// Which address a new host takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CloudAddress {
    Public,
    Private,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudAsk {
    pub(crate) provider: CloudProvider,
    /// An AWS profile, a doctl context, an Azure subscription or a GCP
    /// project; none for the CLI's own default.
    pub(crate) scope: Option<String>,
    pub(crate) address: CloudAddress,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudPlanned {
    pub(crate) label: String,
    pub(crate) address: String,
    pub(crate) region: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudMoved {
    pub(crate) label: String,
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudSkipped {
    pub(crate) name: String,
    pub(crate) reason: String,
}

/// What an import would do, for the user to accept by `token`.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudPreview {
    pub(crate) token: u32,
    /// The program that ran, so the user sees which.
    pub(crate) program: String,
    pub(crate) account: String,
    pub(crate) add: Vec<CloudPlanned>,
    pub(crate) moved: Vec<CloudMoved>,
    pub(crate) back: Vec<String>,
    pub(crate) gone: Vec<String>,
    pub(crate) skipped: Vec<CloudSkipped>,
    /// How many of each there are in all, past what is shown.
    pub(crate) counts: CloudCounts,
}

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudCounts {
    pub(crate) add: u32,
    pub(crate) moved: u32,
    pub(crate) back: u32,
    pub(crate) gone: u32,
    pub(crate) skipped: u32,
}

/// What an import did.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloudResult {
    pub(crate) moved: u32,
    pub(crate) back: u32,
    pub(crate) gone: u32,
    pub(crate) result: ImportResult,
}

/// The CLIs, and the listing the user last saw.
pub(crate) struct Cloud {
    pub(crate) clis: Box<dyn CloudInventory>,
    pub(crate) program: Box<dyn Fn(Provider) -> String + Send + Sync>,
    pending: Mutex<Option<(u32, Listing, Address)>>,
    next: std::sync::atomic::AtomicU32,
}

impl Cloud {
    pub(crate) fn new(clis: Clis) -> Self {
        let finder = clis.clone();
        Self::with(
            Box::new(clis),
            Box::new(move |provider| {
                let name = match provider {
                    Provider::Ec2 | Provider::Lightsail => "aws",
                    Provider::DigitalOcean => "doctl",
                    Provider::Azure => "az",
                    Provider::Gcp => "gcloud",
                };
                finder
                    .program(name)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            }),
        )
    }

    pub(crate) fn with(
        clis: Box<dyn CloudInventory>,
        program: Box<dyn Fn(Provider) -> String + Send + Sync>,
    ) -> Self {
        Self {
            clis,
            program,
            pending: Mutex::new(None),
            next: std::sync::atomic::AtomicU32::new(1),
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn preview(
    token: u32,
    program: String,
    listing: &Listing,
    plan: &Plan,
    inventory: &tauminus_domain::Inventory,
) -> CloudPreview {
    let label = |id: &tauminus_domain::HostId| {
        inventory
            .host(id)
            .map(|h| h.label.as_str().to_owned())
            .unwrap_or_default()
    };
    CloudPreview {
        token,
        program,
        account: listing.account.clone(),
        add: plan
            .add
            .iter()
            .take(MAX_SHOWN)
            .map(|p| CloudPlanned {
                label: p.label.as_str().to_owned(),
                address: p.address.to_string(),
                region: p.region.clone(),
            })
            .collect(),
        moved: plan
            .moved
            .iter()
            .take(MAX_SHOWN)
            .map(|(id, to)| CloudMoved {
                label: label(id),
                from: inventory
                    .host(id)
                    .map(|h| h.address.to_string())
                    .unwrap_or_default(),
                to: to.to_string(),
            })
            .collect(),
        back: plan.back.iter().take(MAX_SHOWN).map(label).collect(),
        gone: plan.gone.iter().take(MAX_SHOWN).map(label).collect(),
        skipped: plan
            .skipped
            .iter()
            .take(MAX_SHOWN)
            .map(|(name, reason)| CloudSkipped {
                name: name.clone(),
                reason: reason.clone(),
            })
            .collect(),
        counts: CloudCounts {
            add: count(plan.add.len()),
            moved: count(plan.moved.len()),
            back: count(plan.back.len()),
            gone: count(plan.gone.len()),
            skipped: count(plan.skipped.len()),
        },
    }
}

/// Lists a cloud account through its CLI and shows what an import would
/// do. Nothing is saved.
#[tauri::command]
#[specta::specta]
pub(crate) async fn cloud_preview(
    cloud: State<'_, Cloud>,
    hosts: State<'_, Hosts>,
    ask: CloudAsk,
) -> Result<CloudPreview, CommandError> {
    list(&cloud, hosts.book()?, ask).await
}

/// Imports the listing preview `token` showed.
#[tauri::command]
#[specta::specta]
pub(crate) async fn cloud_import(
    cloud: State<'_, Cloud>,
    hosts: State<'_, Hosts>,
    token: u32,
) -> Result<CloudResult, CommandError> {
    import(&cloud, hosts.book()?, token).await
}

async fn list(
    cloud: &Cloud,
    book: &tauminus_app::HostBook,
    ask: CloudAsk,
) -> Result<CloudPreview, CommandError> {
    let provider = match ask.provider {
        CloudProvider::Ec2 => Provider::Ec2,
        CloudProvider::Lightsail => Provider::Lightsail,
        CloudProvider::DigitalOcean => Provider::DigitalOcean,
        CloudProvider::Azure => Provider::Azure,
        CloudProvider::Gcp => Provider::Gcp,
    };
    let address = match ask.address {
        CloudAddress::Public => Address::Public,
        CloudAddress::Private => Address::Private,
    };
    let request = CloudRequest {
        provider,
        scope: ask.scope.filter(|s| !s.trim().is_empty()),
    };
    let listing = cloud
        .clis
        .list(&request)
        .await
        .map_err(|e| CommandError::new(ErrorCode::ConnectFailed, e))?;
    let plan = book.cloud_plan(&listing, address).await;
    let token = cloud
        .next
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let shown = preview(
        token,
        (cloud.program)(provider),
        &listing,
        &plan,
        &book.snapshot().await,
    );
    *cloud
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((token, listing, address));
    Ok(shown)
}

async fn import(
    cloud: &Cloud,
    book: &tauminus_app::HostBook,
    token: u32,
) -> Result<CloudResult, CommandError> {
    let pending = cloud
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take_if(|(shown, _, _)| *shown == token);
    let Some((_, listing, address)) = pending else {
        return Err(CommandError::new(
            ErrorCode::NotFound,
            "that list is gone: list the account again",
        ));
    };
    let (report, inventory) = book
        .import_cloud(&listing, address)
        .await
        .map_err(|e: HostBookError| CommandError::from(e))?;
    Ok(CloudResult {
        moved: count(report.moved),
        back: count(report.back),
        gone: count(report.gone),
        result: import_result(
            ImportReport {
                added: report.added,
                skipped: report.skipped,
            },
            &inventory,
        ),
    })
}

#[cfg(test)]
mod tests;
