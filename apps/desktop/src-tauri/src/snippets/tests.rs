// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_app::ports::{BoxFuture, SnippetStore, StoreError};

use super::*;

#[derive(Default)]
struct MemoryStore {
    saved: Mutex<Library>,
    next: AtomicUsize,
}

impl SnippetStore for MemoryStore {
    fn load(&self) -> BoxFuture<'_, Result<Library, StoreError>> {
        let library = self.saved.lock().unwrap().clone();
        Box::pin(async move { Ok(library) })
    }

    fn save<'a>(&'a self, library: &'a Library) -> BoxFuture<'a, Result<(), StoreError>> {
        *self.saved.lock().unwrap() = library.clone();
        Box::pin(async { Ok(()) })
    }

    fn new_id(&self) -> String {
        format!("id{}", self.next.fetch_add(1, Ordering::Relaxed))
    }
}

async fn book() -> SnippetBook {
    SnippetBook::open(Arc::new(MemoryStore::default()))
        .await
        .unwrap()
}

fn input(name: &str, folder: Option<&str>, body: &str) -> SnippetInput {
    SnippetInput {
        id: None,
        name: name.into(),
        folder: folder.map(Into::into),
        body: body.into(),
    }
}

#[tokio::test]
async fn the_list_goes_by_folder_then_name_with_its_placeholders() {
    let book = book().await;
    for (name, folder, body) in [
        ("tail", Some("Logs"), "tail -f {{file:/var/log/syslog}}"),
        ("Disk", None, "df -h"),
        ("journal", Some("Logs"), "journalctl -u {{unit}}"),
        ("blank folder", Some("  "), "uptime"),
    ] {
        let snippet = parse(&book, input(name, folder, body)).unwrap();
        book.edit(|l| l.put(snippet)).await.unwrap();
    }
    let listed = view(&book.snapshot().await);
    let order: Vec<_> = listed
        .iter()
        .map(|s| (s.folder.as_deref(), s.name.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            (None, "blank folder"),
            (None, "Disk"),
            (Some("Logs"), "journal"),
            (Some("Logs"), "tail"),
        ]
    );
    let tail = &listed[3].variables[0];
    assert_eq!(
        (tail.name.as_str(), tail.default.as_deref()),
        ("file", Some("/var/log/syslog"))
    );
}

#[tokio::test]
async fn bad_input_is_refused_with_a_code_the_view_can_tell_apart() {
    let book = book().await;
    let blank = parse(&book, input("x", None, " \n")).unwrap_err();
    assert_eq!(blank.code, ErrorCode::InvalidArgument);
    let unnamed = parse(&book, input("", None, "ls")).unwrap_err();
    assert_eq!(unnamed.code, ErrorCode::InvalidArgument);
    let mut bad_id = input("x", None, "ls");
    bad_id.id = Some("../x".into());
    assert_eq!(
        parse(&book, bad_id).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let missing: CommandError =
        SnippetBookError::Snippet(SnippetError::NoSuch(SnippetId::parse("gone").unwrap())).into();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn an_unknown_id_is_not_created() {
    let book = book().await;
    let mut ghost = input("ghost", None, "ls");
    ghost.id = Some("deleted-one".into());
    assert_eq!(
        save(&book, ghost).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    let saved = save(&book, input("real", None, "ls")).await.unwrap();
    let id = saved.snippets().next().unwrap().id.to_string();
    let mut edit = input("renamed", None, "ls -la");
    edit.id = Some(id);
    let edited = save(&book, edit).await.unwrap();
    assert_eq!(edited.snippets().next().unwrap().name.as_str(), "renamed");
}

#[tokio::test]
async fn a_body_with_editing_keys_is_refused() {
    let book = book().await;
    let error = save(&book, input("x", None, "ls\u{1}rm "))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}
