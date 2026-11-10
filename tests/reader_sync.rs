//! The simple-books reader's own protocol, under `/device/v1`, against the
//! real router.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{PUBLIC_URL, Reply, TestServer};
use simple_books::server::{
    auth::device::hash_secret,
    convert::reader::REVISION,
    db::{pairing::MAX_PENDING, reader::ReaderChange, scope::UserScope},
    now,
};
use sync_proto::{Changes, PairStart, PairStatus, PlaceAnswer, REVISION_HEADER, Spot, Status};
use uuid::Uuid;

async fn start_pairing(server: &TestServer, name: &str) -> Reply {
    server
        .request(
            Method::POST,
            "/device/v1/pair",
            &[("content-type", "application/json")],
            serde_json::json!({ "name": name }).to_string().into_bytes(),
        )
        .await
}

async fn pair_start(server: &TestServer) -> PairStart {
    let reply = start_pairing(server, "Kobo Clara BW").await;
    assert_eq!(reply.status, StatusCode::OK);
    serde_json::from_slice(&reply.body).unwrap()
}

async fn pair_status(server: &TestServer, secret: &str) -> PairStatus {
    let reply = server
        .request(
            Method::GET,
            "/device/v1/pair",
            &[("authorization", &format!("Bearer {secret}"))],
            Vec::new(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    serde_json::from_slice(&reply.body).unwrap()
}

/// Types `code` on the device page, the way a browser posts the form.
async fn enter_code(server: &TestServer, cookie: &str, code: &str) -> String {
    let body = format!("code={}", code.replace(' ', "+"));
    let reply = server
        .request(
            Method::POST,
            "/device/pair",
            &[
                ("cookie", cookie),
                ("content-type", "application/x-www-form-urlencoded"),
                ("sec-fetch-site", "same-origin"),
            ],
            body.into_bytes(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    reply.header("location").unwrap().to_owned()
}

async fn page(server: &TestServer, path: &str, cookie: &str) -> String {
    let reply = server
        .request(Method::GET, path, &[("cookie", cookie)], Vec::new())
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    String::from_utf8(reply.body).unwrap()
}

#[tokio::test]
async fn a_reader_pairs_with_the_code_it_shows() {
    let server = TestServer::start().await;
    let (scope, kobo_token) = server.user("sam").await;
    let cookie = server.cookie(&scope).await;

    let start = pair_start(&server).await;
    assert_eq!(start.url, format!("{PUBLIC_URL}/device"));
    assert_eq!(start.code.len(), 9);
    assert_eq!(start.code.as_bytes()[4], b'-');
    assert!(start.expires_in >= 600);
    assert_eq!(
        pair_status(&server, &start.secret).await,
        PairStatus::Pending
    );
    assert!(
        server.sync(&start.secret, None).await.status == StatusCode::UNAUTHORIZED,
        "not a Kobo, even before pairing"
    );

    // Typed in lower case, as people do.
    let typed = start.code.to_lowercase();
    assert_eq!(
        enter_code(&server, &cookie, &typed).await,
        "/device?done=paired"
    );
    assert_eq!(
        pair_status(&server, &start.secret).await,
        PairStatus::Paired { user: "sam".into() }
    );

    // It's on the device page as a reader, which can't be reconnected like a Kobo.
    let devices = server.state.db.list_devices(scope.user_id()).await.unwrap();
    let reader = devices.iter().find(|d| d.reader).unwrap();
    assert_eq!(reader.name, "Kobo Clara BW");
    let html = page(&server, "/device", &cookie).await;
    assert!(html.contains("simple-books reader"));
    assert!(!html.contains(&format!("/device/{}/reconnect", reader.id)));

    // Each kind of secret works only on its own routes.
    assert_eq!(
        server.sync(&start.secret, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(pair_status(&server, &kobo_token).await, PairStatus::Expired);
    assert_eq!(server.sync(&kobo_token, None).await.status, StatusCode::OK);

    // A code works once.
    assert_eq!(
        enter_code(&server, &cookie, &start.code).await,
        "/device?error=pair_code"
    );

    // Removing it on the website ends it.
    let reply = server
        .request(
            Method::POST,
            &format!("/device/{}/remove", reader.id),
            &[("cookie", &cookie), ("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await;
    assert_eq!(
        reply.header("location"),
        Some("/device?done=device_removed")
    );
    assert_eq!(
        pair_status(&server, &start.secret).await,
        PairStatus::Expired
    );
}

#[tokio::test]
async fn wrong_codes_run_into_a_limit() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let cookie = server.cookie(&scope).await;
    let start = pair_start(&server).await;

    for wrong in ["AAAA-AAAA", "not a code"] {
        assert_eq!(
            enter_code(&server, &cookie, wrong).await,
            "/device?error=pair_code"
        );
    }
    for _ in 0..8 {
        enter_code(&server, &cookie, "BBBB-BBBB").await;
    }
    assert_eq!(
        enter_code(&server, &cookie, &start.code).await,
        "/device?error=slow",
        "even the right code, once over the limit"
    );
    assert_eq!(
        pair_status(&server, &start.secret).await,
        PairStatus::Pending
    );

    // Someone else isn't held up by it.
    let (other, _) = server.user("ann").await;
    let cookie = server.cookie(&other).await;
    assert_eq!(
        enter_code(&server, &cookie, &start.code).await,
        "/device?done=paired"
    );
}

#[tokio::test]
async fn codes_run_out() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let start = pair_start(&server).await;
    let code = start.code.replace('-', "");
    let hash = hash_secret(&start.secret);
    let later = now() + i64::from(start.expires_in);

    let db = &server.state.db;
    assert!(
        db.finish_pairing(scope.user_id(), &code, later)
            .await
            .is_err()
    );
    assert_eq!(
        db.pair_state(&hash, later).await.unwrap(),
        simple_books::server::db::pairing::PairState::Unknown
    );
    // An unknown secret is simply expired.
    assert_eq!(pair_status(&server, "made-up").await, PairStatus::Expired);
}

#[tokio::test]
async fn asking_for_codes_is_limited() {
    let server = TestServer::start().await;
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        last = start_pairing(&server, "Reader").await.status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS, "per address");

    // And in total, however many addresses ask.
    let db = &server.state.db;
    let at = now();
    let mut added = 0;
    for i in 0..MAX_PENDING + 5 {
        if db
            .start_pairing(
                &format!("hash-{i}"),
                &format!("CODE{i:04}"),
                "x",
                at,
                at + 60,
            )
            .await
            .unwrap()
        {
            added += 1;
        }
    }
    assert_eq!(added + 10, MAX_PENDING);
    // Once they run out, there's room again.
    assert!(
        db.start_pairing("hash-late", "LATECODE", "x", at + 61, at + 120)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn bad_requests_get_json_answers() {
    let server = TestServer::start().await;
    let reply = server
        .request(
            Method::POST,
            "/device/v1/pair",
            &[("content-type", "application/json")],
            b"{".to_vec(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.json()["error"].is_string());

    let reply = server.get("/device/v1/pair").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.json()["error"].is_string());
    assert_eq!(reply.header("cache-control"), Some("no-store"));
}

/// A reader paired with `scope`'s library. Returns its secret.
async fn paired(server: &TestServer, scope: &UserScope) -> String {
    let start = pair_start(server).await;
    server
        .state
        .db
        .finish_pairing(scope.user_id(), &start.code.replace('-', ""), now())
        .await
        .unwrap();
    start.secret
}

async fn call(
    server: &TestServer,
    method: Method,
    path: &str,
    secret: &str,
    body: Vec<u8>,
) -> Reply {
    let auth = format!("Bearer {secret}");
    let mut headers = vec![("authorization", auth.as_str())];
    if !body.is_empty() {
        headers.push(("content-type", "application/json"));
    }
    server
        .request(method, &format!("/device/v1{path}"), &headers, body)
        .await
}

async fn changes(server: &TestServer, secret: &str, since: i64) -> Changes {
    let reply = call(
        server,
        Method::GET,
        &format!("/changes?since={since}"),
        secret,
        Vec::new(),
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    serde_json::from_slice(&reply.body).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Pretends the background conversion made the book's reader file.
async fn convert(server: &TestServer, book: Uuid, bytes: &[u8]) {
    let path = server.state.layout.reader(book);
    std::fs::write(&path, bytes).unwrap();
    assert!(
        server
            .state
            .db
            .finish_reader_conversion(book, REVISION, Some((bytes.len() as i64, &sha256(bytes))))
            .await
            .unwrap()
    );
}

fn spot(percent: f32, at: i64) -> Spot {
    Spot {
        chapter: 2,
        block: 7,
        offset: 30,
        revision: REVISION,
        percent,
        status: Status::Reading,
        at,
    }
}

async fn put_place(server: &TestServer, secret: &str, book: Uuid, spot: &Spot) -> Reply {
    call(
        server,
        Method::PUT,
        &format!("/books/{book}/place"),
        secret,
        serde_json::to_vec(spot).unwrap(),
    )
    .await
}

#[tokio::test]
async fn a_reader_gets_books_once_they_are_converted() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let secret = paired(&server, &scope).await;
    let book = server.add_book(&scope, "First Book").await;

    let first = changes(&server, &secret, 0).await;
    assert!(first.full && !first.more);
    assert_eq!(first.books.len(), 1);
    assert_eq!(first.books[0].id, book.to_string());
    assert_eq!(first.books[0].title, "First Book");
    assert_eq!(first.books[0].authors, ["Ann"]);
    assert_eq!(first.books[0].file, None, "not converted yet");
    assert!(first.books[0].cover.is_some());
    let reply = call(
        &server,
        Method::GET,
        &format!("/books/{book}"),
        &secret,
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Converting it is a change, so the reader hears of it.
    assert_eq!(changes(&server, &secret, first.next).await.books, []);
    convert(&server, book, b"the book's bytes").await;
    let second = changes(&server, &secret, first.next).await;
    assert!(!second.full);
    let file = second.books[0].file.clone().unwrap();
    assert_eq!(file.size, 16);
    assert_eq!(file.revision, REVISION);
    assert_eq!(file.sha256, sha256(b"the book's bytes"));

    let reply = call(
        &server,
        Method::GET,
        &format!("/books/{book}"),
        &secret,
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, b"the book's bytes");
    assert_eq!(reply.header("content-length"), Some("16"));
    assert_eq!(
        reply.header(REVISION_HEADER),
        Some(REVISION.to_string().as_str())
    );
    // An interrupted download carries on where it stopped.
    let reply = server
        .request(
            Method::GET,
            &format!("/device/v1/books/{book}"),
            &[
                ("authorization", &format!("Bearer {secret}")),
                ("range", "bytes=4-"),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(reply.body, b"book's bytes");

    let reply = call(
        &server,
        Method::GET,
        &format!("/books/{book}/cover"),
        &secret,
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), Some("image/jpeg"));

    // Edits on the website come through, though the file isn't made again.
    scope.set_title(book, "Better Title").await.unwrap();
    let third = changes(&server, &secret, second.next).await;
    assert_eq!(third.books[0].title, "Better Title");

    // Deleted, it's removed, and can't be fetched.
    simple_books::server::testkit::delete(&scope, &server.state.layout, book)
        .await
        .unwrap();
    let fourth = changes(&server, &secret, third.next).await;
    assert_eq!(fourth.removed, [book.to_string()]);
    assert_eq!(fourth.books, []);
    let reply = call(
        &server,
        Method::GET,
        &format!("/books/{book}"),
        &secret,
        Vec::new(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // A sync from scratch doesn't list removals: it lists what there is.
    let again = changes(&server, &secret, 0).await;
    assert!(again.full && again.books.is_empty() && again.removed.is_empty());
}

#[tokio::test]
async fn a_reader_only_reaches_its_own_library() {
    let server = TestServer::start().await;
    let (sam, kobo_token) = server.user("sam").await;
    let (ann, _) = server.user("ann").await;
    let secret = paired(&server, &sam).await;
    let theirs = server.add_book(&ann, "Ann's").await;
    convert(&server, theirs, b"private").await;

    assert_eq!(changes(&server, &secret, 0).await.books, []);
    for path in [format!("/books/{theirs}"), format!("/books/{theirs}/cover")] {
        let reply = call(&server, Method::GET, &path, &secret, Vec::new()).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    let reply = put_place(&server, &secret, theirs, &spot(10.0, now())).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // No secret, a made-up one, or a Kobo's.
    for secret in ["", "made-up", kobo_token.as_str()] {
        let reply = call(&server, Method::GET, "/changes", secret, Vec::new()).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{secret:?}");
        assert!(reply.json()["error"].is_string());
    }

    // A disabled user's reader stops.
    server
        .state
        .db
        .set_user_disabled(sam.user_id(), true)
        .await
        .unwrap();
    let reply = call(&server, Method::GET, "/changes", &secret, Vec::new()).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_newest_place_wins() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let secret = paired(&server, &scope).await;
    let other = paired(&server, &scope).await;
    let book = server.add_book(&scope, "Book").await;
    let start = changes(&server, &secret, 0).await.next;
    let at = now() - 100;

    let reply = put_place(&server, &secret, book, &spot(40.0, at)).await;
    let answer: PlaceAnswer = serde_json::from_slice(&reply.body).unwrap();
    assert!(answer.kept);
    assert_eq!(answer.place, spot(40.0, at));

    // The other reader hears where the first one is.
    let heard = changes(&server, &other, start).await;
    assert_eq!(heard.places.len(), 1);
    assert_eq!(heard.places[0].book, book.to_string());
    assert_eq!(heard.places[0].spot, spot(40.0, at));

    // An older place, sent late, loses, and the answer says what's there.
    let reply = put_place(&server, &other, book, &spot(20.0, at - 50)).await;
    let answer: PlaceAnswer = serde_json::from_slice(&reply.body).unwrap();
    assert!(!answer.kept);
    assert_eq!(answer.place, spot(40.0, at));
    assert_eq!(changes(&server, &other, heard.next).await.places, []);

    // A newer one wins, and a clock far ahead counts as now.
    let ahead = now() + 365 * 24 * 60 * 60;
    let reply = put_place(&server, &other, book, &spot(55.0, ahead)).await;
    let answer: PlaceAnswer = serde_json::from_slice(&reply.body).unwrap();
    assert!(answer.kept);
    assert!(answer.place.at <= now());
    assert_eq!(answer.place.percent, 55.0);

    // Nonsense isn't stored.
    for body in [
        b"{".to_vec(),
        serde_json::to_vec(&spot(f32::NAN, at)).unwrap_or_else(|_| b"{\"percent\":null}".to_vec()),
        serde_json::to_vec(&spot(10.0, 0)).unwrap(),
    ] {
        let reply = call(
            &server,
            Method::PUT,
            &format!("/books/{book}/place"),
            &secret,
            body,
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn places_reach_hardcover() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let secret = paired(&server, &scope).await;
    let book = server.add_book(&scope, "Book").await;
    let db = &server.state.db;
    let at = now();
    db.hardcover_save_sign_in(
        scope.user_id(),
        "dc",
        "UC",
        "https://hardcover.app/x",
        5,
        at + 600,
        at,
    )
    .await
    .unwrap();
    assert!(
        db.hardcover_connect(scope.user_id(), "dc", 1, "sam", "a", at + 3600, "r", at)
            .await
            .unwrap()
    );

    put_place(&server, &secret, book, &spot(30.0, at)).await;
    let due = db.hardcover_due(at + 120, 10).await.unwrap();
    let due = due.iter().find(|d| d.book_id == book.to_string()).unwrap();
    assert_eq!(due.want_status, 2, "currently reading");
    assert_eq!(due.want_percent, Some(30.0));

    let finished = Spot {
        status: Status::Finished,
        ..spot(100.0, at + 1)
    };
    put_place(&server, &secret, book, &finished).await;
    let due = db.hardcover_due(at + 1, 10).await.unwrap();
    let due = due.iter().find(|d| d.book_id == book.to_string()).unwrap();
    assert_eq!(due.want_status, 3, "read, straight away");
}

#[tokio::test]
async fn changes_come_a_page_at_a_time_and_can_start_over() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let secret = paired(&server, &scope).await;
    let mut books = Vec::new();
    for title in ["One", "Two", "Three"] {
        books.push(server.add_book(&scope, title).await);
    }

    // Pages by sequence, each change once.
    let mut since = 0;
    let mut seen = Vec::new();
    loop {
        let page = scope.reader_changes(since, 2).await.unwrap();
        for (_, change) in &page.changes {
            if let ReaderChange::Book(book) = change {
                seen.push(book.id.clone());
            }
        }
        if !page.more {
            assert_eq!(page.changes.len(), 1);
            break;
        }
        since = page.changes.last().unwrap().0;
    }
    let expected: Vec<String> = books.iter().map(ToString::to_string).collect();
    assert_eq!(seen, expected);

    // "Send everything again" on the website: the next sync starts from 0.
    let next = changes(&server, &secret, 0).await.next;
    assert_eq!(changes(&server, &secret, next).await.books, []);
    let id = server
        .state
        .db
        .list_devices(scope.user_id())
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.reader)
        .unwrap()
        .id;
    assert!(
        server
            .state
            .db
            .request_full_sync(scope.user_id(), id)
            .await
            .unwrap()
    );
    let again = changes(&server, &secret, next).await;
    assert!(again.full);
    assert_eq!(again.books.len(), 3);
    // Only once.
    assert!(!changes(&server, &secret, again.next).await.full);
    // A cursor from the future, as after restoring a backup, starts over too.
    assert!(changes(&server, &secret, next + 1000).await.full);
}
