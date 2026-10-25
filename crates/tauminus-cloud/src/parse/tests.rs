// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use serde_json::json;

use super::*;

#[test]
fn ec2_instances_come_with_their_name_tags_and_addresses() {
    let json = json!({"Reservations": [{
        "OwnerId": "111122223333",
        "Instances": [
            {
                "InstanceId": "i-0abc",
                "State": {"Name": "running"},
                "PublicIpAddress": "203.0.113.7",
                "PrivateIpAddress": "10.0.0.5",
                "Tags": [
                    {"Key": "Name", "Value": "web\u{202e}gpj.exe"},
                    {"Key": "Team", "Value": "Core"},
                    {"Key": "env", "Value": "prod"}
                ]
            },
            {
                "InstanceId": "i-0v6",
                "State": {"Name": "stopped"},
                "NetworkInterfaces": [{"Ipv6Addresses": [{"Ipv6Address": "2001:db8::7"}]}]
            },
            {"InstanceId": "i-0gone", "State": {"Name": "terminated"}},
            {"State": {"Name": "running"}}
        ]
    }]});
    let mut read = Read::default();
    ec2(&json, "111122223333", "eu-west-1", &mut read);
    assert_eq!(read.unreadable, 1, "the one with no id");
    assert_eq!(read.instances.len(), 2, "terminated left out");
    let web = &read.instances[0];
    assert_eq!(web.origin.as_str(), "aws-ec2:111122223333:eu-west-1:i-0abc");
    assert_eq!(web.name, "webgpj.exe", "a bidi override is no part of it");
    assert_eq!(web.public.as_deref(), Some("203.0.113.7"));
    assert_eq!(web.private.as_deref(), Some("10.0.0.5"));
    // `env.prod`, never `prod`: a tag does not turn the guard on.
    assert_eq!(web.tags, ["team.core", "env.prod"]);
    assert_eq!(read.instances[1].public.as_deref(), Some("2001:db8::7"));
    assert_eq!(
        ec2_regions(
            &json!({"Regions": [{"RegionName": "eu-west-1"}, {"RegionName": "us-east-1"}]})
        ),
        ["eu-west-1", "us-east-1"]
    );
    assert_eq!(
        aws_account(&json!({"Account": "111122223333", "Arn": "arn:aws:iam::1:user/x"})).as_deref(),
        Some("111122223333")
    );
}

#[test]
fn lightsail_instances_bring_their_user() {
    let json = json!({"instances": [{
        "name": "blog",
        "arn": "arn:aws:lightsail:us-east-1:111122223333:Instance/1d2c3b4a-0000-4000-8000-000000000001",
        "username": "bitnami",
        "publicIpAddress": "198.51.100.4",
        "privateIpAddress": "172.26.0.4",
        "tags": [{"key": "site"}, {"key": "owner", "value": "ann"}]
    }]});
    let mut read = Read::default();
    lightsail(&json, "111122223333", "us-east-1", &mut read);
    let blog = &read.instances[0];
    assert_eq!(
        blog.origin.as_str(),
        "aws-lightsail:111122223333:us-east-1:1d2c3b4a-0000-4000-8000-000000000001"
    );
    assert_eq!(blog.user.as_deref(), Some("bitnami"));
    assert_eq!(blog.tags, ["site", "owner.ann"]);
    assert_eq!(
        lightsail_regions(&json!({"regions": [{"name": "us-east-1"}]})),
        ["us-east-1"]
    );
}

#[test]
fn droplets_come_with_root_and_their_public_address() {
    let json = json!([{
        "id": 3_164_444,
        "name": "example.com",
        "networks": {
            "v4": [
                {"ip_address": "10.128.0.2", "type": "private"},
                {"ip_address": "104.236.32.182", "type": "public"}
            ],
            "v6": [{"ip_address": "2604:a880:0:1010::18a:a001", "type": "public"}]
        },
        "region": {"slug": "nyc3"},
        "tags": ["web", "Has Space"]
    }]);
    let mut read = Read::default();
    droplets(&json, "b6fc48a1-0000", &mut read);
    let droplet = &read.instances[0];
    assert_eq!(droplet.origin.as_str(), "do:b6fc48a1-0000:3164444");
    assert_eq!(droplet.public.as_deref(), Some("104.236.32.182"));
    assert_eq!(droplet.private.as_deref(), Some("10.128.0.2"));
    assert_eq!(droplet.user.as_deref(), Some("root"));
    assert_eq!(droplet.region, "nyc3");
    assert_eq!(
        do_account(&json!({"uuid": "b6fc48a1-0000"})).as_deref(),
        Some("b6fc48a1-0000")
    );
    assert_eq!(do_account(&json!([{"uuid": "x"}])).as_deref(), Some("x"));
}

#[test]
fn azure_vms_come_with_their_first_addresses_and_admin() {
    let id = "/subscriptions/00000000-1111-2222-3333-444444444444/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/vm1";
    let json = json!([{
        "id": id,
        "name": "vm1",
        "location": "westeurope",
        "publicIps": "20.1.2.3, 20.1.2.4",
        "privateIps": "10.0.0.4",
        "osProfile": {"adminUsername": "azureuser"},
        "tags": {"env": "dev"}
    }]);
    let mut read = Read::default();
    azure(&json, &mut read);
    let vm = &read.instances[0];
    assert_eq!(vm.origin.as_str(), format!("azure:{id}"));
    assert_eq!(vm.public.as_deref(), Some("20.1.2.3"));
    assert_eq!(vm.user.as_deref(), Some("azureuser"));
    assert_eq!(vm.tags, ["env.dev"]);
    assert_eq!(
        azure_scope(id),
        Some("00000000-1111-2222-3333-444444444444")
    );
}

#[test]
fn gcp_instances_come_with_their_region_labels_and_network_tags() {
    let json = json!([{
        "id": "4567890123456789",
        "name": "vm-1",
        "zone": "https://www.googleapis.com/compute/v1/projects/my-project/zones/us-central1-a",
        "networkInterfaces": [{
            "networkIP": "10.128.0.2",
            "accessConfigs": [{"natIP": "34.66.1.2"}]
        }],
        "labels": {"team": "data"},
        "tags": {"items": ["http-server"]}
    }, {"id": "not-a-number", "name": "odd"}]);
    let mut read = Read::default();
    gcp(&json, "my-project", &mut read);
    assert_eq!(read.unreadable, 1);
    let vm = &read.instances[0];
    assert_eq!(vm.origin.as_str(), "gcp:my-project:4567890123456789");
    assert_eq!(vm.region, "us-central1");
    assert_eq!(vm.public.as_deref(), Some("34.66.1.2"));
    assert_eq!(vm.private.as_deref(), Some("10.128.0.2"));
    assert_eq!(vm.tags, ["team.data", "http-server"]);
}

#[test]
fn a_name_is_kept_as_a_label_may_hold_it() {
    assert_eq!(clean("  a\u{2066}b\nc  "), "abc");
    assert_eq!(clean(&"x".repeat(300)).len(), 128);
}
