// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The real `aws` against a local AWS stand-in. Run with it listening,
//! the `aws` CLI on `PATH`, `AWS_ENDPOINT_URL` set to it, and test keys:
//! `cargo test -p tauminus-cloud --test localstack -- --ignored`.

use tauminus_app::cloud::{CloudInventory, CloudRequest, Provider};
use tauminus_cloud::Clis;

#[tokio::test]
#[ignore = "needs a local AWS stand-in and the aws CLI"]
async fn ec2_lists_every_region_of_localstack() {
    let listing = Clis::new()
        .list(&CloudRequest {
            provider: Provider::Ec2,
            scope: None,
        })
        .await
        .unwrap();
    assert_eq!(listing.account, "000000000000");
    // `LocalStack` serves `cn-*` and `us-gov-*` from the same store as
    // `us-east-1`, so only the two regions the instances went to are
    // checked.
    let east: Vec<_> = listing
        .instances
        .iter()
        .filter(|i| i.region == "us-east-1")
        .collect();
    assert_eq!(east.len(), 2, "{:?}", listing.instances);
    assert!(east.iter().all(|i| i.name == "live-web"));
    assert!(east[0].tags.contains(&"team.core".to_owned()));
    assert_eq!(
        listing
            .instances
            .iter()
            .filter(|i| i.region == "eu-west-1")
            .count(),
        1,
        "the other region asked too"
    );
    for instance in &listing.instances {
        assert!(
            instance
                .origin
                .as_str()
                .starts_with("aws-ec2:000000000000:"),
            "{}",
            instance.origin.as_str()
        );
    }
}
