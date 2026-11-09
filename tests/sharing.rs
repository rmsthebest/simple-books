//! Who can do what to which book, and the rules around sharing a file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{TestServer, entry_id};
use serde_json::json;
use simple_books::server::db::{
    models::{BookDetails, NewBook, Visibility},
    scope::{BookError, UserScope},
};
use uuid::Uuid;

struct World {
    server: TestServer,
    uploader: (UserScope, String),
    admin: (UserScope, String),
    other: (UserScope, String),
}

async fn world() -> World {
    let server = TestServer::start().await;
    let uploader = server.user("uma").await;
    let admin = server.admin("ann").await;
    let other = server.user("otto").await;
    World {
        server,
        uploader,
        admin,
        other,
    }
}

/// Whether a device can reach a book, checked on all four routes that serve one.
/// A refusal looks different per route, because Nickel needs it to: a 404 on
/// metadata or state fails its whole sync, so those answer an empty 200 instead.
/// Either way, no book data comes back.
async fn device_can_reach(server: &TestServer, token: &str, book: Uuid, cover: Uuid) -> bool {
    let metadata = server
        .get(&format!("/kobo/{token}/v1/library/{book}/metadata"))
        .await;
    let state = server
        .get(&format!("/kobo/{token}/v1/library/{book}/state"))
        .await;
    let file = server
        .get(&format!("/kobo/{token}/download/{book}/kepub"))
        .await;
    let image = server
        .get(&format!(
            "/kobo/{token}/covers/{cover}/300/400/80/false/image.jpg"
        ))
        .await;

    assert_eq!(metadata.status, StatusCode::OK);
    assert_eq!(state.status, StatusCode::OK);
    let reachable = file.status == StatusCode::OK;
    if reachable {
        assert_eq!(metadata.json()[0]["EntitlementId"], book.to_string());
        assert_eq!(state.json()[0]["EntitlementId"], book.to_string());
        assert_eq!(image.status, StatusCode::OK);
    } else {
        assert_eq!(metadata.json(), json!({}));
        assert_eq!(state.json(), json!({}));
        assert_eq!(file.status, StatusCode::NOT_FOUND);
        assert_eq!(image.status, StatusCode::NOT_FOUND);
    }
    reachable
}

fn cover_of(book: &simple_books::server::db::models::BookRow) -> Uuid {
    book.cover_id.as_deref().unwrap().parse().unwrap()
}

// By value, so call sites can pass the awaited call directly.
#[allow(clippy::needless_pass_by_value)]
fn assert_not_found<T: std::fmt::Debug>(result: Result<T, BookError>, what: &str) {
    assert!(
        matches!(result, Err(BookError::NotFound)),
        "{what}: {result:?}"
    );
}

// By value, so call sites can pass the awaited call directly.
#[allow(clippy::needless_pass_by_value)]
fn assert_forbidden<T: std::fmt::Debug>(result: Result<T, BookError>, what: &str) {
    assert!(
        matches!(result, Err(BookError::Forbidden)),
        "{what}: {result:?}"
    );
}

/// A private book doesn't exist as far as anyone else can tell, admins included.
#[tokio::test]
async fn private_book_is_invisible_to_everyone_else() {
    let w = world().await;
    let book = w.server.add_book(&w.uploader.0, "secret").await;
    let row = w.uploader.0.library_book(book).await.unwrap().unwrap();
    let cover = cover_of(&row);

    for (name, (scope, token)) in [("admin", &w.admin), ("other", &w.other)] {
        assert!(scope.readable_book(book).await.unwrap().is_none(), "{name}");
        assert!(scope.library_book(book).await.unwrap().is_none(), "{name}");
        assert_not_found(scope.set_title(book, "x").await, name);
        assert_not_found(scope.set_cover(book, Uuid::new_v4()).await, name);
        assert_not_found(scope.set_visibility(book, Visibility::Shared).await, name);
        assert_not_found(scope.delete_book(book).await, name);
        assert_not_found(scope.add_to_library(book).await, name);
        assert_not_found(scope.remove_from_library(book).await, name);
        assert!(
            !device_can_reach(&w.server, token, book, cover).await,
            "{name}'s device"
        );
    }

    assert_eq!(
        w.uploader
            .0
            .library_book(book)
            .await
            .unwrap()
            .unwrap()
            .title,
        "secret",
        "nothing changed"
    );
}

#[tokio::test]
async fn uploader_can_do_everything_except_drop_it_from_their_own_library() {
    let w = world().await;
    let book = w.server.add_book(&w.uploader.0, "mine").await;
    let scope = &w.uploader.0;

    assert!(scope.readable_book(book).await.unwrap().is_some());
    scope.set_title(book, "renamed").await.unwrap();
    scope.set_cover(book, Uuid::new_v4()).await.unwrap();
    scope
        .set_visibility(book, Visibility::Shared)
        .await
        .unwrap();
    scope
        .set_visibility(book, Visibility::Private)
        .await
        .unwrap();
    assert_forbidden(scope.remove_from_library(book).await, "own book");
    scope.delete_book(book).await.unwrap();
    assert!(scope.readable_book(book).await.unwrap().is_none());
}

#[tokio::test]
async fn admin_manages_shared_books_without_holding_them() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "shared").await;
    let admin = &w.admin.0;

    let seen = admin.readable_book(book).await.unwrap().unwrap();
    assert_eq!(seen.first_seq, None, "not in the admin's library");
    assert!(admin.library_book(book).await.unwrap().is_none());

    admin.set_title(book, "fixed").await.unwrap();
    assert_eq!(
        w.uploader
            .0
            .library_book(book)
            .await
            .unwrap()
            .unwrap()
            .title,
        "fixed"
    );

    admin
        .set_visibility(book, Visibility::Private)
        .await
        .unwrap();
    assert!(
        admin.readable_book(book).await.unwrap().is_none(),
        "once private, it's out of the admin's reach too"
    );
    assert!(w.uploader.0.library_book(book).await.unwrap().is_some());
}

#[tokio::test]
async fn admin_can_delete_a_shared_book_for_everyone() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "shared").await;
    w.other.0.add_to_library(book).await.unwrap();

    w.admin.0.delete_book(book).await.unwrap();

    for (scope, _) in [&w.uploader, &w.other] {
        assert!(scope.readable_book(book).await.unwrap().is_none());
        assert!(scope.library_book(book).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn other_user_can_read_and_add_a_shared_book_but_not_change_it() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "shared").await;
    let (other, token) = &w.other;

    assert!(other.readable_book(book).await.unwrap().is_some());
    assert_forbidden(other.set_title(book, "x").await, "title");
    assert_forbidden(other.set_cover(book, Uuid::new_v4()).await, "cover");
    assert_forbidden(
        other.set_visibility(book, Visibility::Private).await,
        "visibility",
    );
    assert_forbidden(other.delete_book(book).await, "delete");

    let row = w.uploader.0.library_book(book).await.unwrap().unwrap();
    let cover = cover_of(&row);

    // A shared book stays out of reach of their device until they add it.
    assert!(other.library_book(book).await.unwrap().is_none());
    assert!(!device_can_reach(&w.server, token, book, cover).await);
    let reply = w.server.sync(token, None).await;
    assert_eq!(reply.json(), json!([]), "not in their sync stream");

    other.add_to_library(book).await.unwrap();
    assert!(device_can_reach(&w.server, token, book, cover).await);
    let reply = w.server.sync(token, None).await;
    let entries = reply.json();
    let entries = entries.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].get("NewEntitlement").is_some());
    assert_eq!(entry_id(&entries[0]), book.to_string());

    other.remove_from_library(book).await.unwrap();
    assert!(!device_can_reach(&w.server, token, book, cover).await);
}

#[tokio::test]
async fn adding_twice_changes_nothing() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "shared").await;
    let (other, token) = &w.other;

    other.add_to_library(book).await.unwrap();
    let synced = w.server.sync(token, None).await;
    let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

    other.add_to_library(book).await.unwrap();
    let reply = w.server.sync(token, Some(&cursor)).await;
    assert_eq!(reply.json(), json!([]));
}

#[tokio::test]
async fn an_edit_reaches_each_holder_only() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "before").await;
    w.other.0.add_to_library(book).await.unwrap();
    let cursor = |reply: &common::Reply| reply.header("x-kobo-synctoken").unwrap().to_owned();

    let uploader_cursor = cursor(&w.server.sync(&w.uploader.1, None).await);
    let other_cursor = cursor(&w.server.sync(&w.other.1, None).await);
    let admin_cursor = cursor(&w.server.sync(&w.admin.1, None).await);

    w.uploader.0.set_title(book, "after").await.unwrap();

    for (token, cursor) in [(&w.uploader.1, uploader_cursor), (&w.other.1, other_cursor)] {
        let reply = w.server.sync(token, Some(&cursor)).await;
        let entries = reply.json();
        assert_eq!(
            entries[0]["ChangedProductMetadata"]["BookMetadata"]["Title"], "after",
            "{entries}"
        );
    }
    let reply = w.server.sync(&w.admin.1, Some(&admin_cursor)).await;
    assert_eq!(reply.json(), json!([]), "the admin doesn't hold it");
}

/// Making a book private leaves the other reader's device with a removal, and
/// their reading position goes with it.
#[tokio::test]
async fn unsharing_removes_it_from_other_libraries_and_forgets_their_position() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "shared").await;
    let (other, token) = &w.other;
    other.add_to_library(book).await.unwrap();
    let cursor = w
        .server
        .sync(token, None)
        .await
        .header("x-kobo-synctoken")
        .unwrap()
        .to_owned();

    let state = json!({ "ReadingStates": [{
        "StatusInfo": { "Status": "Reading" },
        "CurrentBookmark": { "ProgressPercent": 40 },
    }]});
    let reply = w
        .server
        .request(
            Method::PUT,
            &format!("/kobo/{token}/v1/library/{book}/state"),
            &[],
            state.to_string().into_bytes(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(other.reading_state(book).await.unwrap().is_some());

    w.uploader
        .0
        .set_visibility(book, Visibility::Private)
        .await
        .unwrap();

    let reply = w.server.sync(token, Some(&cursor)).await;
    let entries = reply.json();
    let removal = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.get("ChangedEntitlement").is_some())
        .expect("a removal");
    assert_eq!(
        removal["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"],
        true
    );
    assert_eq!(entry_id(removal), book.to_string());

    assert!(other.reading_state(book).await.unwrap().is_none());
    assert_not_found(other.add_to_library(book).await, "now private");

    // Shared again, and re-added, it starts over for them.
    w.uploader
        .0
        .set_visibility(book, Visibility::Shared)
        .await
        .unwrap();
    other.add_to_library(book).await.unwrap();
    assert!(other.reading_state(book).await.unwrap().is_none());
}

fn book_with_hash(hash: &str, visibility: Visibility) -> NewBook {
    NewBook {
        id: Uuid::new_v4(),
        title: "same file".to_owned(),
        authors: Vec::new(),
        cover_id: None,
        original_sha256: hash.to_owned(),
        original_size: 10,
        visibility,
        kepub_size: Some(10),
        details: BookDetails::default(),
    }
}

#[tokio::test]
async fn only_one_shared_copy_of_a_file_can_exist() {
    let w = world().await;
    let shared = book_with_hash("abc", Visibility::Shared);
    w.uploader.0.create_book(&shared, true).await.unwrap();

    // A second shared upload of the file is refused.
    let second = book_with_hash("abc", Visibility::Shared);
    assert!(matches!(
        w.other.0.create_book(&second, true).await,
        Err(BookError::AlreadyShared)
    ));

    // A private copy is fine, but it can't be switched to shared while the
    // first copy is.
    let private = book_with_hash("abc", Visibility::Private);
    w.other.0.create_book(&private, true).await.unwrap();
    assert!(matches!(
        w.other
            .0
            .set_visibility(private.id, Visibility::Shared)
            .await,
        Err(BookError::AlreadyShared)
    ));
    let row = w.other.0.library_book(private.id).await.unwrap().unwrap();
    assert_eq!(row.visibility, "private", "refused means unchanged");

    // Once the first copy stops being shared, the way is clear.
    w.uploader
        .0
        .set_visibility(shared.id, Visibility::Private)
        .await
        .unwrap();
    w.other
        .0
        .set_visibility(private.id, Visibility::Shared)
        .await
        .unwrap();
}

/// A user uploading the same file twice is a different clash from someone else
/// already sharing it, and mustn't be reported as one.
#[tokio::test]
async fn own_duplicate_is_told_apart_from_someone_elses_shared_book() {
    let w = world().await;
    let first = book_with_hash("abc", Visibility::Private);
    w.uploader.0.create_book(&first, true).await.unwrap();

    let again = book_with_hash("abc", Visibility::Private);
    assert!(matches!(
        w.uploader.0.create_book(&again, true).await,
        Err(BookError::AlreadyYours)
    ));
}

/// A book being converted stays hidden from every holder, then appears to all.
#[tokio::test]
async fn pending_book_reaches_every_holder_when_conversion_finishes() {
    let w = world().await;
    let mut book = book_with_hash("abc", Visibility::Shared);
    book.kepub_size = None;
    w.uploader.0.create_book(&book, false).await.unwrap();
    w.other.0.add_to_library(book.id).await.unwrap();

    for token in [&w.uploader.1, &w.other.1] {
        let reply = w.server.sync(token, None).await;
        assert_eq!(reply.json(), json!([]));
    }

    // Only the uploader's conversion job finishes a book.
    assert_not_found(w.other.0.mark_kepub(book.id, Some(10)).await, "other");
    w.uploader.0.mark_kepub(book.id, Some(10)).await.unwrap();

    for token in [&w.uploader.1, &w.other.1] {
        let reply = w.server.sync(token, None).await;
        let entries = reply.json();
        assert!(entries[0].get("NewEntitlement").is_some(), "{entries}");
    }
}

/// A cover ID belongs to a book, so a device can't use one to reach a book
/// outside its library. Metadata goes the same way once a book is deleted.
#[tokio::test]
async fn deleted_book_leaves_every_device() {
    let w = world().await;
    let book = w.server.add_shared_book(&w.uploader.0, "shared").await;
    let row = w.uploader.0.library_book(book).await.unwrap().unwrap();
    let cover = cover_of(&row);
    w.other.0.add_to_library(book).await.unwrap();
    let cursor = w
        .server
        .sync(&w.other.1, None)
        .await
        .header("x-kobo-synctoken")
        .unwrap()
        .to_owned();

    w.uploader.0.delete_book(book).await.unwrap();

    assert!(!device_can_reach(&w.server, &w.other.1, book, cover).await);
    let reply = w.server.sync(&w.other.1, Some(&cursor)).await;
    let entries = reply.json();
    assert_eq!(
        entries[0]["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"], true,
        "{entries}"
    );
}
