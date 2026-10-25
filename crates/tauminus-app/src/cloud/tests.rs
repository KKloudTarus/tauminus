// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use super::*;
use crate::HostBook;
use crate::fakes::FakeHostStore;

fn instance(id: &str, name: &str, public: Option<&str>, region: &str) -> Instance {
    Instance {
        origin: Origin::parse(&format!("aws-ec2:111122223333:{region}:{id}")).unwrap(),
        name: name.into(),
        public: public.map(str::to_owned),
        private: Some("10.0.0.5".into()),
        user: None,
        tags: vec!["team.core".into(), "Not A Tag".into()],
        region: region.into(),
    }
}

fn listing(instances: Vec<Instance>) -> Listing {
    Listing {
        provider: "AWS EC2".into(),
        account: "111122223333".into(),
        instances,
        whole: vec!["aws-ec2:111122223333".into()],
        unreadable: 0,
        failed: Vec::new(),
    }
}

async fn book() -> HostBook {
    HostBook::open(Arc::new(FakeHostStore::default()))
        .await
        .unwrap()
}

fn by_origin<'a>(inventory: &'a Inventory, origin: &str) -> &'a Host {
    inventory
        .hosts()
        .find(|h| h.origin.as_ref().is_some_and(|o| o.as_str() == origin))
        .unwrap()
}

#[tokio::test]
async fn new_instances_are_added_under_provider_account_and_region() {
    let book = book().await;
    let listing = listing(vec![
        instance("i-1", "web", Some("203.0.113.7"), "eu-west-1"),
        instance("i-2", "db", None, "us-east-1"),
    ]);
    let preview = book.cloud_plan(&listing, Address::Public).await;
    assert_eq!(preview.add.len(), 2);
    let (report, inventory) = book.import_cloud(&listing, Address::Public).await.unwrap();
    assert_eq!(report.added.len(), 2);
    let web = by_origin(&inventory, "aws-ec2:111122223333:eu-west-1:i-1");
    assert_eq!(web.address.as_str(), "203.0.113.7");
    assert_eq!(
        web.tags().iter().map(Tag::as_str).collect::<Vec<_>>(),
        ["team.core"]
    );
    // No public address: the private one stands in.
    let db = by_origin(&inventory, "aws-ec2:111122223333:us-east-1:i-2");
    assert_eq!(db.address.as_str(), "10.0.0.5");
    let path = |host: &Host| {
        let mut names = Vec::new();
        let mut group = host.group.clone();
        while let Some(id) = group {
            let found = inventory.group(&id).unwrap();
            names.push(found.name.as_str().to_owned());
            group = found.parent.clone();
        }
        names.reverse();
        names
    };
    assert_eq!(path(web), ["AWS EC2", "111122223333", "eu-west-1"]);
    assert_eq!(path(db), ["AWS EC2", "111122223333", "us-east-1"]);
    // Again: nothing to do, and no group made twice.
    let again = book.cloud_plan(&listing, Address::Public).await;
    assert_eq!(again, Plan::default());
    let (_, inventory) = book.import_cloud(&listing, Address::Public).await.unwrap();
    assert_eq!(inventory.groups().count(), 4);
}

#[tokio::test]
async fn a_host_found_again_keeps_what_the_user_set_and_follows_its_address() {
    let book = book().await;
    let first = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.7"),
        "eu-west-1",
    )]);
    book.import_cloud(&first, Address::Public).await.unwrap();
    let origin = "aws-ec2:111122223333:eu-west-1:i-1";
    book.edit(|i| {
        let mut host = by_origin(i, origin).clone();
        host.label = Label::parse("my web").unwrap();
        host.settings.port = Some(2222);
        i.put_host(host)
    })
    .await
    .unwrap();
    // Stopped and started: a new public address, and a new name.
    let moved = listing(vec![instance(
        "i-1",
        "web-2",
        Some("203.0.113.9"),
        "eu-west-1",
    )]);
    let plan = book.cloud_plan(&moved, Address::Public).await;
    assert_eq!(plan.moved.len(), 1);
    assert_eq!(plan.add.len(), 0);
    let (report, inventory) = book.import_cloud(&moved, Address::Public).await.unwrap();
    assert_eq!(report.moved, 1);
    let host = by_origin(&inventory, origin);
    assert_eq!(host.address.as_str(), "203.0.113.9");
    assert_eq!(host.label.as_str(), "my web");
    assert_eq!(host.settings.port, Some(2222));
}

#[tokio::test]
async fn a_host_gone_is_tagged_only_when_its_account_listed_whole_and_untagged_when_back() {
    let book = book().await;
    let both = listing(vec![
        instance("i-1", "web", Some("203.0.113.7"), "eu-west-1"),
        instance("i-2", "db", Some("203.0.113.8"), "eu-west-1"),
    ]);
    book.import_cloud(&both, Address::Public).await.unwrap();
    let only_web = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.7"),
        "eu-west-1",
    )]);
    // A region failed: nothing is called gone.
    let partial = Listing {
        whole: Vec::new(),
        unreadable: 0,
        failed: Vec::new(),
        ..only_web.clone()
    };
    assert_eq!(
        book.cloud_plan(&partial, Address::Public).await.gone,
        Vec::<HostId>::new()
    );
    let (report, inventory) = book.import_cloud(&only_web, Address::Public).await.unwrap();
    assert_eq!(report.gone, 1);
    let db = by_origin(&inventory, "aws-ec2:111122223333:eu-west-1:i-2");
    assert!(db.tags().iter().any(|t| t.as_str() == GONE_TAG));
    // Never deleted; and once listed again, untagged.
    let (report, inventory) = book.import_cloud(&both, Address::Public).await.unwrap();
    assert_eq!((report.back, report.added.len()), (1, 0));
    let db = by_origin(&inventory, "aws-ec2:111122223333:eu-west-1:i-2");
    assert!(!db.tags().iter().any(|t| t.as_str() == GONE_TAG));
    // Another account's hosts are not this listing's to call gone.
    let other = Listing {
        account: "999".into(),
        whole: vec!["aws-ec2:999".into()],
        unreadable: 0,
        failed: Vec::new(),
        instances: Vec::new(),
        ..both
    };
    assert_eq!(
        book.cloud_plan(&other, Address::Public).await.gone,
        Vec::<HostId>::new()
    );
}

#[tokio::test]
async fn odd_addresses_and_taken_names_are_skipped_or_told_apart() {
    let book = book().await;
    book.edit(|i| {
        i.put_host(Host::new(
            HostId::parse("mine").unwrap(),
            Label::parse("web").unwrap(),
            HostName::parse("web.example.com").unwrap(),
        ))
    })
    .await
    .unwrap();
    let mut metadata = instance("i-3", "meta", Some("169.254.169.254"), "eu-west-1");
    metadata.private = None;
    let mut loopback = instance("i-4", "lo", Some("127.0.0.1"), "eu-west-1");
    loopback.private = None;
    let mut nothing = instance("i-5", "none", None, "eu-west-1");
    nothing.private = None;
    let mut dashed = instance("i-6", "dash", Some("-oProxyCommand=x"), "eu-west-1");
    dashed.private = None;
    let plan = book
        .cloud_plan(
            &listing(vec![
                instance("i-1", "web", Some("203.0.113.7"), "eu-west-1"),
                metadata,
                loopback,
                nothing,
                dashed,
            ]),
            Address::Public,
        )
        .await;
    assert_eq!(plan.add.len(), 1);
    assert_eq!(
        plan.add[0].label.as_str(),
        "web (i-1)",
        "the name was taken"
    );
    let skipped: Vec<&str> = plan.skipped.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(skipped, ["meta", "lo", "none", "dash"]);
    // Private asked for, and an instance with none: skipped, not public.
    let mut public_only = instance("i-7", "pub", Some("203.0.113.9"), "eu-west-1");
    public_only.private = None;
    let plan = book
        .cloud_plan(&listing(vec![public_only]), Address::Private)
        .await;
    assert_eq!(plan.skipped[0].1, "it has no private address");
}

#[test]
fn the_user_comes_only_when_the_provider_names_a_valid_one() {
    let mut with_user = instance("i-1", "web", Some("203.0.113.7"), "eu-west-1");
    with_user.user = Some("ubuntu".into());
    let mut bad_user = instance("i-2", "db", Some("203.0.113.8"), "eu-west-1");
    bad_user.user = Some("-oops".into());
    let plan = plan(
        &Inventory::default(),
        &listing(vec![with_user, bad_user]),
        Address::Public,
    );
    assert_eq!(
        plan.add[0].user.as_ref().map(UserName::as_str),
        Some("ubuntu")
    );
    assert_eq!(plan.add[1].user, None);
}

#[tokio::test]
async fn a_host_full_of_tags_is_told_gone_and_keeps_every_tag() {
    let book = book().await;
    let one = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.7"),
        "eu-west-1",
    )]);
    book.import_cloud(&one, Address::Public).await.unwrap();
    let tags: Vec<Tag> = (0..MAX_TAGS)
        .map(|n| Tag::parse(&format!("t{n:02}")).unwrap())
        .chain([Tag::parse(PROD_TAG).unwrap()])
        .skip(1)
        .collect();
    book.edit(|inventory| {
        let mut host = inventory.hosts().next().unwrap().clone();
        host.set_tags(tags.clone())?;
        inventory.put_host(host)
    })
    .await
    .unwrap();
    let (report, inventory) = book
        .import_cloud(&listing(Vec::new()), Address::Public)
        .await
        .unwrap();
    assert_eq!(report.gone, 0);
    assert!(
        report.skipped[0].1.contains("no room"),
        "{:?}",
        report.skipped
    );
    let host = inventory.hosts().next().unwrap();
    let mut set = tags.clone();
    set.sort();
    assert_eq!(host.tags(), set.as_slice(), "prod and every other tag kept");
    // An imported host always has room: its tags stop one short.
    let crowded = Instance {
        tags: (0..40).map(|n| format!("c{n:02}")).collect(),
        ..instance("i-2", "api", Some("203.0.113.8"), "eu-west-1")
    };
    let planned = plan(
        &Inventory::default(),
        &listing(vec![crowded]),
        Address::Public,
    );
    assert_eq!(planned.add[0].tags.len(), MAX_TAGS - 1);
}

#[tokio::test]
async fn two_hosts_of_one_origin_both_follow_it() {
    let book = book().await;
    let one = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.7"),
        "eu-west-1",
    )]);
    book.import_cloud(&one, Address::Public).await.unwrap();
    // As two devices that imported one account leave it after a sync.
    let twin = book.new_host_id().unwrap();
    book.edit(|inventory| {
        let mut host = inventory.hosts().next().unwrap().clone();
        host.id = twin.clone();
        host.label = Label::parse("web (other device)")?;
        inventory.put_host(host)
    })
    .await
    .unwrap();
    let moved = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.9"),
        "eu-west-1",
    )]);
    let planned = book.cloud_plan(&moved, Address::Public).await;
    assert_eq!(planned.moved.len(), 2);
    let planned = book.cloud_plan(&listing(Vec::new()), Address::Public).await;
    assert_eq!(planned.gone.len(), 2);
}

#[tokio::test]
async fn a_stopped_instance_keeps_its_host_s_public_address() {
    let book = book().await;
    let one = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.7"),
        "eu-west-1",
    )]);
    book.import_cloud(&one, Address::Public).await.unwrap();
    // Stopped: EC2 drops the public address and keeps the private one.
    let stopped = listing(vec![instance("i-1", "web", None, "eu-west-1")]);
    let planned = book.cloud_plan(&stopped, Address::Public).await;
    assert_eq!(planned.moved, []);
    assert_eq!(planned.gone, []);
}

#[test]
fn a_new_host_falls_back_to_a_usable_private_address() {
    let odd = instance("i-1", "web", Some("169.254.1.1"), "eu-west-1");
    let planned = plan(&Inventory::default(), &listing(vec![odd]), Address::Public);
    assert_eq!(planned.add[0].address.as_str(), "10.0.0.5");
    let none = Instance {
        private: None,
        ..instance("i-2", "db", None, "eu-west-1")
    };
    let planned = plan(&Inventory::default(), &listing(vec![none]), Address::Public);
    assert_eq!(planned.skipped[0].1, "it has no address");
}

#[test]
fn loopback_and_metadata_in_any_spelling_are_skipped() {
    for address in [
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "::127.0.0.1",
        "::",
        "0.1.2.3",
        "255.255.255.255",
        "fd00:ec2::254",
        "127.1",
        "2130706433",
        "localhost",
    ] {
        let odd = Instance {
            private: None,
            ..instance("i-1", "web", Some(address), "eu-west-1")
        };
        let planned = plan(&Inventory::default(), &listing(vec![odd]), Address::Public);
        assert_eq!(planned.add, [], "{address} added");
    }
    let fine = instance("i-1", "web", Some("2001:db8::7"), "eu-west-1");
    let planned = plan(&Inventory::default(), &listing(vec![fine]), Address::Public);
    assert_eq!(planned.add.len(), 1);
}

#[test]
fn cloud_tags_never_set_prod_or_the_gone_mark() {
    let tagged = Instance {
        tags: vec!["prod".into(), "cloud-gone".into(), "web".into()],
        ..instance("i-1", "web", Some("203.0.113.7"), "eu-west-1")
    };
    let planned = plan(
        &Inventory::default(),
        &listing(vec![tagged]),
        Address::Public,
    );
    assert_eq!(
        planned.add[0]
            .tags
            .iter()
            .map(Tag::as_str)
            .collect::<Vec<_>>(),
        ["web"]
    );
}

#[test]
fn azure_vms_of_one_name_are_told_apart_by_resource_group() {
    let vm = |group: &str| {
        Instance {
        origin: Origin::parse(&format!(
            "azure:/subscriptions/s/resourcegroups/{group}/providers/microsoft.compute/virtualmachines/web"
        ))
        .unwrap(),
        ..instance("x", "web", Some("20.1.2.3"), "westeurope")
    }
    };
    let planned = plan(
        &Inventory::default(),
        &listing(vec![vm("a"), vm("b"), vm("rg(prod)")]),
        Address::Public,
    );
    let labels: Vec<_> = planned.add.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["web", "web (b)", "web (rg(prod))"]);
}

#[test]
fn a_long_name_taken_is_cut_to_fit_its_id() {
    let long = "x".repeat(MAX_LABEL);
    let twice = vec![
        instance("i-1", &long, Some("203.0.113.7"), "eu-west-1"),
        instance("i-2", &long, Some("203.0.113.8"), "eu-west-1"),
    ];
    let planned = plan(&Inventory::default(), &listing(twice), Address::Public);
    assert_eq!(planned.add.len(), 2);
    assert!(planned.add[1].label.as_str().ends_with(" (i-2)"));
    assert_eq!(planned.add[1].label.as_str().chars().count(), MAX_LABEL);
}

#[tokio::test]
async fn an_answer_not_all_read_or_a_region_refused_marks_nothing_gone() {
    let book = book().await;
    let one = listing(vec![instance(
        "i-1",
        "web",
        Some("203.0.113.7"),
        "eu-west-1",
    )]);
    book.import_cloud(&one, Address::Public).await.unwrap();
    let unread = Listing {
        unreadable: 2,
        ..listing(Vec::new())
    };
    let planned = book.cloud_plan(&unread, Address::Public).await;
    assert_eq!(planned.gone, []);
    assert_eq!(planned.skipped[0].0, "2 more");
    let refused = Listing {
        whole: vec!["aws-ec2:111122223333:us-east-1".into()],
        failed: vec![("region eu-west-1".into(), "UnauthorizedOperation".into())],
        ..listing(Vec::new())
    };
    let planned = book.cloud_plan(&refused, Address::Public).await;
    assert_eq!(planned.gone, []);
    assert_eq!(planned.skipped, refused.failed);
}
