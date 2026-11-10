//! Secrets must never reach the logs, even with protocol tracing on.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::Method;
use common::TestServer;
use serde_json::json;
use simple_books::server::db::scope::UserScope;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Installs a Kobo as the device page does, returning its secret.
async fn install_from_website(server: &TestServer, scope: &UserScope) -> String {
    let cookie = server.cookie(scope).await;
    let installed = server
        .request(
            Method::POST,
            "/device/kobo-install",
            &[("cookie", &cookie), ("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await
        .json();
    installed["secret"].as_str().unwrap().to_owned()
}

// One run through every way a secret comes in, so one log to search.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn tokens_never_reach_the_logs() {
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();

    let server = TestServer::start_with(&[("KOBO_TRACE", "true")]).await;
    let (scope, token) = server.user("sam").await;
    let book = server.add_book(&scope, "Book").await;

    let bearer = "bearer-secret-value";
    server
        .get(&format!("/kobo/{token}/v1/initialization"))
        .await;
    server
        .request(
            Method::POST,
            &format!("/kobo/{token}/v1/auth/device"),
            &[("authorization", &format!("Bearer {bearer}"))],
            json!({ "UserKey": "k" }).to_string().into_bytes(),
        )
        .await;
    server.sync(&token, None).await;
    server
        .get(&format!("/kobo/{token}/v1/library/{book}/metadata"))
        .await;
    server
        .get(&format!("/kobo/{token}/download/{book}/kepub"))
        .await;
    server.get(&format!("/kobo/{token}/v1/unknown")).await;
    server
        .get(&format!("/kobo/wrong-{token}/v1/library/sync"))
        .await;

    // A simple-books reader pairing and syncing.
    let start = server
        .request(
            Method::POST,
            "/device/v1/pair",
            &[("content-type", "application/json")],
            json!({ "name": "Reader" }).to_string().into_bytes(),
        )
        .await
        .json();
    let secret = start["secret"].as_str().unwrap().to_owned();
    let code = start["code"].as_str().unwrap().replace('-', "");
    let reader_auth = format!("Bearer {secret}");
    server
        .request(
            Method::GET,
            "/device/v1/pair",
            &[("authorization", &reader_auth)],
            Vec::new(),
        )
        .await;
    server
        .state
        .db
        .finish_pairing(scope.user_id(), &code, simple_books::server::now())
        .await
        .unwrap();
    for path in [
        "/device/v1/changes?since=0".to_owned(),
        format!("/device/v1/books/{book}"),
    ] {
        server
            .request(
                Method::GET,
                &path,
                &[("authorization", &reader_auth)],
                Vec::new(),
            )
            .await;
    }

    let installed = install_from_website(&server, &scope).await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("installed from the website"),
        "the install should be logged"
    );
    assert!(
        !logs.contains(&installed),
        "an installed reader's secret reached the logs"
    );
    assert!(
        logs.contains("kobo_trace"),
        "tracing should be on for this test to mean anything"
    );
    assert!(
        logs.contains("/kobo/{token}/v1/library/sync"),
        "paths should be logged, redacted"
    );
    assert!(!logs.contains(&token), "a device token reached the logs");
    assert!(
        logs.contains("/device/v1/changes"),
        "reader requests should be logged"
    );
    assert!(
        !logs.contains(&secret),
        "a reader's secret reached the logs"
    );
    assert!(!logs.contains(&code), "a pairing code reached the logs");
    assert!(
        !logs.contains(bearer),
        "an Authorization header reached the logs"
    );
}
