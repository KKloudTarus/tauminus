// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from cloud accounts, read through the providers' own CLIs, as
//! the user is signed in with them (ADR 0032): no cloud credential is
//! kept here. Each command has a fixed argument list, no shell, no
//! input, a deadline and a cap on what it may print.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauminus_app::cloud::{CloudInventory, CloudRequest, Listing, Provider};
use tauminus_app::ports::BoxFuture;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

mod parse;

/// How long one command may take: `az` alone starts in seconds.
const DEADLINE: Duration = Duration::from_secs(120);
/// Most bytes of output read from one command.
const MAX_OUTPUT: u64 = 64 << 20;
/// Regions asked at once.
const PARALLEL: usize = 4;
/// Regions asked, at most.
const MAX_REGIONS: usize = 64;
/// Most instances one listing holds.
const MAX_INSTANCES: usize = 10_000;
/// Bytes of a command's error shown.
const MAX_ERROR: usize = 256;

/// Where the CLIs are often installed, past the app's `PATH`, which a
/// desktop app started from a dock may lack.
const KNOWN_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/snap/bin",
    "/run/current-system/sw/bin",
    "/usr/bin",
];

/// The providers' CLIs.
#[derive(Debug, Clone)]
pub struct Clis {
    /// Where to look in place of `PATH`, for tests.
    dirs: Option<Vec<PathBuf>>,
    deadline: Duration,
}

impl Default for Clis {
    fn default() -> Self {
        Self {
            dirs: None,
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
            dirs: Some(dirs),
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
        let dirs: Vec<PathBuf> = if let Some(dirs) = &self.dirs {
            dirs.clone()
        } else {
            let path = std::env::var_os("PATH").unwrap_or_default();
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .chain(KNOWN_DIRS.iter().map(PathBuf::from))
                .collect()
        };
        // `az` and `gcloud` are batch files on Windows.
        let names: &[String] = &if cfg!(windows) {
            vec![format!("{}.exe", cli.name), format!("{}.cmd", cli.name)]
        } else {
            vec![cli.name.to_owned()]
        };
        dirs.iter()
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| is_program(candidate))
            .ok_or_else(|| {
                format!(
                    "{} is not installed on this computer: {}",
                    cli.name, cli.install
                )
            })
    }

    /// Runs `cli` with `args` and reads its JSON.
    async fn run(&self, cli: &Cli, args: &[&str]) -> Result<Value, String> {
        let program = self.find(cli)?;
        let mut command = Command::new(&program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // AWS's pager and prompts would wait for a terminal.
            .env("AWS_PAGER", "")
            .env("AWS_CLI_AUTO_PROMPT", "off");
        let mut child = command
            .spawn()
            .map_err(|e| format!("{} could not start: {e}", cli.name))?;
        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take())
        else {
            return Err(format!("{} could not start", cli.name));
        };
        let work = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut out_reader = (&mut stdout).take(MAX_OUTPUT + 1);
            let mut err_reader = (&mut stderr).take(64 << 10);
            let (read, _) = tokio::join!(
                out_reader.read_to_end(&mut out),
                err_reader.read_to_end(&mut err),
            );
            read.map_err(|e| e.to_string())?;
            if out.len() as u64 > MAX_OUTPUT {
                return Err(format!("{} printed more than an import reads", cli.name));
            }
            let status = child.wait().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((status, out, err))
        };
        let (status, out, err) =
            tokio::time::timeout(self.deadline, work)
                .await
                .map_err(|_| {
                    format!(
                        "{} took longer than {} s",
                        cli.name,
                        self.deadline.as_secs()
                    )
                })??;
        if !status.success() {
            let first = String::from_utf8_lossy(&err)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| parse::clean(l).chars().take(MAX_ERROR).collect::<String>())
                .unwrap_or_default();
            return Err(format!("{} failed ({status}): {first}", cli.name));
        }
        serde_json::from_slice(&out).map_err(|_| format!("{} printed what is not JSON", cli.name))
    }

    async fn list(&self, request: &CloudRequest) -> Result<Listing, String> {
        if let Some(scope) = &request.scope
            && !valid_scope(scope)
        {
            return Err("a profile, subscription or project is letters, digits and `_.:@-`".into());
        }
        let scope = request.scope.as_deref();
        match request.provider {
            Provider::Ec2 | Provider::Lightsail => self.aws(request.provider, scope).await,
            Provider::DigitalOcean => self.digitalocean(scope).await,
            Provider::Azure => self.azure(scope).await,
            Provider::Gcp => self.gcp(scope).await,
        }
    }

    async fn aws(&self, provider: Provider, profile: Option<&str>) -> Result<Listing, String> {
        let base: Vec<&str> = match profile {
            Some(profile) => vec!["--profile", profile, "--output", "json", "--no-cli-pager"],
            None => vec!["--output", "json", "--no-cli-pager"],
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
        let permits = Arc::new(Semaphore::new(PARALLEL));
        let mut tasks = tokio::task::JoinSet::new();
        for region in regions {
            let (clis, permits, account) = (self.clone(), Arc::clone(&permits), account.clone());
            let mut args: Vec<String> = base.iter().map(|&a| a.to_owned()).collect();
            args.extend(
                if lightsail {
                    ["lightsail", "get-instances", "--region"]
                } else {
                    ["ec2", "describe-instances", "--region"]
                }
                .map(str::to_owned),
            );
            args.push(region.clone());
            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await;
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let json = clis.run(&AWS, &args).await?;
                let mut read = parse::Read::default();
                if lightsail {
                    parse::lightsail(&json, &account, &region, &mut read);
                } else {
                    parse::ec2(&json, &account, &region, &mut read);
                }
                Ok::<_, String>(read)
            });
        }
        let mut instances = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let read = joined.map_err(|e| e.to_string())??;
            instances.extend(read.instances);
            if instances.len() > MAX_INSTANCES {
                return Err(format!("more than {MAX_INSTANCES} instances"));
            }
        }
        // Regions answer in any order: the plan is the same each time.
        instances.sort_by(|a, b| a.origin.cmp(&b.origin));
        let prefix = if lightsail {
            "aws-lightsail"
        } else {
            "aws-ec2"
        };
        Ok(Listing {
            provider: if lightsail {
                "AWS Lightsail"
            } else {
                "AWS EC2"
            }
            .into(),
            whole: vec![format!("{prefix}:{account}")],
            account,
            instances,
        })
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
        })
    }
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

fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests;
