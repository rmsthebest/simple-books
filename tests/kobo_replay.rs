//! Replays every kind of request a real Kobo made during the device tests. Nickel
//! aborts a whole sync over one bad reply, so none of them may get a surprise.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::Method;
use common::TestServer;
use serde_json::{Value, json};
use uuid::Uuid;

const REQUESTS: &str = include_str!("fixtures/kobo-fw6-requests.txt");
const STATE: &str = include_str!("fixtures/kobo-fw6-state.json");

/// `(method, path)` for each line of the fixture.
fn recorded() -> Vec<(Method, String)> {
    REQUESTS
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split_whitespace().skip(1);
            let method = fields.next().unwrap().parse().unwrap();
            (method, fields.next().unwrap().to_owned())
        })
        .collect()
}

#[test]
fn the_fixture_covers_what_the_server_relies_on() {
    let requests = recorded();
    for (method, path) in [
        (Method::GET, "/v1/initialization"),
        (Method::GET, "/v1/library/sync"),
        (Method::GET, "/v1/library/{book}/metadata"),
        (Method::PUT, "/v1/library/{book}/state"),
        (Method::DELETE, "/v1/library/{book}"),
    ] {
        assert!(
            requests.iter().any(|(m, p)| *m == method && p == path),
            "{method} {path} is missing"
        );
    }
    assert!(requests.iter().any(|(_, p)| p.starts_with("/download/")));
    assert!(requests.iter().any(|(_, p)| p.contains("{cover}")));
    assert!(requests.iter().any(|(_, p)| p.contains("{foreign-cover}")));
}

#[tokio::test]
async fn every_recorded_request_gets_a_reply_nickel_accepts() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "a book").await;
    let cover = scope
        .library_book(book)
        .await
        .unwrap()
        .unwrap()
        .cover_id
        .unwrap();
    let state = STATE.replace("{book}", &book.to_string());

    for (method, shape) in recorded() {
        let path = shape
            .replace("{book}", &book.to_string())
            .replace("{cover}", &cover)
            .replace("{foreign-cover}", &format!("{}-1700000000", Uuid::new_v4()))
            .replace("{id}", &Uuid::new_v4().to_string());
        let url = format!("/kobo/{token}{path}");
        let body = match method {
            Method::GET | Method::HEAD => Vec::new(),
            Method::PUT if shape.ends_with("/state") => state.clone().into_bytes(),
            _ => b"{}".to_vec(),
        };
        let reply = server.request(method.clone(), &url, &[], body).await;
        let what = format!("{method} {shape}");

        let content_type = reply.header("content-type").unwrap_or_default().to_owned();
        assert!(!content_type.contains("html"), "{what}: {content_type}");
        assert!(!reply.body.starts_with(b"<"), "{what}: looks like HTML");
        assert!(reply.header("content-length").is_some(), "{what}");

        if shape.contains("{foreign-cover}") {
            // Covers it remembers from another server: missing, but still JSON.
            assert_eq!(reply.status, 404, "{what}");
            assert_eq!(reply.json(), json!({}), "{what}");
        } else {
            assert!(reply.status.is_success(), "{what}: {}", reply.status);
        }
        if shape.starts_with("/download/") || shape.contains("{cover}") {
            assert!(!reply.body.is_empty(), "{what}");
        } else if !shape.contains("{foreign-cover}") && !reply.body.is_empty() {
            serde_json::from_slice::<Value>(&reply.body)
                .unwrap_or_else(|e| panic!("{what}: not JSON: {e}"));
        }
    }

    assert!(
        scope.library_book(book).await.unwrap().is_some(),
        "a device's DELETE must not remove the server's copy"
    );
}

#[tokio::test]
async fn a_recorded_position_comes_back_as_it_went_in() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "a book").await;
    let path = format!("/kobo/{token}/v1/library/{book}/state");

    let body = STATE.replace("{book}", &book.to_string()).into_bytes();
    let reply = server.request(Method::PUT, &path, &[], body).await;
    assert_eq!(reply.status, 200);

    let stored = server.get(&path).await.json();
    let sent: Value = serde_json::from_str(STATE).unwrap();
    let sent = &sent["ReadingStates"][0];
    assert_eq!(stored[0]["CurrentBookmark"], sent["CurrentBookmark"]);
    assert_eq!(stored[0]["EntitlementId"], book.to_string());
}

/// These files end up in a public repository.
#[test]
fn fixtures_hold_no_addresses_or_tokens() {
    for text in [REQUESTS, STATE] {
        assert!(!text.contains("192.168."));
        assert!(!text.contains("http"));
        for word in text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
            assert!(word.len() < 40, "a long opaque value: {} chars", word.len());
        }
    }
}
