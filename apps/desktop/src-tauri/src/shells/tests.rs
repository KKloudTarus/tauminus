// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A catalog whose listings are `listings`, oldest first, and how many it ran.
fn catalog(listings: Vec<ShellListing>) -> (ShellCatalog, Arc<AtomicUsize>) {
    let queue = Mutex::new(listings.into_iter());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let catalog = ShellCatalog::new(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next()
            .expect("a listing for each call")
    }));
    (catalog, calls)
}

fn shell(id: &str, name: &str) -> KnownShell {
    KnownShell {
        id: id.to_owned(),
        name: name.to_owned(),
        shell: LocalShell {
            program: Some(format!(r"C:\shells\{name}.exe")),
            ..LocalShell::default()
        },
    }
}

fn listing(shells: &[KnownShell], wsl_answered: bool) -> ShellListing {
    ShellListing {
        shells: shells.to_vec(),
        wsl_answered,
    }
}

#[test]
fn finds_a_listed_shell_and_refuses_an_unknown_id_after_one_more_look() {
    let pwsh = shell("pwsh", "PowerShell 7");
    let (catalog, calls) = catalog(vec![
        listing(std::slice::from_ref(&pwsh), true),
        listing(std::slice::from_ref(&pwsh), true),
    ]);
    assert_eq!(catalog.find("pwsh"), Some(pwsh.shell.clone()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(catalog.find("pwsh"), Some(pwsh.shell));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(catalog.find(r"C:\Windows\System32\calc.exe"), None);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn keeps_the_distributions_it_had_when_wsl_is_slow() {
    let cmd = shell("cmd", "Command Prompt");
    let ubuntu = shell("wsl:Ubuntu", "Ubuntu (WSL)");
    let (catalog, _) = catalog(vec![
        listing(&[cmd.clone(), ubuntu.clone()], true),
        listing(std::slice::from_ref(&cmd), false),
    ]);
    assert!(catalog.refresh().1);
    let (shells, complete) = catalog.refresh();
    assert!(!complete);
    assert_eq!(shells, [cmd, ubuntu.clone()]);
    assert_eq!(catalog.find("wsl:Ubuntu"), Some(ubuntu.shell));
}

#[test]
fn looks_again_for_a_distribution_a_slow_first_listing_missed() {
    let cmd = shell("cmd", "Command Prompt");
    let ubuntu = shell("wsl:Ubuntu", "Ubuntu (WSL)");
    let (catalog, calls) = catalog(vec![
        listing(std::slice::from_ref(&cmd), false),
        listing(&[cmd, ubuntu.clone()], true),
    ]);
    assert!(!catalog.refresh().1);
    assert_eq!(catalog.find("wsl:Ubuntu"), Some(ubuntu.shell));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn leaves_out_a_name_that_reorders_text() {
    let cmd = shell("cmd", "Command Prompt");
    let spoof = shell("wsl:a", "\u{202E}xue (WSL)");
    let (catalog, _) = catalog(vec![listing(&[cmd.clone(), spoof], true)]);
    assert_eq!(catalog.refresh().0, [cmd]);
}
