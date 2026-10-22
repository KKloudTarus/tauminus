// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[test]
fn history_errors_map_to_stable_codes() {
    let code = |error: HistoryError| CommandError::from(error).code;
    assert_eq!(code(HistoryError::Locked), ErrorCode::VaultLocked);
    assert_eq!(code(HistoryError::NoVault), ErrorCode::NotFound);
    assert_eq!(
        code(HistoryError::Invalid("x".into())),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(HistoryError::Store("x".into())),
        ErrorCode::StoreFailed
    );
}

#[test]
fn times_from_the_view_are_whole_milliseconds_or_nothing() {
    assert_eq!(millis(Some(1_700_000_000_123.0)), Some(1_700_000_000_123));
    assert_eq!(millis(Some(f64::NAN)), None);
    assert_eq!(millis(Some(f64::INFINITY)), None);
    assert_eq!(millis(None), None);
}

#[test]
fn a_start_time_from_the_view_stays_within_the_last_day() {
    let now = 1_800_000_000_000;
    assert_eq!(started_at(None, now), now);
    assert_eq!(started_at(Some(1_799_999_990), now), 1_799_999_990_000);
    assert_eq!(started_at(Some(u32::MAX), now), now);
    assert_eq!(started_at(Some(0), now), now - 24 * 60 * 60 * 1000);
}
