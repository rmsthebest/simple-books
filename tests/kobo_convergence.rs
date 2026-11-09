//! However changes interleave with a paginated sync, a device must end up
//! matching the server, and must never be told about a change to a book it
//! doesn't have.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::Method;
use common::TestServer;
use proptest::prelude::*;
use serde_json::{Value, json};
use simple_books::server::db::{
    models::{BookDetails, NewBook, Visibility},
    scope::{BookError, UserScope},
};
use uuid::Uuid;

#[derive(Debug, Clone)]
enum Op {
    Add,
    Edit(usize),
    Delete(usize),
    /// A reading state update from another device.
    State(usize),
    /// The device syncs one page.
    SyncPage,
    /// The device is reset and forgets everything.
    FactoryReset,
}

/// Indexes come from a small range, so the same few books get edited, deleted
/// and updated repeatedly. That's where the interesting interleavings are.
fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Add),
        3 => (0usize..4).prop_map(Op::Edit),
        3 => (0usize..4).prop_map(Op::Delete),
        3 => (0usize..4).prop_map(Op::State),
        4 => Just(Op::SyncPage),
        1 => Just(Op::FactoryReset),
    ]
}

#[derive(Default)]
struct FakeDevice {
    token: Option<String>,
    /// id -> (title, reading percent)
    books: BTreeMap<String, (String, Option<i64>)>,
}

impl FakeDevice {
    /// Applies one sync page and returns whether more pages remain.
    async fn sync_page(&mut self, server: &TestServer, kobo_token: &str) -> bool {
        let reply = server.sync(kobo_token, self.token.as_deref()).await;
        assert_eq!(reply.status, 200);
        self.token = Some(
            reply
                .header("x-kobo-synctoken")
                .expect("sync token")
                .to_owned(),
        );

        for entry in reply.json().as_array().unwrap() {
            self.apply(entry);
        }
        reply.header("x-kobo-sync") == Some("continue")
    }

    fn apply(&mut self, entry: &Value) {
        if let Some(new) = entry.get("NewEntitlement") {
            self.upsert(new);
        } else if let Some(changed) = entry.get("ChangedEntitlement") {
            // Like firmware 6, act only on removal and ignore any metadata here.
            assert_eq!(
                changed["BookEntitlement"]["IsRemoved"], true,
                "edits must use ChangedProductMetadata: {changed}"
            );
            let id = changed["BookEntitlement"]["Id"].as_str().unwrap();
            self.books.remove(id);
        } else if let Some(changed) = entry.get("ChangedProductMetadata") {
            let id = changed["BookEntitlement"]["Id"].as_str().unwrap();
            let title = changed["BookMetadata"]["Title"]
                .as_str()
                .unwrap()
                .to_owned();
            // Metadata leaves the reading position alone.
            let book = self
                .books
                .get_mut(id)
                .unwrap_or_else(|| panic!("edit for a book the device doesn't have: {id}"));
            book.0 = title;
        } else if let Some(changed) = entry.get("ChangedReadingState") {
            let state = &changed["ReadingState"];
            let id = state["EntitlementId"].as_str().unwrap();
            if let Some(book) = self.books.get_mut(id) {
                book.1 = state["CurrentBookmark"]["ProgressPercent"].as_i64();
            }
        } else {
            panic!("unrecognised entry: {entry}");
        }
    }

    fn upsert(&mut self, entitlement: &Value) {
        let id = entitlement["BookEntitlement"]["Id"]
            .as_str()
            .unwrap()
            .to_owned();
        let title = entitlement["BookMetadata"]["Title"]
            .as_str()
            .unwrap()
            .to_owned();
        let percent = entitlement["ReadingState"]["CurrentBookmark"]["ProgressPercent"].as_i64();
        self.books.insert(id, (title, percent));
    }

    async fn sync_to_end(&mut self, server: &TestServer, kobo_token: &str) {
        while self.sync_page(server, kobo_token).await {}
    }
}

async fn run(ops: Vec<Op>) {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;

    // Live books on the server, with their expected title and percent.
    let mut live: BTreeMap<Uuid, (String, Option<i64>)> = BTreeMap::new();
    let mut device = FakeDevice::default();
    let mut counter = 0i64;

    // Large enough that pagination is actually exercised: a page holds 100.
    for index in 0..120 {
        let title = format!("seed {index}");
        live.insert(server.add_book(&scope, &title).await, (title, None));
    }

    for op in ops {
        counter += 1;
        let pick = |n: usize| live.keys().nth(n % live.len().max(1)).copied();
        match op {
            Op::Add => {
                let title = format!("added {counter}");
                live.insert(server.add_book(&scope, &title).await, (title, None));
            }
            Op::Edit(n) => {
                if let Some(id) = pick(n) {
                    let title = format!("edited {counter}");
                    scope.set_title(id, &title).await.unwrap();
                    live.get_mut(&id).unwrap().0 = title;
                }
            }
            Op::Delete(n) => {
                if let Some(id) = pick(n) {
                    scope.delete_book(id).await.unwrap();
                    live.remove(&id);
                }
            }
            Op::State(n) => {
                if let Some(id) = pick(n) {
                    let percent = counter % 100;
                    let body = json!({ "ReadingStates": [{
                        "StatusInfo": { "Status": "Reading" },
                        "CurrentBookmark": { "ProgressPercent": percent },
                    }]});
                    let path = format!("/kobo/{token}/v1/library/{id}/state");
                    let reply = server
                        .request(Method::PUT, &path, &[], body.to_string().into_bytes())
                        .await;
                    assert_eq!(reply.status, 200);
                    live.get_mut(&id).unwrap().1 = Some(percent);
                }
            }
            Op::SyncPage => {
                device.sync_page(&server, &token).await;
            }
            Op::FactoryReset => device = FakeDevice::default(),
        }
    }

    device.sync_to_end(&server, &token).await;

    let expected: BTreeMap<String, (String, Option<i64>)> = live
        .iter()
        .map(|(id, value)| (id.to_string(), value.clone()))
        .collect();
    assert_eq!(
        device.books.keys().collect::<BTreeSet<_>>(),
        expected.keys().collect::<BTreeSet<_>>(),
        "the device's book set must match the server's"
    );
    assert_eq!(
        device.books, expected,
        "titles and reading progress must match"
    );

    // A synced device has nothing further to receive.
    let reply = server.sync(&token, device.token.as_deref()).await;
    assert_eq!(reply.json(), json!([]));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn device_converges(ops in proptest::collection::vec(op(), 1..50)) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(ops));
    }
}

// ─── Several users ───────────────────────────────────────────────────────────

const USERS: usize = 3;
/// Holds the admin role in these runs.
const ADMIN: usize = 0;

#[derive(Debug, Clone)]
enum SharedOp {
    Upload {
        user: usize,
        shared: bool,
        pending: bool,
    },
    FinishConversion(usize),
    AddToLibrary {
        user: usize,
        book: usize,
    },
    RemoveFromLibrary {
        user: usize,
        book: usize,
    },
    SetShared {
        user: usize,
        book: usize,
        shared: bool,
    },
    Edit {
        user: usize,
        book: usize,
    },
    Delete {
        user: usize,
        book: usize,
    },
    State {
        user: usize,
        book: usize,
    },
    SyncPage(usize),
    FactoryReset(usize),
}

fn shared_op() -> impl Strategy<Value = SharedOp> {
    let user = || 0..USERS;
    let book = || 0usize..6;
    prop_oneof![
        3 => (user(), any::<bool>(), proptest::bool::weighted(0.25)).prop_map(
            |(user, shared, pending)| SharedOp::Upload { user, shared, pending }
        ),
        1 => book().prop_map(SharedOp::FinishConversion),
        4 => (user(), book()).prop_map(|(user, book)| SharedOp::AddToLibrary { user, book }),
        3 => (user(), book()).prop_map(|(user, book)| SharedOp::RemoveFromLibrary { user, book }),
        2 => (user(), book(), any::<bool>()).prop_map(
            |(user, book, shared)| SharedOp::SetShared { user, book, shared }
        ),
        2 => (user(), book()).prop_map(|(user, book)| SharedOp::Edit { user, book }),
        1 => (user(), book()).prop_map(|(user, book)| SharedOp::Delete { user, book }),
        2 => (user(), book()).prop_map(|(user, book)| SharedOp::State { user, book }),
        5 => user().prop_map(SharedOp::SyncPage),
        1 => user().prop_map(SharedOp::FactoryReset),
    ]
}

/// What the server should hold for a book, tracked independently of it.
struct ModelBook {
    id: Uuid,
    owner: usize,
    shared: bool,
    pending: bool,
    title: String,
    /// Who has it in their library, and their reading percent.
    holders: BTreeMap<usize, Option<i64>>,
}

impl ModelBook {
    fn readable_by(&self, user: usize) -> bool {
        self.owner == user || self.shared
    }

    fn edit_outcome(&self, user: usize) -> Outcome {
        if self.owner == user || (self.shared && user == ADMIN) {
            Outcome::Done
        } else if self.shared {
            Outcome::Forbidden
        } else {
            Outcome::NotFound
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Done,
    NotFound,
    Forbidden,
}

fn outcome(result: Result<(), BookError>) -> Outcome {
    match result {
        Ok(()) => Outcome::Done,
        Err(BookError::NotFound) => Outcome::NotFound,
        Err(BookError::Forbidden) => Outcome::Forbidden,
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

/// Files aren't touched here, so runs stay fast: these devices never download.
async fn upload(scope: &UserScope, title: &str, shared: bool, pending: bool) -> Uuid {
    let id = Uuid::new_v4();
    let new = NewBook {
        id,
        title: title.to_owned(),
        authors: vec!["Ann".to_owned()],
        cover_id: None,
        original_sha256: id.to_string(),
        original_size: 10,
        visibility: if shared {
            Visibility::Shared
        } else {
            Visibility::Private
        },
        kepub_size: if pending { None } else { Some(10) },
        details: BookDetails::default(),
    };
    scope.create_book(&new, !pending).await.unwrap();
    id
}

// One long match over the operations reads better than one function for each.
#[allow(clippy::too_many_lines)]
async fn run_shared(ops: Vec<SharedOp>) {
    let server = TestServer::start().await;
    let users = [
        server.admin("ann").await,
        server.user("bob").await,
        server.user("cat").await,
    ];
    let mut devices: Vec<FakeDevice> = (0..USERS).map(|_| FakeDevice::default()).collect();
    let mut books: Vec<ModelBook> = Vec::new();
    let mut counter = 0i64;

    // Bob holds enough of Ann's books to need several pages.
    for index in 0..105 {
        let title = format!("seed {index}");
        let id = upload(&users[ADMIN].0, &title, true, false).await;
        users[1].0.add_to_library(id).await.unwrap();
        books.push(ModelBook {
            id,
            owner: ADMIN,
            shared: true,
            pending: false,
            title,
            holders: BTreeMap::from([(ADMIN, None), (1, None)]),
        });
    }

    for op in ops {
        counter += 1;
        // Only the newest few, so the same books get worked over repeatedly.
        let pick = |n: usize, len: usize| (len > 0).then(|| len - 1 - (n % len.min(6)));
        match op {
            SharedOp::Upload {
                user,
                shared,
                pending,
            } => {
                let title = format!("upload {counter}");
                let id = upload(&users[user].0, &title, shared, pending).await;
                books.push(ModelBook {
                    id,
                    owner: user,
                    shared,
                    pending,
                    title,
                    holders: BTreeMap::from([(user, None)]),
                });
            }
            SharedOp::FinishConversion(n) => {
                if let Some(i) = pick(n, books.len()) {
                    let book = &mut books[i];
                    users[book.owner]
                        .0
                        .mark_kepub(book.id, Some(10))
                        .await
                        .unwrap();
                    book.pending = false;
                }
            }
            SharedOp::AddToLibrary { user, book } => {
                if let Some(i) = pick(book, books.len()) {
                    let book = &mut books[i];
                    let got = outcome(users[user].0.add_to_library(book.id).await);
                    if book.readable_by(user) {
                        assert_eq!(got, Outcome::Done);
                        book.holders.entry(user).or_insert(None);
                    } else {
                        assert_eq!(got, Outcome::NotFound);
                    }
                }
            }
            SharedOp::RemoveFromLibrary { user, book } => {
                if let Some(i) = pick(book, books.len()) {
                    let book = &mut books[i];
                    let got = outcome(users[user].0.remove_from_library(book.id).await);
                    if !book.readable_by(user) {
                        assert_eq!(got, Outcome::NotFound);
                    } else if book.owner == user {
                        assert_eq!(got, Outcome::Forbidden);
                    } else {
                        assert_eq!(got, Outcome::Done);
                        book.holders.remove(&user);
                    }
                }
            }
            SharedOp::SetShared { user, book, shared } => {
                if let Some(i) = pick(book, books.len()) {
                    let book = &mut books[i];
                    let visibility = if shared {
                        Visibility::Shared
                    } else {
                        Visibility::Private
                    };
                    let got = outcome(users[user].0.set_visibility(book.id, visibility).await);
                    assert_eq!(got, book.edit_outcome(user));
                    if got == Outcome::Done {
                        book.shared = shared;
                        if !shared {
                            let owner = book.owner;
                            book.holders.retain(|holder, _| *holder == owner);
                        }
                    }
                }
            }
            SharedOp::Edit { user, book } => {
                if let Some(i) = pick(book, books.len()) {
                    let book = &mut books[i];
                    let title = format!("edited {counter}");
                    let got = outcome(users[user].0.set_title(book.id, &title).await);
                    assert_eq!(got, book.edit_outcome(user));
                    if got == Outcome::Done {
                        book.title = title;
                    }
                }
            }
            SharedOp::Delete { user, book } => {
                if let Some(i) = pick(book, books.len()) {
                    let got = outcome(users[user].0.delete_book(books[i].id).await);
                    assert_eq!(got, books[i].edit_outcome(user));
                    if got == Outcome::Done {
                        books.remove(i);
                    }
                }
            }
            SharedOp::State { user, book } => {
                if let Some(i) = pick(book, books.len()) {
                    let book = &mut books[i];
                    let percent = counter % 100;
                    let body = json!({ "ReadingStates": [{
                        "StatusInfo": { "Status": "Reading" },
                        "CurrentBookmark": { "ProgressPercent": percent },
                    }]});
                    let path = format!("/kobo/{}/v1/library/{}/state", users[user].1, book.id);
                    let reply = server
                        .request(Method::PUT, &path, &[], body.to_string().into_bytes())
                        .await;
                    if let Some(state) = book.holders.get_mut(&user) {
                        assert_eq!(reply.status, 200);
                        *state = Some(percent);
                    } else {
                        // Nickel gets a success, but nothing is stored: a 404
                        // here would fail its whole sync.
                        assert_eq!(reply.status, 200);
                        assert_eq!(reply.json(), json!({}));
                    }
                }
            }
            SharedOp::SyncPage(user) => {
                devices[user].sync_page(&server, &users[user].1).await;
            }
            SharedOp::FactoryReset(user) => devices[user] = FakeDevice::default(),
        }
    }

    for (user, device) in devices.iter_mut().enumerate() {
        device.sync_to_end(&server, &users[user].1).await;
        let expected: BTreeMap<String, (String, Option<i64>)> = books
            .iter()
            .filter(|book| !book.pending)
            .filter_map(|book| {
                let percent = *book.holders.get(&user)?;
                Some((book.id.to_string(), (book.title.clone(), percent)))
            })
            .collect();
        assert_eq!(
            device.books, expected,
            "user {user}'s device must match exactly that user's library"
        );

        let reply = server.sync(&users[user].1, device.token.as_deref()).await;
        assert_eq!(reply.json(), json!([]), "user {user} has nothing further");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    #[test]
    fn every_users_device_converges(ops in proptest::collection::vec(shared_op(), 1..70)) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_shared(ops));
    }
}

/// A device that slept through add, remove, re-add and remove still holds the
/// book from the first add. The tombstone has to remember that.
#[tokio::test]
async fn second_removal_still_reaches_a_device_that_missed_the_first() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, bob_token) = server.user("bob").await;
    let book = server.add_shared_book(&ann, "shared").await;

    bob.add_to_library(book).await.unwrap();
    let mut device = FakeDevice::default();
    device.sync_to_end(&server, &bob_token).await;
    assert!(device.books.contains_key(&book.to_string()));

    bob.remove_from_library(book).await.unwrap();
    bob.add_to_library(book).await.unwrap();
    bob.remove_from_library(book).await.unwrap();

    device.sync_to_end(&server, &bob_token).await;
    assert!(
        !device.books.contains_key(&book.to_string()),
        "the removal must not be lost"
    );
}

/// The run that a stricter tombstone rule gets wrong: a book is delivered, then
/// edited and deleted, all before the paginated run finishes.
#[tokio::test]
async fn delete_after_edit_during_a_run_still_reaches_the_device() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;

    let mut ids = Vec::new();
    for index in 0..150 {
        ids.push(server.add_book(&scope, &format!("book {index}")).await);
    }

    let mut device = FakeDevice::default();
    assert!(
        device.sync_page(&server, &token).await,
        "the first page should leave more to send"
    );
    let delivered = ids[0];
    assert!(device.books.contains_key(&delivered.to_string()));

    scope.set_title(delivered, "edited").await.unwrap();
    scope.delete_book(delivered).await.unwrap();

    device.sync_to_end(&server, &token).await;
    assert!(
        !device.books.contains_key(&delivered.to_string()),
        "the delete must not be lost"
    );
}

/// A book created and deleted while the device was away is simply never seen.
#[tokio::test]
async fn unseen_book_produces_no_removal() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;

    let mut device = FakeDevice::default();
    device.sync_to_end(&server, &token).await;

    let id = server.add_book(&scope, "brief").await;
    scope.delete_book(id).await.unwrap();

    let reply = server.sync(&token, device.token.as_deref()).await;
    assert_eq!(reply.json(), json!([]));
}
