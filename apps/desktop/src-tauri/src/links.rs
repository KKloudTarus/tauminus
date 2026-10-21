// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opening links from terminal output in the default browser. Output is
//! whatever a remote program printed, so only plain web links pass.

use crate::ipc::{CommandError, ErrorCode};

/// Longest link opened. Browsers take more, but a terminal link that long is
/// not one a person meant to click.
const MAX_URL: usize = 2048;

/// `url` if it is an `http` or `https` link with a host and only printable
/// ASCII: no spaces, controls, or lookalike characters that could make it
/// read as a different site than it opens. O(n).
pub(crate) fn checked_link(url: &str) -> Result<&str, CommandError> {
    let refused = |why: &str| {
        CommandError::new(
            ErrorCode::InvalidArgument,
            format!("not opening this link: {why}"),
        )
    };
    if url.len() > MAX_URL {
        return Err(refused("too long"));
    }
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return Err(refused("only http and https links open"));
    };
    if rest.is_empty() || rest.starts_with(['/', '?', '#']) {
        return Err(refused("it names no host"));
    }
    if !url.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(refused(
            "it holds spaces, controls, or non-ASCII characters",
        ));
    }
    Ok(url)
}

/// Opens an `http` or `https` link in the default browser.
#[tauri::command]
#[specta::specta]
pub(crate) async fn open_link(url: String) -> Result<(), CommandError> {
    checked_link(&url)?;
    // The opener returns once the browser has been asked; it does not wait
    // for the page.
    tauri::async_runtime::spawn_blocking(move || open::that_detached(&url))
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| {
            CommandError::new(ErrorCode::Internal, format!("could not open the link: {e}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_web_links_pass() {
        for url in [
            "https://example.com",
            "http://example.com/path?q=1#top",
            "HTTPS://Example.COM/a%20b",
            "https://[2001:db8::1]:8443/",
        ] {
            assert_eq!(checked_link(url).map_err(|e| e.message), Ok(url), "{url}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ssh://host",
            "https://",
            "https:///path",
            "https://exa mple.com",
            "https://example.com/\u{7}",
            "https://exаmple.com",
            "-https://example.com",
            &format!("https://example.com/{}", "a".repeat(MAX_URL)),
        ] {
            let error = checked_link(url).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{url:?}");
        }
    }
}
