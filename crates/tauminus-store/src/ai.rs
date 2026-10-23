// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The AI assistant's settings file, version 1. It holds no secret: the
//! key is in the vault. As with hosts, an older build refuses a newer file.

use serde::{Deserialize, Serialize};
use tauminus_app::AiSettings;
use tauminus_domain::AiEndpoint;

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    local_output: bool,
    /// The origin the vault's key was saved for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_origin: Option<String>,
}

pub(crate) fn to_json(settings: &AiSettings) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&File {
        version: VERSION,
        enabled: settings.enabled,
        base_url: settings.endpoint.as_ref().map(AiEndpoint::base_url),
        model: settings.endpoint.as_ref().map(|e| e.model().to_owned()),
        local_output: settings.local_output,
        key_origin: settings.key_origin.clone(),
    })
}

pub(crate) fn from_json(bytes: &[u8]) -> Result<AiSettings, String> {
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "it is version {}, and this build reads version {VERSION}; update Tauminus",
            file.version
        ));
    }
    let endpoint = match (file.base_url, file.model) {
        (Some(url), Some(model)) => {
            Some(AiEndpoint::parse(&url, &model).map_err(|e| format!("server: {e}"))?)
        }
        (None, None) => None,
        _ => return Err("the server needs both an address and a model".into()),
    };
    if file.enabled && endpoint.is_none() {
        return Err("the assistant is on with no server".into());
    }
    Ok(AiSettings {
        enabled: file.enabled,
        endpoint,
        local_output: file.local_output,
        key_origin: file.key_origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_load_back_and_a_bad_file_is_refused() {
        let settings = AiSettings {
            enabled: true,
            endpoint: Some(AiEndpoint::parse("http://localhost:11434/v1", "llama3").unwrap()),
            local_output: true,
            key_origin: Some("http://localhost:11434".into()),
        };
        let bytes = to_json(&settings).unwrap();
        assert_eq!(from_json(&bytes).unwrap(), settings);
        assert_eq!(
            from_json(br#"{"version": 1}"#).unwrap(),
            AiSettings::default()
        );
        for bad in [
            r#"{"version": 2}"#,
            r#"{"version": 1, "enabled": true}"#,
            r#"{"version": 1, "baseUrl": "http://evil.example", "model": "m"}"#,
            r#"{"version": 1, "baseUrl": "https://a.example"}"#,
            r#"{"version": 1, "apiKey": "sk"}"#,
        ] {
            assert!(from_json(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
