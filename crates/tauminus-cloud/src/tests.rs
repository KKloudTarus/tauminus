// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The CLIs, played by small scripts that print what the real ones do.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tauminus-cloud-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A CLI named `name` that logs its arguments and environment, then
    /// runs `body`.
    fn cli(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.0.join(name);
        let log = self.0.join(format!("{name}.log"));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s|' \"$@\" >> '{log}'\necho \"pager=$AWS_PAGER\" >> '{log}'\n{body}\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(format!("{name}.log"))).unwrap_or_default()
    }

    fn clis(&self) -> Clis {
        Clis::in_dirs(vec![self.0.clone()], Duration::from_secs(10))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn request(provider: Provider, scope: Option<&str>) -> CloudRequest {
    CloudRequest {
        provider,
        scope: scope.map(str::to_owned),
    }
}

const AWS_SCRIPT: &str = r#"case "$*" in
*get-caller-identity*) echo '{"Account":"111122223333"}' ;;
*describe-regions*) echo '{"Regions":[{"RegionName":"eu-west-1"},{"RegionName":"us-east-1"}]}' ;;
*"--region eu-west-1"*) echo '{"Reservations":[{"Instances":[{"InstanceId":"i-1","State":{"Name":"running"},"PublicIpAddress":"203.0.113.7","Tags":[{"Key":"Name","Value":"web"}]}]}]}' ;;
*"--region us-east-1"*) echo '{"Reservations":[]}' ;;
*) echo "unexpected: $*" >&2; exit 2 ;;
esac"#;

#[tokio::test]
async fn ec2_asks_every_region_with_a_fixed_command_and_no_pager() {
    let dir = Dir::new("ec2");
    dir.cli("aws", AWS_SCRIPT);
    let listing = dir
        .clis()
        .list(&request(Provider::Ec2, Some("work")))
        .await
        .unwrap();
    assert_eq!(listing.provider, "AWS EC2");
    assert_eq!(listing.account, "111122223333");
    assert_eq!(
        listing.whole,
        [
            "aws-ec2:111122223333:eu-west-1",
            "aws-ec2:111122223333:us-east-1"
        ]
    );
    assert_eq!(listing.instances.len(), 1);
    assert_eq!(listing.instances[0].name, "web");
    let log = dir.log("aws");
    assert!(log.contains("--profile|work|--output|json|sts|get-caller-identity|"));
    assert!(log.contains("|--region|us-east-1|"));
    assert!(log.contains("ec2|describe-instances|--query|{Reservations: "));
    assert!(
        log.lines()
            .filter(|l| l.starts_with("pager="))
            .all(|l| l == "pager=")
    );
}

#[tokio::test]
async fn a_region_refused_is_told_and_the_others_import() {
    let dir = Dir::new("denied");
    dir.cli(
        "aws",
        &AWS_SCRIPT.replace(
            r#"*"--region us-east-1"*) echo '{"Reservations":[]}' ;;"#,
            r#"*"--region us-east-1"*) echo 'An error occurred (UnauthorizedOperation) when calling the DescribeInstances operation' >&2; exit 254 ;;"#,
        ),
    );
    let listing = dir
        .clis()
        .list(&request(Provider::Ec2, None))
        .await
        .unwrap();
    assert_eq!(listing.instances.len(), 1);
    assert_eq!(
        listing.whole,
        ["aws-ec2:111122223333:eu-west-1"],
        "only the region listed"
    );
    assert_eq!(listing.failed.len(), 1);
    assert_eq!(listing.failed[0].0, "region us-east-1");
    assert!(
        listing.failed[0].1.contains("UnauthorizedOperation"),
        "{:?}",
        listing.failed
    );
}

#[tokio::test]
async fn a_failing_cli_fails_the_import_with_its_first_line() {
    let dir = Dir::new("fail");
    dir.cli(
        "aws",
        "echo '\nUnable to locate credentials. You can configure credentials by running \"aws configure\".' >&2; exit 253",
    );
    let error = dir
        .clis()
        .list(&request(Provider::Lightsail, None))
        .await
        .unwrap_err();
    assert!(error.starts_with("aws failed"), "{error}");
    assert!(error.contains("Unable to locate credentials"), "{error}");
}

#[tokio::test]
async fn a_cli_that_hangs_is_stopped_at_its_deadline() {
    let dir = Dir::new("hang");
    dir.cli("doctl", "sleep 30");
    let clis = Clis::in_dirs(vec![dir.0.clone()], Duration::from_millis(300));
    let started = std::time::Instant::now();
    let error = clis
        .list(&request(Provider::DigitalOcean, None))
        .await
        .unwrap_err();
    assert!(error.contains("took longer"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_missing_cli_says_how_to_install_it() {
    let dir = Dir::new("missing");
    let error = dir
        .clis()
        .list(&request(Provider::Azure, None))
        .await
        .unwrap_err();
    assert!(error.contains("az is not installed"), "{error}");
    assert!(error.contains("learn.microsoft.com"), "{error}");
}

#[tokio::test]
async fn a_scope_that_could_mean_something_to_a_shell_runs_nothing() {
    let dir = Dir::new("scope");
    dir.cli("gcloud", "echo '[]'");
    for scope in ["a b", "x&calc", "\"q\"", "%PATH%", "-oops", ""] {
        let error = dir
            .clis()
            .list(&request(Provider::Gcp, Some(scope)))
            .await
            .unwrap_err();
        assert!(error.contains("letters, digits"), "{scope}: {error}");
    }
    assert_eq!(dir.log("gcloud"), "", "never ran");
}

#[tokio::test]
async fn digitalocean_azure_and_gcp_list_their_accounts() {
    let dir = Dir::new("others");
    dir.cli(
        "doctl",
        r#"case "$*" in
account*) echo '{"uuid":"b6fc48a1"}' ;;
*) echo '[{"id":7,"name":"d","networks":{"v4":[{"ip_address":"104.236.32.182","type":"public"}]},"region":{"slug":"nyc3"}}]' ;;
esac"#,
    );
    dir.cli(
        "az",
        r#"case "$*" in
account*) echo '{"id":"sub-1"}' ;;
*) echo '[{"id":"/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/a","name":"a","location":"westeurope","publicIps":"20.1.2.3"},{"id":"/subscriptions/other/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/b","name":"b","publicIps":"20.1.2.4"}]' ;;
esac"#,
    );
    dir.cli(
        "gcloud",
        r#"case "$*" in
config*) echo '"my-project"' ;;
*) echo '[{"id":"42","name":"g","zone":"projects/my-project/zones/europe-west1-b","networkInterfaces":[{"networkIP":"10.1.0.2"}]}]' ;;
esac"#,
    );
    let clis = dir.clis();
    let digitalocean = clis
        .list(&request(Provider::DigitalOcean, None))
        .await
        .unwrap();
    assert_eq!(digitalocean.whole, ["do:b6fc48a1"]);
    assert_eq!(digitalocean.instances[0].origin.as_str(), "do:b6fc48a1:7");
    let azure = clis.list(&request(Provider::Azure, None)).await.unwrap();
    assert_eq!(azure.whole, ["azure:/subscriptions/sub-1"]);
    assert_eq!(azure.instances.len(), 1, "only the subscription asked");
    let gcp = clis.list(&request(Provider::Gcp, None)).await.unwrap();
    assert_eq!(gcp.whole, ["gcp:my-project"]);
    assert_eq!(gcp.instances[0].region, "europe-west1");
    assert!(
        dir.log("gcloud")
            .contains("--project|my-project|--format=json|--quiet|")
    );
}
