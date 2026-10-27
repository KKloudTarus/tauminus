// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Secrets a password manager holds (ADR 0035), as the view names them,
//! and the user's own yes before a new one is read. The question comes
//! from the backend in a native dialog: a webview that is not the user's
//! cannot answer it.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::BoxFuture;
use tauminus_app::vault::SecretRefs;
use tauminus_domain::{SecretRef, SecretSlot};

/// Where a password manager holds one secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "manager", rename_all = "camelCase")]
pub(crate) enum SecretRefSpec {
    /// `op://vault/item/[section/]field`.
    OnePassword { reference: String },
    /// A Bitwarden item's id.
    Bitwarden { item: String },
    /// A field in HashiCorp Vault's KV engine.
    Vault {
        address: String,
        mount: String,
        path: String,
        field: String,
    },
}

/// An identity's references, by slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SecretRefsSpec {
    #[serde(default)]
    pub(crate) password: Option<SecretRefSpec>,
    #[serde(default)]
    pub(crate) private_key: Option<SecretRefSpec>,
    #[serde(default)]
    pub(crate) passphrase: Option<SecretRefSpec>,
}

impl From<SecretRefSpec> for SecretRef {
    fn from(spec: SecretRefSpec) -> Self {
        match spec {
            SecretRefSpec::OnePassword { reference } => Self::OnePassword { reference },
            SecretRefSpec::Bitwarden { item } => Self::Bitwarden { item },
            SecretRefSpec::Vault {
                address,
                mount,
                path,
                field,
            } => Self::Vault {
                address,
                mount,
                path,
                field,
            },
        }
    }
}

impl From<SecretRef> for SecretRefSpec {
    fn from(reference: SecretRef) -> Self {
        match reference {
            SecretRef::OnePassword { reference } => Self::OnePassword { reference },
            SecretRef::Bitwarden { item } => Self::Bitwarden { item },
            SecretRef::Vault {
                address,
                mount,
                path,
                field,
            } => Self::Vault {
                address,
                mount,
                path,
                field,
            },
        }
    }
}

impl From<SecretRefsSpec> for SecretRefs {
    fn from(spec: SecretRefsSpec) -> Self {
        Self {
            password: spec.password.map(Into::into),
            private_key: spec.private_key.map(Into::into),
            passphrase: spec.passphrase.map(Into::into),
        }
    }
}

impl From<SecretRefs> for SecretRefsSpec {
    fn from(refs: SecretRefs) -> Self {
        Self {
            password: refs.password.map(Into::into),
            private_key: refs.private_key.map(Into::into),
            passphrase: refs.passphrase.map(Into::into),
        }
    }
}

/// Asks the user before the vault reads a reference it has not read.
pub(crate) trait ReferenceConsent: Send + Sync + 'static {
    fn approve(&self, question: String) -> BoxFuture<'_, bool>;
}

pub(crate) type Consent = Arc<dyn ReferenceConsent>;

/// The system's own dialog.
pub(crate) struct DialogConsent<R: tauri::Runtime>(pub(crate) tauri::AppHandle<R>);

impl<R: tauri::Runtime> ReferenceConsent for DialogConsent<R> {
    fn approve(&self, question: String) -> BoxFuture<'_, bool> {
        let app = self.0.clone();
        Box::pin(async move {
            tauri::async_runtime::spawn_blocking(move || {
                use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
                app.dialog()
                    .message(question)
                    .title("Read from a password manager")
                    .kind(MessageDialogKind::Warning)
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "Allow".into(),
                        "Cancel".into(),
                    ))
                    .blocking_show()
            })
            .await
            .unwrap_or(false)
        })
    }
}

/// The references `next` sets that `before` did not hold, by slot.
pub(crate) fn added<'a>(
    before: &SecretRefs,
    next: &'a SecretRefs,
) -> Vec<(SecretSlot, &'a SecretRef)> {
    [
        SecretSlot::Password,
        SecretSlot::PrivateKey,
        SecretSlot::Passphrase,
    ]
    .into_iter()
    .filter_map(|slot| {
        let reference = next.get(slot)?;
        (before.get(slot) != Some(reference)).then_some((slot, reference))
    })
    .collect()
}

/// What the dialog asks: which identity, and each reference in full.
pub(crate) fn question(identity: &str, added: &[(SecretSlot, &SecretRef)]) -> String {
    let mut text = format!(
        "Tauminus will read these from your password manager each time it signs in as \u{201c}{identity}\u{201d}:\n"
    );
    for (slot, reference) in added {
        let slot = match slot {
            SecretSlot::Password => "password",
            SecretSlot::PrivateKey => "private key",
            SecretSlot::Passphrase => "key passphrase",
        };
        let named = match reference {
            SecretRef::OnePassword { reference } => format!("1Password {reference}"),
            SecretRef::Bitwarden { item } => format!("Bitwarden item {item}"),
            // The Vault CLI sends the user's token to the address named.
            SecretRef::Vault {
                address,
                mount,
                path,
                field,
            } => format!(
                "Vault {mount}/{path}, field {field}, read by sending your Vault sign-in token to {address}"
            ),
        };
        let _ = write!(text, "\n\u{2022} {slot}: {named}");
    }
    text.push_str("\n\nAllow it only if you set this up yourself.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_names_each_new_reference_and_nothing_kept() {
        let kept = SecretRef::Bitwarden {
            item: "3f2504e0-4f89-11d3-9a0c-0305e82c3301".into(),
        };
        let before = SecretRefs {
            passphrase: Some(kept.clone()),
            ..SecretRefs::default()
        };
        let next = SecretRefs {
            password: Some(SecretRef::OnePassword {
                reference: "op://Ops/db/password".into(),
            }),
            passphrase: Some(kept),
            ..SecretRefs::default()
        };
        let added = added(&before, &next);
        assert_eq!(added.len(), 1);
        let text = question("prod db", &added);
        assert!(text.contains("\u{201c}prod db\u{201d}"), "{text}");
        assert!(text.contains("\u{2022} password: 1Password op://Ops/db/password"));
        assert!(!text.contains("Bitwarden"));
        assert_eq!(super::added(&next, &next), []);
    }

    #[test]
    fn a_vault_reference_says_where_the_token_goes() {
        let vault = SecretRef::Vault {
            address: "https://vault.example.com".into(),
            mount: "secret".into(),
            path: "ops/db".into(),
            field: "password".into(),
        };
        let text = question("db", &[(SecretSlot::Password, &vault)]);
        assert!(
            text.contains("sending your Vault sign-in token to https://vault.example.com"),
            "{text}"
        );
    }
}
