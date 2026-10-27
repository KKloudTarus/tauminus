// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from cloud accounts, read through the providers' own CLIs, as
//! the user is signed in with them (ADR 0032): no cloud credential is
//! kept here. Each command has a fixed argument list, no shell, no
//! input, a deadline and a cap on what it may print.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauminus_app::cloud::{CloudInventory, CloudRequest, Listing, Provider};
use tauminus_app::ports::BoxFuture;
use tauminus_cli::Finder;
use tokio::sync::Semaphore;

mod parse;

/// How long one command may take: `az` alone starts in seconds.
const DEADLINE: Duration = Duration::from_secs(120);
/// Most bytes of output read from one command.
const MAX_OUTPUT: usize = 64 << 20;
/// Regions asked at once: each `aws` is about a second of CPU, so more
/// than a laptop's cores gains nothing.
const PARALLEL: usize = 8;
/// Regions asked, at most.
const MAX_REGIONS: usize = 64;
/// Most instances one listing holds.
const MAX_INSTANCES: usize = 10_000;

/// The providers' CLIs.
#[derive(Debug, Clone)]
pub struct Clis {
    finder: Finder,
    deadline: Duration,
}

impl Default for Clis {
    fn default() -> Self {
        Self {
            finder: Finder::new(),
            deadline: DEADLINE,
        }
    }
}

/// A CLI's name, and where to get it.
struct Cli {
    name: &'static str,
    install: &'static str,
}

const AWS: Cli = Cli {
    name: "aws",
    install: "install the AWS CLI: https://aws.amazon.com/cli/",
};
const DOCTL: Cli = Cli {
    name: "doctl",
    install: "install doctl: https://docs.digitalocean.com/reference/doctl/",
};
const AZ: Cli = Cli {
    name: "az",
    install: "install the Azure CLI: https://learn.microsoft.com/cli/azure/",
};
const GCLOUD: Cli = Cli {
    name: "gcloud",
    install: "install the Google Cloud CLI: https://cloud.google.com/sdk/docs/install",
};

/// Whether `scope` (a profile, subscription or project name) holds only
/// characters that mean nothing to a shell or to `cmd.exe`.
fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 128
        && scope
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:@-".contains(&b))
        && !scope.starts_with('-')
}

impl Clis {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks for the CLIs in `dirs` alone, for tests.
    #[must_use]
    pub fn in_dirs(dirs: Vec<PathBuf>, deadline: Duration) -> Self {
        Self {
            finder: Finder::in_dirs(dirs),
            deadline,
        }
    }

    /// The file `cli` runs from: the first in the absolute entries of
    /// `PATH`, then the usual places. A relative entry would name a file
    /// in whatever directory the app started in.
    ///
    /// # Errors
    /// Not found, with how to install it.
    pub fn program(&self, name: &str) -> Result<PathBuf, String> {
        let cli = [AWS, DOCTL, AZ, GCLOUD]
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("no CLI {name}"))?;
        self.find(&cli)
    }

    fn find(&self, cli: &Cli) -> Result<PathBuf, String> {
        self.finder.find(cli.name, cli.install)
    }

    /// Runs `cli` with `args` and reads its JSON.
    async fn run(&self, cli: &Cli, args: &[&str]) -> Result<Value, String> {
        let program = self.find(cli)?;
        // AWS's pager and prompts would wait for a terminal.
        let env = [("AWS_PAGER", ""), ("AWS_CLI_AUTO_PROMPT", "off")];
        let run =
            tauminus_cli::run(cli.name, &program, args, &env, self.deadline, MAX_OUTPUT, 0).await?;
        if !run.status.success() {
            return Err(format!(
                "{} failed ({}): {}",
                cli.name, run.status, run.error
            ));
        }
        serde_json::from_slice(&run.stdout)
            .map_err(|_| format!("{} printed what is not JSON", cli.name))
    }

    async fn list(&self, request: &CloudRequest) -> Result<Listing, String> {
        if let Some(scope) = &request.scope
            && !valid_scope(scope)
        {
            return Err("a profile, subscription or project is letters, digits and `_.:@-`".into());
        }
        let scope = request.scope.as_deref();
        let work = async {
            match request.provider {
                Provider::Ec2 | Provider::Lightsail => self.aws(request.provider, scope).await,
                Provider::DigitalOcean => self.digitalocean(scope).await,
                Provider::Azure => self.azure(scope).await,
                Provider::Gcp => self.gcp(scope).await,
            }
        };
        // Regions run in waves: the whole listing gets a few commands' time.
        tokio::time::timeout(self.deadline * 3, work)
            .await
            .map_err(|_| "listing the account took longer than an import waits".to_owned())?
    }

    async fn aws(&self, provider: Provider, profile: Option<&str>) -> Result<Listing, String> {
        // `AWS_PAGER` empty turns version 2's pager off; version 1 has
        // none, and knows no `--no-cli-pager`.
        let base: Vec<&str> = match profile {
            Some(profile) => vec!["--profile", profile, "--output", "json"],
            None => vec!["--output", "json"],
        };
        let with = |args: &[&'static str]| {
            let mut all = base.clone();
            all.extend_from_slice(args);
            all
        };
        let identity = self
            .run(&AWS, &with(&["sts", "get-caller-identity"]))
            .await?;
        let account = parse::aws_account(&identity).ok_or("aws named no account")?;
        let lightsail = provider == Provider::Lightsail;
        let regions = if lightsail {
            parse::lightsail_regions(&self.run(&AWS, &with(&["lightsail", "get-regions"])).await?)
        } else {
            parse::ec2_regions(&self.run(&AWS, &with(&["ec2", "describe-regions"])).await?)
        };
        if regions.len() > MAX_REGIONS {
            return Err("aws listed more regions than an import asks".into());
        }
        if let Some(odd) = regions.iter().find(|r| !valid_region(r)) {
            return Err(format!("aws listed a region named {}", parse::clean(odd)));
        }
        let permits = Arc::new(Semaphore::new(PARALLEL));
        let mut tasks = tokio::task::JoinSet::new();
        for region in regions {
            let (clis, permits, account) = (self.clone(), Arc::clone(&permits), account.clone());
            let mut args: Vec<String> = base.iter().map(|&a| a.to_owned()).collect();
            // `--query` keeps the fields read and nothing else: a large
            // region's full answer runs to tens of MiB.
            args.extend(
                if lightsail {
                    [
                        "lightsail",
                        "get-instances",
                        "--query",
                        LIGHTSAIL_FIELDS,
                        "--region",
                    ]
                } else {
                    [
                        "ec2",
                        "describe-instances",
                        "--query",
                        EC2_FIELDS,
                        "--region",
                    ]
                }
                .map(str::to_owned),
            );
            args.push(region.clone());
            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await;
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let mut read = parse::Read::default();
                let json = match clis.run(&AWS, &args).await {
                    Ok(json) => json,
                    Err(error) => return (region, Err(error)),
                };
                if lightsail {
                    parse::lightsail(&json, &account, &region, &mut read);
                } else {
                    parse::ec2(&json, &account, &region, &mut read);
                }
                (region, Ok(read))
            });
        }
        let (provider, prefix) = if lightsail {
            ("AWS Lightsail", "aws-lightsail")
        } else {
            ("AWS EC2", "aws-ec2")
        };
        let mut listing = gather(tasks, &format!("{prefix}:{account}")).await?;
        listing.provider = provider.into();
        listing.account = account;
        Ok(listing)
    }

    async fn digitalocean(&self, context: Option<&str>) -> Result<Listing, String> {
        let mut base = vec!["--output", "json"];
        if let Some(context) = context {
            base.extend(["--context", context]);
        }
        let with = |args: &[&'static str]| {
            let mut all = args.to_vec();
            all.extend_from_slice(&base);
            all
        };
        let account = self.run(&DOCTL, &with(&["account", "get"])).await?;
        let uuid = parse::do_account(&account).ok_or("doctl named no account")?;
        let json = self
            .run(&DOCTL, &with(&["compute", "droplet", "list"]))
            .await?;
        let mut read = parse::Read::default();
        parse::droplets(&json, &uuid, &mut read);
        bounded(&read)?;
        Ok(Listing {
            provider: "DigitalOcean".into(),
            whole: vec![format!("do:{uuid}")],
            account: uuid,
            instances: read.instances,
            unreadable: read.unreadable,
            failed: Vec::new(),
        })
    }

    async fn azure(&self, subscription: Option<&str>) -> Result<Listing, String> {
        let mut base = vec!["--output", "json", "--only-show-errors"];
        if let Some(subscription) = subscription {
            base.extend(["--subscription", subscription]);
        }
        let with = |args: &[&'static str]| {
            let mut all = args.to_vec();
            all.extend_from_slice(&base);
            all
        };
        let account = self.run(&AZ, &with(&["account", "show"])).await?;
        let id = parse::azure_subscription(&account).ok_or("az named no subscription")?;
        let json = self
            .run(&AZ, &with(&["vm", "list", "--show-details"]))
            .await?;
        let mut read = parse::Read::default();
        parse::azure(&json, &mut read);
        // Only VMs of the subscription asked count as listed whole.
        read.instances.retain(|i| {
            i.origin
                .as_str()
                .strip_prefix("azure:")
                .and_then(parse::azure_scope)
                == Some(id.as_str())
        });
        bounded(&read)?;
        Ok(Listing {
            provider: "Azure".into(),
            whole: vec![format!("azure:/subscriptions/{id}")],
            account: id,
            instances: read.instances,
            unreadable: read.unreadable,
            failed: Vec::new(),
        })
    }

    async fn gcp(&self, project: Option<&str>) -> Result<Listing, String> {
        let project = if let Some(project) = project {
            project.to_owned()
        } else {
            let json = self
                .run(
                    &GCLOUD,
                    &["config", "get-value", "project", "--format=json", "--quiet"],
                )
                .await?;
            let project = json.as_str().unwrap_or_default().to_owned();
            if !valid_scope(&project) {
                return Err("gcloud has no project set: name one".into());
            }
            project
        };
        let json = self
            .run(
                &GCLOUD,
                &[
                    "compute",
                    "instances",
                    "list",
                    "--project",
                    &project,
                    "--format=json",
                    "--quiet",
                ],
            )
            .await?;
        let mut read = parse::Read::default();
        parse::gcp(&json, &project, &mut read);
        bounded(&read)?;
        Ok(Listing {
            provider: "Google Cloud".into(),
            whole: vec![format!("gcp:{project}")],
            account: project,
            instances: read.instances,
            unreadable: read.unreadable,
            failed: Vec::new(),
        })
    }
}

/// The regions' answers as one listing. A region refused (an
/// organization's policy denying it, say) is told, and its hosts are not
/// marked gone; the others import. Every region refused fails it.
async fn gather(
    mut tasks: tokio::task::JoinSet<(String, Result<parse::Read, String>)>,
    scope: &str,
) -> Result<Listing, String> {
    let mut listing = Listing::default();
    while let Some(joined) = tasks.join_next().await {
        let (region, read) = joined.map_err(|e| e.to_string())?;
        match read {
            Ok(read) => {
                listing.whole.push(format!("{scope}:{region}"));
                listing.instances.extend(read.instances);
                listing.unreadable += read.unreadable;
            }
            Err(error) => listing.failed.push((format!("region {region}"), error)),
        }
        if listing.instances.len() > MAX_INSTANCES {
            return Err(format!("more than {MAX_INSTANCES} instances"));
        }
    }
    if listing.whole.is_empty()
        && let Some((_, error)) = listing.failed.pop()
    {
        return Err(error);
    }
    listing.whole.sort();
    listing.failed.sort();
    // Regions answer in any order: the plan is the same each time.
    listing.instances.sort_by(|a, b| a.origin.cmp(&b.origin));
    Ok(listing)
}

/// The fields `parse::ec2` reads, in the shape it reads them.
const EC2_FIELDS: &str = "{Reservations: Reservations[].{Instances: Instances[].{InstanceId: InstanceId, State: State, PublicIpAddress: PublicIpAddress, PrivateIpAddress: PrivateIpAddress, Tags: Tags, NetworkInterfaces: NetworkInterfaces[].{Ipv6Addresses: Ipv6Addresses}}}}";
/// The fields `parse::lightsail` reads.
const LIGHTSAIL_FIELDS: &str = "{instances: instances[].{name: name, arn: arn, username: username, publicIpAddress: publicIpAddress, privateIpAddress: privateIpAddress, tags: tags}}";

/// A region name as AWS gives them, `eu-west-1`: it goes into a command
/// line.
fn valid_region(region: &str) -> bool {
    (1..=32).contains(&region.len())
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !region.starts_with('-')
}

fn bounded(read: &parse::Read) -> Result<(), String> {
    if read.instances.len() > MAX_INSTANCES {
        return Err(format!("more than {MAX_INSTANCES} instances"));
    }
    Ok(())
}

impl CloudInventory for Clis {
    fn list<'a>(&'a self, request: &'a CloudRequest) -> BoxFuture<'a, Result<Listing, String>> {
        Box::pin(Clis::list(self, request))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod search {
    #[test]
    fn a_region_is_lowercase_letters_digits_and_dashes() {
        assert!(super::valid_region("eu-west-1"));
        for odd in [
            "",
            "-x",
            "--endpoint-url=http://evil",
            "EU",
            "a b",
            &"a".repeat(33),
        ] {
            assert!(!super::valid_region(odd), "{odd}");
        }
    }
}
