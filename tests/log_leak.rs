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

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
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
        !logs.contains(bearer),
        "an Authorization header reached the logs"
    );
}
