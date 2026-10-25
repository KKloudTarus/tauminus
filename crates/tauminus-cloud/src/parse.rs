// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The CLIs' JSON as instances. Each reader takes only the fields it
//! needs, and leaves out an instance it cannot read, counting it.

use serde_json::Value;
use tauminus_app::cloud::Instance;
use tauminus_domain::Origin;

/// Characters of a name kept: a label's length.
const MAX_NAME: usize = 128;
/// Tags read per instance, at most.
const MAX_TAGS: usize = 64;

/// A name as it may be kept: control characters and those that change
/// how text reads (bidi controls) out, cut to a label's length, as ADR
/// 0029 keeps names from files.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
                )
        })
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// `key.value`, or `key` alone, lowercase: candidates the app keeps when
/// they are valid tags.
fn tag(key: &str, value: &str) -> String {
    if value.is_empty() {
        key.to_ascii_lowercase()
    } else {
        format!("{key}.{value}").to_ascii_lowercase()
    }
}

fn origin(text: &str) -> Option<Origin> {
    Origin::parse(text).ok()
}

/// What a reader made of a document: instances, and how many were not
/// readable.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Read {
    pub instances: Vec<Instance>,
    pub unreadable: usize,
}

impl Read {
    fn push(&mut self, instance: Option<Instance>) {
        match instance {
            Some(instance) => self.instances.push(instance),
            None => self.unreadable += 1,
        }
    }
}

/// `aws ec2 describe-regions`: the region names.
pub(crate) fn ec2_regions(json: &Value) -> Vec<String> {
    array(json, "Regions")
        .iter()
        .filter_map(|r| text(r, "RegionName"))
        .map(str::to_owned)
        .collect()
}

/// `aws lightsail get-regions`.
pub(crate) fn lightsail_regions(json: &Value) -> Vec<String> {
    array(json, "regions")
        .iter()
        .filter_map(|r| text(r, "name"))
        .map(str::to_owned)
        .collect()
}

/// `aws sts get-caller-identity`: the account.
pub(crate) fn aws_account(json: &Value) -> Option<String> {
    text(json, "Account").map(str::to_owned)
}

/// `aws ec2 describe-instances` in `region` of `account`. Terminated and
/// terminating instances are left out.
pub(crate) fn ec2(json: &Value, account: &str, region: &str, out: &mut Read) {
    for reservation in array(json, "Reservations") {
        for instance in array(reservation, "Instances") {
            let state = instance
                .get("State")
                .and_then(|s| text(s, "Name"))
                .unwrap_or_default();
            if matches!(state, "terminated" | "shutting-down") {
                continue;
            }
            out.push(ec2_instance(instance, account, region));
        }
    }
}

fn ec2_instance(instance: &Value, account: &str, region: &str) -> Option<Instance> {
    let id = text(instance, "InstanceId")?;
    let mut name = String::new();
    let mut tags = Vec::new();
    for pair in array(instance, "Tags").iter().take(MAX_TAGS) {
        let (Some(key), value) = (text(pair, "Key"), text(pair, "Value").unwrap_or_default())
        else {
            continue;
        };
        if key == "Name" {
            name = clean(value);
        } else {
            tags.push(tag(key, value));
        }
    }
    let public = text(instance, "PublicIpAddress")
        .map(str::to_owned)
        .or_else(|| {
            array(instance, "NetworkInterfaces")
                .iter()
                .flat_map(|n| array(n, "Ipv6Addresses"))
                .find_map(|a| text(a, "Ipv6Address"))
                .map(str::to_owned)
        });
    Some(Instance {
        origin: origin(&format!("aws-ec2:{account}:{region}:{id}"))?,
        name,
        public,
        private: text(instance, "PrivateIpAddress").map(str::to_owned),
        user: None,
        tags,
        region: region.to_owned(),
    })
}

/// `aws lightsail get-instances` in `region` of `account`.
pub(crate) fn lightsail(json: &Value, account: &str, region: &str, out: &mut Read) {
    for instance in array(json, "instances") {
        out.push(lightsail_instance(instance, account, region));
    }
}

fn lightsail_instance(instance: &Value, account: &str, region: &str) -> Option<Instance> {
    // The ARN ends in an id that outlives the name.
    let id = text(instance, "arn")?.rsplit('/').next()?;
    let tags = array(instance, "tags")
        .iter()
        .take(MAX_TAGS)
        .filter_map(|t| Some(tag(text(t, "key")?, text(t, "value").unwrap_or_default())))
        .collect();
    Some(Instance {
        origin: origin(&format!("aws-lightsail:{account}:{region}:{id}"))?,
        name: clean(text(instance, "name").unwrap_or_default()),
        public: text(instance, "publicIpAddress").map(str::to_owned),
        private: text(instance, "privateIpAddress").map(str::to_owned),
        user: text(instance, "username").map(str::to_owned),
        tags,
        region: region.to_owned(),
    })
}

/// `doctl account get`: the account's uuid. Some versions print a list
/// of one.
pub(crate) fn do_account(json: &Value) -> Option<String> {
    let account = json.as_array().and_then(|a| a.first()).unwrap_or(json);
    text(account, "uuid").map(str::to_owned)
}

/// `doctl compute droplet list` of `account`.
pub(crate) fn droplets(json: &Value, account: &str, out: &mut Read) {
    for droplet in json.as_array().map_or(&[][..], Vec::as_slice) {
        out.push(droplet_instance(droplet, account));
    }
}

fn droplet_instance(droplet: &Value, account: &str) -> Option<Instance> {
    let id = droplet.get("id")?.as_u64()?;
    let networks = droplet.get("networks");
    let address = |family: &str, kind: &str| {
        networks
            .map(|n| array(n, family))
            .unwrap_or_default()
            .iter()
            .find(|a| text(a, "type") == Some(kind))
            .and_then(|a| text(a, "ip_address"))
            .map(str::to_owned)
    };
    Some(Instance {
        origin: origin(&format!("do:{account}:{id}"))?,
        name: clean(text(droplet, "name").unwrap_or_default()),
        public: address("v4", "public").or_else(|| address("v6", "public")),
        private: address("v4", "private"),
        user: Some("root".into()),
        tags: array(droplet, "tags")
            .iter()
            .take(MAX_TAGS)
            .filter_map(Value::as_str)
            .map(|t| tag(t, ""))
            .collect(),
        region: droplet
            .get("region")
            .and_then(|r| text(r, "slug"))
            .unwrap_or_default()
            .to_owned(),
    })
}

/// `az account show`: the subscription id.
pub(crate) fn azure_subscription(json: &Value) -> Option<String> {
    text(json, "id").map(str::to_owned)
}

/// `az vm list --show-details`.
pub(crate) fn azure(json: &Value, out: &mut Read) {
    for vm in json.as_array().map_or(&[][..], Vec::as_slice) {
        out.push(azure_instance(vm));
    }
}

fn azure_instance(vm: &Value) -> Option<Instance> {
    let id = text(vm, "id")?;
    // Several addresses come as one string, comma-separated.
    let first = |key: &str| {
        text(vm, key)
            .and_then(|list| list.split(',').map(str::trim).find(|a| !a.is_empty()))
            .map(str::to_owned)
    };
    let tags = vm
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .take(MAX_TAGS)
                .map(|(key, value)| tag(key, value.as_str().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    Some(Instance {
        origin: origin(&format!("azure:{id}"))?,
        name: clean(text(vm, "name").unwrap_or_default()),
        public: first("publicIps"),
        private: first("privateIps"),
        user: vm
            .get("osProfile")
            .and_then(|p| text(p, "adminUsername"))
            .map(str::to_owned),
        tags,
        region: text(vm, "location").unwrap_or_default().to_owned(),
    })
}

/// The subscription a VM's resource id names.
pub(crate) fn azure_scope(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("/subscriptions/")?;
    rest.split('/').next()
}

/// `gcloud compute instances list` of `project`: terminated ones too, as
/// they come back with the same id.
pub(crate) fn gcp(json: &Value, project: &str, out: &mut Read) {
    for instance in json.as_array().map_or(&[][..], Vec::as_slice) {
        out.push(gcp_instance(instance, project));
    }
}

fn gcp_instance(instance: &Value, project: &str) -> Option<Instance> {
    let id = text(instance, "id")?;
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let nic = array(instance, "networkInterfaces").first();
    let public = nic.and_then(|n| {
        array(n, "accessConfigs")
            .iter()
            .find_map(|c| text(c, "natIP"))
            .map(str::to_owned)
    });
    let mut tags: Vec<String> = instance
        .get("labels")
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .take(MAX_TAGS)
                .map(|(key, value)| tag(key, value.as_str().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(network) = instance.get("tags") {
        tags.extend(
            array(network, "items")
                .iter()
                .take(MAX_TAGS)
                .filter_map(Value::as_str)
                .map(|t| tag(t, "")),
        );
    }
    let zone = text(instance, "zone")
        .and_then(|z| z.rsplit('/').next())
        .unwrap_or_default();
    // A zone's region: `us-central1-a` is in `us-central1`.
    let region = zone.rsplit_once('-').map_or(zone, |(region, _)| region);
    Some(Instance {
        origin: origin(&format!("gcp:{project}:{id}"))?,
        name: clean(text(instance, "name").unwrap_or_default()),
        public,
        private: nic.and_then(|n| text(n, "networkIP")).map(str::to_owned),
        user: None,
        tags,
        region: region.to_owned(),
    })
}

#[cfg(test)]
mod tests;
