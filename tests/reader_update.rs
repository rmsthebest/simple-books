//! A paired reader updating itself to the Kobo files the server has, over a
//! socket: only what changed is downloaded, then put in place.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::{fs, net::SocketAddr, sync::atomic::AtomicBool};

use common::TestServer;
use reader_sync::{Client, update};
use simple_books::server::reader::install;

async fn listen(server: &TestServer) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = server
        .router
        .clone()
        .into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reader_downloads_what_changed_and_puts_it_in_place() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let installed = install::install(&server.state.db, &url, scope.user_id())
        .await
        .unwrap();

    // What the server would install: v2.
    let files = server.dir.path().join("kobo-files");
    let offered = files.join(".adds/simple-books");
    fs::create_dir_all(&offered).unwrap();
    fs::create_dir_all(files.join(".kobo")).unwrap();
    fs::write(files.join(".kobo/Kobo.tgz"), b"plugin, packed").unwrap();
    fs::write(offered.join("reader-kobo"), b"reader 2").unwrap();
    fs::write(offered.join("dict-en-sv"), b"dictionary").unwrap();
    fs::write(offered.join("version"), b"v2\n").unwrap();

    // The reader has v1, with the same dictionary.
    let reader = tempfile::tempdir().unwrap();
    let app = reader.path();
    fs::write(app.join("reader-kobo"), b"reader 1").unwrap();
    fs::write(app.join("dict-en-sv"), b"dictionary").unwrap();
    fs::write(app.join("version"), b"v1\n").unwrap();

    let secret = installed.secret.clone();
    let dir = app.to_owned();
    let staged = tokio::task::spawn_blocking(move || {
        let client = Client::new(&url);
        let stop = AtomicBool::new(false);
        let first = update::stage(&client, &secret, &dir, &stop).unwrap();
        // Asked again, it's already there.
        let again = update::stage(&client, &secret, &dir, &stop).unwrap();
        let wrong = update::stage(&client, "not the secret", &dir, &stop);
        (first, again, wrong.is_err())
    })
    .await
    .unwrap();
    assert_eq!(staged.0.as_deref(), Some("v2"));
    assert_eq!(staged.1.as_deref(), Some("v2"));
    assert!(staged.2, "only for paired readers");
    assert!(
        !app.join("update/dict-en-sv").exists(),
        "unchanged, so not downloaded"
    );

    let applied = update::apply(app).unwrap().unwrap();
    assert_eq!(applied.files, ["reader-kobo", "version"]);
    assert_eq!(fs::read(app.join("reader-kobo")).unwrap(), b"reader 2");
    assert_eq!(update::running(app).as_deref(), Some("v2"));
    assert_eq!(update::settled(app).as_deref(), Some("v2"));

    // Up to date now: nothing to stage.
    let secret = installed.secret.clone();
    let url = installed.server.clone();
    let dir = app.to_owned();
    let after = tokio::task::spawn_blocking(move || {
        update::stage(&Client::new(&url), &secret, &dir, &AtomicBool::new(false)).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(after, None);
    assert!(!app.join("update").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_kobo_files_has_no_update() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let installed = install::install(&server.state.db, &url, scope.user_id())
        .await
        .unwrap();
    let reader = tempfile::tempdir().unwrap();
    fs::write(reader.path().join("version"), b"v1\n").unwrap();
    let dir = reader.path().to_owned();
    let staged = tokio::task::spawn_blocking(move || {
        update::stage(
            &Client::new(&url),
            &installed.secret,
            &dir,
            &AtomicBool::new(false),
        )
    })
    .await
    .unwrap();
    assert_eq!(staged.unwrap(), None);
}
