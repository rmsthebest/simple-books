//! Exercises the Kobo surface the way Nickel does, against the real router.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{PUBLIC_URL, TestServer, entry_id};
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_wire_rules(reply: &common::Reply, what: &str) {
    let length = reply
        .header("content-length")
        .unwrap_or_else(|| panic!("{what}: no Content-Length"));
    assert_eq!(
        length.parse::<usize>().unwrap(),
        reply.body.len(),
        "{what}: Content-Length mismatch"
    );
    assert!(
        reply.header("transfer-encoding").is_none(),
        "{what}: must not be chunked"
    );
    assert!(
        reply.header("content-encoding").is_none(),
        "{what}: must not be compressed"
    );
    let cache = reply.header("cache-control").unwrap_or_default();
    assert!(
        cache.contains("no-transform"),
        "{what}: missing no-transform, got {cache:?}"
    );
    assert!(
        cache.contains("no-store"),
        "{what}: missing no-store, got {cache:?}"
    );
}

fn assert_json_reply(reply: &common::Reply, what: &str) {
    assert_wire_rules(reply, what);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8"),
        "{what}"
    );
    let _ = reply.json();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One long story, told in order, is the point.
async fn full_device_flow() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let base = format!("{PUBLIC_URL}/kobo/{token}");
    let book = server.add_book(&scope, "First Book").await;

    // Initialization points the device back at this server.
    let init = server
        .get(&format!("/kobo/{token}/v1/initialization"))
        .await;
    assert_eq!(init.status, 200);
    assert_json_reply(&init, "initialization");
    assert_eq!(init.header("x-kobo-apitoken"), Some("e30="));
    assert_eq!(
        init.json()["Resources"]["library_sync"],
        format!("{base}/v1/library/sync")
    );

    // Auth echoes the user key and returns placeholder tokens.
    let auth = server
        .request(
            Method::POST,
            &format!("/kobo/{token}/v1/auth/device"),
            &[],
            json!({ "UserKey": "abc" }).to_string().into_bytes(),
        )
        .await;
    assert_json_reply(&auth, "auth");
    assert_eq!(auth.json()["UserKey"], "abc");
    assert!(
        auth.json()["AccessToken"]
            .as_str()
            .is_some_and(|t| !t.is_empty())
    );

    // First sync: the book arrives as new, with a token and no continuation.
    let sync = server.sync(&token, None).await;
    assert_json_reply(&sync, "sync");
    assert!(sync.header("x-kobo-sync").is_none());
    let next = sync.header("x-kobo-synctoken").unwrap().to_owned();
    let entries = sync.json();
    let entry = &entries[0]["NewEntitlement"];
    assert_eq!(entry["BookEntitlement"]["Id"], book.to_string());
    assert!(entry["BookEntitlement"]["ActivePeriod"]["From"].is_string());
    assert_eq!(entry["BookMetadata"]["Title"], "First Book");

    // Metadata is a one-element array, and its URL is under api_endpoint.
    let metadata = server
        .get(&format!("/kobo/{token}/v1/library/{book}/metadata"))
        .await;
    assert_json_reply(&metadata, "metadata");
    let metadata = metadata.json();
    assert_eq!(
        metadata.as_array().map(Vec::len),
        Some(1),
        "must be an array"
    );
    let download = &metadata[0]["DownloadUrls"][0];
    let url = download["Url"].as_str().unwrap();
    assert!(url.starts_with(&base), "{url} must start with {base}");
    assert_eq!(download["Format"], "KEPUB");

    // The download's size matches what metadata promised.
    let file = server.get(url.strip_prefix(PUBLIC_URL).unwrap()).await;
    assert_eq!(file.status, 200);
    assert_wire_rules(&file, "download");
    assert_eq!(download["Size"].as_u64().unwrap() as usize, file.body.len());
    assert_eq!(file.body, std::fs::read(&server.kepub).unwrap());
    assert_eq!(file.header("x-kobo-apitoken"), Some("e30="));
    assert_eq!(
        file.header("content-type"),
        Some("application/octet-stream")
    );

    // Both cover URL shapes work, whatever size is requested.
    let cover = metadata[0]["CoverImageId"].as_str().unwrap().to_owned();
    for path in [
        format!("/kobo/{token}/covers/{cover}/300/400/80/false/image.jpg"),
        format!("/kobo/{token}/covers/{cover}/300/400/false/image.jpg"),
    ] {
        let image = server.get(&path).await;
        assert_eq!(image.status, 200, "{path}");
        assert_eq!(image.header("content-type"), Some("image/jpeg"));
        assert_wire_rules(&image, "cover");
    }

    // Nothing has changed, so a further sync is empty.
    let idle = server.sync(&token, Some(&next)).await;
    assert_eq!(idle.json(), json!([]));
    let next = idle.header("x-kobo-synctoken").unwrap().to_owned();

    // A reading state update round-trips.
    let put = server
        .request(
            Method::PUT,
            &format!("/kobo/{token}/v1/library/{book}/state"),
            &[],
            json!({ "ReadingStates": [{
                "StatusInfo": { "Status": "Reading" },
                "CurrentBookmark": { "ProgressPercent": 37 },
                "Secret": "dropped",
            }]})
            .to_string()
            .into_bytes(),
        )
        .await;
    assert_json_reply(&put, "put state");
    assert_eq!(put.json()["RequestResult"], "Success");
    assert_eq!(
        put.json()["UpdateResults"][0]["EntitlementId"],
        book.to_string()
    );

    let state = server
        .get(&format!("/kobo/{token}/v1/library/{book}/state"))
        .await;
    assert_eq!(state.json()[0]["CurrentBookmark"]["ProgressPercent"], 37);
    assert!(state.json()[0].get("Secret").is_none());

    let sync = server.sync(&token, Some(&next)).await;
    let entries = sync.json();
    assert_eq!(
        entries[0]["ChangedReadingState"]["ReadingState"]["CurrentBookmark"]["ProgressPercent"],
        37
    );
    let next = sync.header("x-kobo-synctoken").unwrap().to_owned();

    // An edit arrives as a change, with the same IDs so nothing re-downloads.
    scope.set_title(book, "Better Title").await.unwrap();
    let sync = server.sync(&token, Some(&next)).await;
    let entries = sync.json();
    let changed = &entries[0]["ChangedProductMetadata"];
    assert_eq!(changed["BookMetadata"]["Title"], "Better Title");
    assert_eq!(changed["BookEntitlement"]["RevisionId"], book.to_string());
    assert_eq!(changed["BookEntitlement"]["IsRemoved"], false);
    assert!(
        changed.get("ReadingState").is_none(),
        "an edit must not touch the reading position"
    );
    let next = sync.header("x-kobo-synctoken").unwrap().to_owned();

    // A new cover ID, and no other ID change.
    let new_cover = simple_books::server::testkit::replace_cover(
        &scope,
        &server.state.layout,
        book,
        &server.cover,
    )
    .await
    .unwrap();
    let sync = server.sync(&token, Some(&next)).await;
    let entries = sync.json();
    assert_eq!(
        entries[0]["ChangedProductMetadata"]["BookMetadata"]["CoverImageId"],
        new_cover.to_string()
    );
    assert_eq!(
        entries[0]["ChangedProductMetadata"]["BookMetadata"]["RevisionId"],
        book.to_string()
    );
    let next = sync.header("x-kobo-synctoken").unwrap().to_owned();

    // A device-initiated delete changes nothing on the server.
    let removed = server
        .request(
            Method::DELETE,
            &format!("/kobo/{token}/v1/library/{book}"),
            &[],
            Vec::new(),
        )
        .await;
    assert_eq!(removed.status, 200);
    assert_json_reply(&removed, "delete");
    assert!(scope.library_book(book).await.unwrap().is_some());

    // A server-side delete reaches the device, and the files are gone.
    simple_books::server::testkit::delete(&scope, &server.state.layout, book)
        .await
        .unwrap();
    let sync = server.sync(&token, Some(&next)).await;
    let entries = sync.json();
    assert_eq!(
        entries[0]["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"],
        true
    );
    assert_eq!(entry_id(&entries[0]), book.to_string());
    assert!(!server.state.layout.book_dir(book).exists());

    let gone = server.get(url.strip_prefix(PUBLIC_URL).unwrap()).await;
    assert_eq!(gone.status, 404);
    assert_json_reply(&gone, "download of a deleted book");
}

#[tokio::test]
async fn pagination_delivers_every_book_exactly_once() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;

    let mut expected = Vec::new();
    for index in 0..250 {
        expected.push(
            server
                .add_book(&scope, &format!("book {index}"))
                .await
                .to_string(),
        );
    }

    let mut seen = Vec::new();
    let mut pages = 0;
    let mut sync_token: Option<String> = None;
    loop {
        let reply = server.sync(&token, sync_token.as_deref()).await;
        pages += 1;
        let entries = reply.json();
        for entry in entries.as_array().unwrap() {
            assert!(
                entry.get("NewEntitlement").is_some(),
                "all should be new: {entry}"
            );
            seen.push(entry_id(entry));
        }
        sync_token = reply.header("x-kobo-synctoken").map(str::to_owned);
        if reply.header("x-kobo-sync") != Some("continue") {
            break;
        }
    }

    assert_eq!(pages, 3);
    assert_eq!(seen, expected);
    assert_eq!(
        server.sync(&token, sync_token.as_deref()).await.json(),
        json!([])
    );
}

#[tokio::test]
async fn unusable_tokens_force_a_full_sync() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "Only Book").await;

    for bad in ["garbage", "sb1-999-999", "eyJhIjoxfQ==", ""] {
        let entries = server.sync(&token, Some(bad)).await.json();
        assert_eq!(entries.as_array().unwrap().len(), 1, "token {bad:?}");
        assert_eq!(
            entries[0]["NewEntitlement"]["BookEntitlement"]["Id"],
            book.to_string()
        );
    }
}

#[tokio::test]
async fn requested_resync_replays_the_library_once() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "Only Book").await;

    let first = server.sync(&token, None).await;
    let next = first.header("x-kobo-synctoken").unwrap().to_owned();
    assert_eq!(server.sync(&token, Some(&next)).await.json(), json!([]));

    let device = server
        .state
        .db
        .kobo_device_by_token_hash(&simple_books::server::auth::device::hash_secret(&token))
        .await
        .unwrap()
        .unwrap();
    server
        .state
        .db
        .request_full_sync(device.user_id, device.device_id)
        .await
        .unwrap();

    let replay = server.sync(&token, Some(&next)).await;
    assert_eq!(
        replay.json()[0]["NewEntitlement"]["BookEntitlement"]["Id"],
        book.to_string()
    );
    assert_eq!(
        server.sync(&token, Some(&next)).await.json(),
        json!([]),
        "the flag clears after one use"
    );
}

#[tokio::test]
async fn every_unknown_route_answers_json_for_every_method() {
    let server = TestServer::start().await;
    let (_, token) = server.user("sam").await;

    for method in [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
    ] {
        for path in [
            "v1/nothing/here",
            "v1/library/sync",
            "v1/products/x/y/z",
            "",
            "v1/library/not-a-uuid",
        ] {
            let uri = format!("/kobo/{token}/{path}");
            let reply = server.request(method.clone(), &uri, &[], Vec::new()).await;
            assert_ne!(
                reply.status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
            assert!(
                reply.status.is_success(),
                "{method} {path}: {}",
                reply.status
            );
            assert_json_reply(&reply, &format!("{method} {path}"));
        }
    }
}

#[tokio::test]
async fn stubs_answer_in_the_shapes_nickel_parses() {
    let server = TestServer::start().await;
    let (_, token) = server.user("sam").await;

    let cases: [(&str, Value); 5] = [
        ("v1/user/wishlist", json!({ "Items": [], "TotalCount": 0 })),
        ("v1/deals", json!({ "Deals": [] })),
        ("v1/user/loyalty/benefits", json!({ "Benefits": {} })),
        ("v1/products/abc/nextread", json!([])),
        ("v1/analytics/event", json!({})),
    ];
    for (path, expected) in cases {
        let reply = server.get(&format!("/kobo/{token}/{path}")).await;
        assert_json_reply(&reply, path);
        assert_eq!(reply.json(), expected, "{path}");
    }

    let tests = server
        .request(
            Method::POST,
            &format!("/kobo/{token}/v1/analytics/gettests"),
            &[],
            Vec::new(),
        )
        .await;
    assert_eq!(tests.json()["Result"], "Success");
    assert_eq!(tests.json()["TestKey"].as_str().unwrap().len(), 12);
    assert_eq!(
        server
            .get(&format!("/kobo/{token}/v1/user/profile"))
            .await
            .json()["IsOneStore"],
        false
    );
}

#[tokio::test]
async fn bad_tokens_get_json_and_learn_nothing() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "Private").await;

    for path in [
        "/kobo/wrong/v1/initialization".to_owned(),
        "/kobo/wrong/v1/library/sync".to_owned(),
        format!("/kobo/wrong/download/{book}/kepub"),
        "/kobo/wrong/anything".to_owned(),
        format!("/kobo/{token}x/v1/library/sync"),
    ] {
        let reply = server.get(&path).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_json_reply(&reply, &path);
        assert_eq!(reply.json(), json!({}), "{path}");
    }
}

#[tokio::test]
async fn one_user_cannot_reach_anothers_books() {
    let server = TestServer::start().await;
    let (alice, alice_token) = server.user("alice").await;
    let (_, mallory_token) = server.user("mallory").await;
    let book = server.add_book(&alice, "Alice's Book").await;
    let cover = alice
        .library_book(book)
        .await
        .unwrap()
        .unwrap()
        .cover_id
        .unwrap();

    assert_eq!(server.sync(&mallory_token, None).await.json(), json!([]));

    let data_routes = [
        (
            Method::GET,
            format!("/kobo/{mallory_token}/v1/library/{book}/metadata"),
        ),
        (
            Method::GET,
            format!("/kobo/{mallory_token}/v1/library/{book}/state"),
        ),
    ];
    for (method, path) in data_routes {
        let reply = server.request(method, &path, &[], Vec::new()).await;
        assert_eq!(reply.json(), json!({}), "{path}");
    }

    for path in [
        format!("/kobo/{mallory_token}/download/{book}/kepub"),
        format!("/kobo/{mallory_token}/download/{book}/epub"),
        format!("/kobo/{mallory_token}/covers/{cover}/1/1/false/image.jpg"),
    ] {
        let reply = server.get(&path).await;
        assert_eq!(reply.status, 404, "{path}");
        assert_eq!(reply.json(), json!({}), "{path}");
    }

    // Writing another user's reading state must not store anything.
    let body = json!({ "ReadingStates": [{ "CurrentBookmark": { "ProgressPercent": 99 } }] });
    let path = format!("/kobo/{mallory_token}/v1/library/{book}/state");
    let reply = server
        .request(Method::PUT, &path, &[], body.to_string().into_bytes())
        .await;
    assert_eq!(reply.json(), json!({}));
    assert!(alice.reading_state(book).await.unwrap().is_none());

    // Alice is unaffected, and her book is intact.
    let entries = server.sync(&alice_token, None).await.json();
    assert_eq!(
        entries[0]["NewEntitlement"]["BookEntitlement"]["Id"],
        book.to_string()
    );
}

#[tokio::test]
async fn urls_come_from_public_url_and_never_the_host_header() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    server.add_book(&scope, "Book").await;

    for path in ["v1/initialization", "v1/library/sync"] {
        let uri = format!("/kobo/{token}/{path}");
        let reply = server
            .request(
                Method::GET,
                &uri,
                &[("host", "evil.example:1234")],
                Vec::new(),
            )
            .await;
        let text = String::from_utf8(reply.body).unwrap();
        assert!(
            !text.contains("evil.example"),
            "{path} used the Host header"
        );
        assert!(text.contains(PUBLIC_URL), "{path}");
    }
}

#[tokio::test]
async fn range_requests_work_for_resumed_downloads() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "Book").await;

    let path = format!("/kobo/{token}/download/{book}/epub");
    let reply = server
        .request(Method::GET, &path, &[("range", "bytes=0-9")], Vec::new())
        .await;
    assert_eq!(reply.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(reply.body.len(), 10);
    assert_eq!(reply.header("content-length"), Some("10"));
}

#[tokio::test]
async fn malformed_requests_still_get_json() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "Book").await;

    let path = format!("/kobo/{token}/v1/library/{book}/state");
    let reply = server
        .request(Method::PUT, &path, &[], b"not json".to_vec())
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_json_reply(&reply, "malformed state");

    let huge = vec![b'x'; 512 * 1024];
    let reply = server.request(Method::PUT, &path, &[], huge).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_json_reply(&reply, "oversized state");

    let unknown = Uuid::new_v4();
    let reply = server
        .get(&format!("/kobo/{token}/v1/library/{unknown}/metadata"))
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), json!({}));
}

#[tokio::test]
async fn pages_carry_security_headers() {
    let server = TestServer::start().await;
    let reply = server.get("/healthz").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(reply.header("referrer-policy"), Some("same-origin"));
    assert!(reply.header("strict-transport-security").is_some());

    let page = server.get("/login").await;
    assert_eq!(page.status, 200);
    let csp = page
        .header("content-security-policy")
        .expect("page has a CSP");
    let html = String::from_utf8(page.body.clone()).unwrap();
    let nonce = csp
        .split("'nonce-")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert!(
        html.contains(&format!("nonce=\"{nonce}\"")),
        "CSP nonce must match the HTML"
    );
    assert!(page.header("cache-control").unwrap().contains("no-store"));
}
