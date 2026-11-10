//! Installing Simple Books on a Kobo from the website: the files the
//! installer writes, and the secret it pairs the reader with.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{PUBLIC_URL, Reply, TestServer};
use simple_books::server::reader::install::{Installed, READER_NAME};

async fn install(server: &TestServer, cookie: &str) -> Reply {
    server
        .request(
            Method::POST,
            "/device/kobo-install",
            &[("cookie", cookie), ("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await
}

async fn files(server: &TestServer, cookie: &str) -> Reply {
    server
        .request(
            Method::GET,
            "/device/kobo-files",
            &[("cookie", cookie)],
            Vec::new(),
        )
        .await
}

async fn changes(server: &TestServer, secret: &str) -> StatusCode {
    server
        .request(
            Method::GET,
            "/device/v1/changes?since=0",
            &[("authorization", &format!("Bearer {secret}"))],
            Vec::new(),
        )
        .await
        .status
}

#[tokio::test]
async fn installing_pairs_a_reader_that_can_sync() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let cookie = server.cookie(&scope).await;

    let reply = install(&server, &cookie).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let installed: Installed = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(installed.server, PUBLIC_URL);
    assert_eq!(changes(&server, &installed.secret).await, StatusCode::OK);

    // A second install is a second reader, with its own secret.
    let again: Installed = serde_json::from_slice(&install(&server, &cookie).await.body).unwrap();
    assert_ne!(again.secret, installed.secret);
    assert_eq!(changes(&server, &again.secret).await, StatusCode::OK);

    // It's on the device page, by the name the reader gives itself.
    let page = server
        .request(Method::GET, "/device", &[("cookie", &cookie)], Vec::new())
        .await;
    assert!(String::from_utf8(page.body).unwrap().contains(READER_NAME));
}

#[tokio::test]
async fn installing_needs_a_sign_in_from_this_site() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let cookie = server.cookie(&scope).await;

    let signed_out = server
        .request(
            Method::POST,
            "/device/kobo-install",
            &[("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await;
    assert_ne!(signed_out.status, StatusCode::OK);
    let elsewhere = server
        .request(
            Method::POST,
            "/device/kobo-install",
            &[("cookie", &cookie), ("sec-fetch-site", "cross-site")],
            Vec::new(),
        )
        .await;
    assert_eq!(elsewhere.status, StatusCode::FORBIDDEN);
    assert_ne!(
        server.get("/device/kobo-files").await.status,
        StatusCode::OK
    );
    // Neither made a device.
    let page = server
        .request(Method::GET, "/device", &[("cookie", &cookie)], Vec::new())
        .await;
    assert!(!String::from_utf8(page.body).unwrap().contains(READER_NAME));
}

#[tokio::test]
async fn the_files_are_served_as_a_tar_once_built() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("sam").await;
    let cookie = server.cookie(&scope).await;

    let missing = files(&server, &cookie).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert!(
        String::from_utf8(missing.body)
            .unwrap()
            .contains("build-kobo-files.sh")
    );

    let dir = server.dir.path().join("kobo-files");
    std::fs::create_dir_all(dir.join(".adds/simple-books")).unwrap();
    std::fs::create_dir_all(dir.join(".kobo")).unwrap();
    std::fs::write(dir.join(".adds/simple-books/reader-kobo"), b"reader").unwrap();
    std::fs::write(dir.join(".kobo/Kobo.tgz"), b"plugin").unwrap();

    let reply = files(&server, &cookie).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), Some("application/x-tar"));
    let entries = kobo_files::read(&reply.body).unwrap();
    let found: Vec<(&str, &[u8])> = entries.iter().map(|e| (e.path.as_str(), e.bytes)).collect();
    assert_eq!(
        found,
        [
            (".adds/simple-books/reader-kobo", &b"reader"[..]),
            (".kobo/Kobo.tgz", &b"plugin"[..]),
        ]
    );
}
