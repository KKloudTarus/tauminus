// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[test]
fn a_fingerprint_round_trips_and_anything_else_is_refused() {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::try_from(i * 7).unwrap();
    }
    let view = ReviewView::from(Review {
        host: HostId::parse("h").unwrap(),
        label: "h".into(),
        new: true,
        lines: Vec::new(),
        startup: String::new(),
        fingerprint: Fingerprint(bytes),
    });
    assert_eq!(fingerprint(&view.fingerprint).unwrap(), Fingerprint(bytes));
    for bad in ["", "00", &"g".repeat(64), &"é".repeat(32), &"0".repeat(66)] {
        assert!(fingerprint(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_target_comes_in_trimmed_and_a_remote_as_a_secret() {
    let git = TargetInput {
        kind: TargetKind::Git,
        address: " https://t@example.com/s.git \n".into(),
        s3: None,
    }
    .into_target()
    .unwrap();
    assert_eq!(
        git,
        SyncTarget::Git(Secret::new("https://t@example.com/s.git".into()))
    );
    let folder = TargetInput {
        kind: TargetKind::Folder,
        address: "/srv/sync".into(),
        s3: None,
    }
    .into_target()
    .unwrap();
    assert_eq!(folder, SyncTarget::Folder("/srv/sync".into()));
    let bucket = S3Input {
        endpoint: " https://s3.example.com ".into(),
        region: "auto".into(),
        bucket: "team".into(),
        prefix: "sync".into(),
        key_id: "AKID".into(),
        secret: " s3-secret\n".into(),
    };
    assert!(!format!("{bucket:?}").contains("s3-secret"));
    let s3 = TargetInput {
        kind: TargetKind::S3,
        address: String::new(),
        s3: Some(bucket),
    }
    .into_target()
    .unwrap();
    let SyncTarget::S3(s3) = s3 else {
        panic!("not a bucket");
    };
    assert_eq!(s3.endpoint, "https://s3.example.com");
    assert_eq!(s3.secret.as_str(), "s3-secret");
    let missing = TargetInput {
        kind: TargetKind::S3,
        address: String::new(),
        s3: None,
    }
    .into_target();
    assert_eq!(missing.unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn sync_errors_map_to_codes_the_view_branches_on() {
    let cases = [
        (SyncServiceError::Running, ErrorCode::Busy),
        (SyncServiceError::Run(SyncRunError::Busy), ErrorCode::Busy),
        (SyncServiceError::Off, ErrorCode::NotFound),
        (SyncServiceError::On, ErrorCode::InvalidArgument),
        (
            SyncServiceError::Vault(tauminus_app::vault::VaultError::Locked),
            ErrorCode::VaultLocked,
        ),
        (
            SyncServiceError::Sync(SyncError::Sealed("wrong passphrase".into())),
            ErrorCode::WrongPassword,
        ),
        (
            SyncServiceError::Run(SyncRunError::Sync(SyncError::Target("down".into()))),
            ErrorCode::ConnectFailed,
        ),
        (
            SyncServiceError::Sync(SyncError::Newer("0.6".into())),
            ErrorCode::Unsupported,
        ),
    ];
    for (error, code) in cases {
        assert_eq!(CommandError::from(error).code, code);
    }
}
