// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end encrypted sync of the saved hosts, groups and snippets
//! (ADR 0030), through a folder or a git remote. Each device writes only
//! its own sealed objects, each holding its whole merged state; merging
//! is per field, by hybrid logical clock stamps.

pub mod connector;
pub mod doc;
pub mod engine;
pub mod flat;
pub mod git;
pub mod s3;
pub mod seal;
pub mod stamp;
pub mod target;
