//! Books going in and out through the web routes, with a stand-in for kepubify. The
//! parser's hostile files have their own tests; this checks what the server does
//! with its answers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::{
    fmt::Write as _,
    fs,
    io::{Cursor, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use axum::http::Method;
use common::{Part, TestServer};
use image::{ImageEncoder, Rgb, RgbImage, codecs::png::PngEncoder};
use serde_json::{Value, json};
use simple_books::server::{
    convert::{self, Tools, convert_next, reader},
    db::{reader::ReaderChange, scope::UserScope},
    storage::reconcile,
};
use uuid::Uuid;
use zip::{ZipWriter, write::SimpleFileOptions};

const OPF: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>A Real Book</dc:title>
<dc:creator opf:role="aut">Ann Author</dc:creator>
<dc:publisher>Good Press</dc:publisher>
<dc:language>en</dc:language>
<dc:date>2001-05-17</dc:date>
<dc:description>&lt;p&gt;A &lt;b&gt;good&lt;/b&gt; one&lt;/p&gt;</dc:description>
<meta name="calibre:series" content="The Series"/>
<meta name="calibre:series_index" content="2"/>
</metadata>
<manifest>
<item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
<item id="img" href="cover.png" media-type="image/png" properties="cover-image"/>
</manifest>
<spine><itemref idref="c1"/></spine>
</package>"#;

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = RgbImage::from_pixel(width, height, Rgb([200, 100, 50]));
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn epub_with(opf: &str, cover: &[u8], padding: &[u8]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let entries: [(&str, &[u8]); 5] = [
        ("mimetype", b"application/epub+zip"),
        (
            "META-INF/container.xml",
            br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf"
                media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        ),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/c1.xhtml", b"<html><body>Hello</body></html>"),
        ("OEBPS/cover.png", cover),
    ];
    for (name, body) in entries {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(body).unwrap();
    }
    zip.start_file("OEBPS/padding.bin", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(padding).unwrap();
    zip.finish().unwrap().into_inner()
}

/// A valid EPUB. The salt makes each one a different file.
fn epub(salt: &str) -> Vec<u8> {
    epub_with(OPF, &png(600, 900), salt.as_bytes())
}

fn file_part(bytes: &[u8]) -> Part<'_> {
    Part {
        name: "file",
        filename: Some("upload.epub"),
        body: bytes,
    }
}

fn shared_part() -> Part<'static> {
    Part {
        name: "visibility",
        filename: None,
        body: b"shared",
    }
}

async fn upload(server: &TestServer, cookie: &str, bytes: &[u8]) -> Uuid {
    let reply = server
        .post_form("/books/upload", cookie, &[file_part(bytes)])
        .await;
    assert_eq!(
        reply.status,
        201,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    reply.json()["id"].as_str().unwrap().parse().unwrap()
}

async fn upload_shared(server: &TestServer, cookie: &str, bytes: &[u8]) -> Uuid {
    let reply = server
        .post_form("/books/upload", cookie, &[shared_part(), file_part(bytes)])
        .await;
    assert_eq!(
        reply.status,
        201,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    reply.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Stand-ins for kepubify, kept for the whole test run. A child process writes each
/// one: if this process held it open for writing, a fork in another test could
/// inherit that and make the script fail to run with "text file busy".
fn script(name: &str, body: &str) -> PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    let path = dir.path().join(name);
    if !path.exists() {
        let temporary = dir.path().join(format!("{name}.{}", Uuid::new_v4()));
        let status = std::process::Command::new("sh")
            .args([
                "-c",
                r#"printf '#!/bin/sh\n%s\n' "$1" > "$2" && chmod 755 "$2""#,
                "sh",
            ])
            .arg(body)
            .arg(&temporary)
            .status()
            .unwrap();
        assert!(status.success());
        fs::rename(&temporary, &path).unwrap();
    }
    path
}

fn tools(kepubify: PathBuf) -> Tools {
    let mut tools = Tools::new(kepubify, PathBuf::from(env!("CARGO_BIN_EXE_simple-books")));
    // A script needs a shell and its libraries. The real, static kepubify doesn't.
    tools.extra_read = vec![PathBuf::from("/")];
    tools
}

/// Copies its input to its output: `-o OUT IN`.
fn working() -> Tools {
    tools(script("copy", r#"exec cat "$3" > "$2""#))
}

async fn drain(server: &TestServer, tools: &Tools) {
    while convert_next(&server.state, tools, Some("v-test"))
        .await
        .unwrap()
    {}
}

fn is_empty(path: &Path) -> bool {
    fs::read_dir(path).unwrap().next().is_none()
}

fn assert_nothing_left(server: &TestServer) {
    let layout = &server.state.layout;
    assert!(is_empty(&layout.tmp_dir()), "temp files left behind");
    assert!(is_empty(&layout.books_dir()), "book files left behind");
}

fn sync_ids(reply: &common::Reply) -> Vec<String> {
    reply
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(common::entry_id)
        .collect()
}

#[tokio::test]
async fn an_upload_becomes_a_book_with_what_the_file_says() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    let bytes = epub("x");
    let reply = server
        .post_form("/books/upload", &cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 201);
    let body = reply.json();
    assert_eq!(body["outcome"], "added");
    assert_eq!(body["title"], "A Real Book");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();

    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.title, "A Real Book");
    assert_eq!(book.visibility, "private", "the default");
    assert_eq!(book.authors_json, r#"["Ann Author"]"#);
    assert_eq!(book.series.as_deref(), Some("The Series"));
    assert_eq!(book.series_index, Some(2.0));
    assert_eq!(book.publisher.as_deref(), Some("Good Press"));
    assert_eq!(book.language.as_deref(), Some("en"));
    assert_eq!(book.published_on.as_deref(), Some("2001-05-17"));
    assert_eq!(book.description.as_deref(), Some("A good one"));
    assert_eq!(book.original_size, i64::try_from(bytes.len()).unwrap());
    assert_eq!(book.kepub_state, "pending");

    let layout = &server.state.layout;
    assert_eq!(
        fs::read(layout.original(id)).unwrap(),
        bytes,
        "kept as uploaded"
    );
    let cover: Uuid = book.cover_id.as_deref().unwrap().parse().unwrap();
    assert!(layout.cover(id, cover).exists());
    assert!(layout.thumb(id, cover).exists());
    assert!(is_empty(&layout.tmp_dir()), "the temp file moved");
}

#[tokio::test]
async fn stored_files_are_private() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;
    drain(&server, &working()).await;

    let layout = &server.state.layout;
    let book = scope.library_book(id).await.unwrap().unwrap();
    let cover: Uuid = book.cover_id.as_deref().unwrap().parse().unwrap();
    for path in [
        layout.original(id),
        layout.kepub(id),
        layout.cover(id, cover),
        layout.thumb(id, cover),
    ] {
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", path.display());
    }
    let mode = fs::metadata(layout.book_dir(id))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

/// The whole path a book takes, as far as a server can see: dropped, converted,
/// offered in the sync, downloaded.
#[tokio::test]
async fn an_upload_reaches_the_kobo_only_once_converted() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let early = server.sync(&token, None).await;
    assert_eq!(early.json(), json!([]), "a Kobo needs the final size");

    drain(&server, &working()).await;
    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.kepub_state, "ready");
    assert!(book.kepub_size.is_some());

    let synced = server.sync(&token, None).await.json();
    let entry = &synced[0]["NewEntitlement"];
    assert_eq!(entry["BookMetadata"]["Title"], "A Real Book");
    let url = entry["BookMetadata"]["DownloadUrls"][0]["Url"]
        .as_str()
        .unwrap();
    assert!(url.contains("/kepub"), "{url}");
    assert_eq!(
        entry["BookMetadata"]["DownloadUrls"][0]["Size"].as_i64(),
        book.kepub_size
    );
    let path = url.strip_prefix(common::PUBLIC_URL).unwrap();
    let file = server.get(path).await;
    assert_eq!(file.status, 200);
    assert_eq!(i64::try_from(file.body.len()).ok(), book.kepub_size);
}

#[tokio::test]
async fn a_failed_conversion_falls_back_to_the_epub_after_three_tries() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let bytes = epub("x");
    let id = upload(&server, &cookie, &bytes).await;

    let failing = tools(script("fail", "echo 'it broke' >&2; exit 3"));
    for attempt in 1..=3 {
        assert_eq!(
            server.sync(&token, None).await.json(),
            json!([]),
            "{attempt}"
        );
        assert!(convert_next(&server.state, &failing, None).await.unwrap());
    }
    assert!(
        !convert_next(&server.state, &failing, None).await.unwrap(),
        "done trying"
    );

    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.kepub_state, "failed");

    let synced = server.sync(&token, None).await.json();
    let url = synced[0]["NewEntitlement"]["BookMetadata"]["DownloadUrls"][0]["Url"]
        .as_str()
        .unwrap();
    assert!(url.ends_with("/epub"), "{url}");
    let path = url.strip_prefix(common::PUBLIC_URL).unwrap();
    assert_eq!(server.get(path).await.body, bytes);
}

#[tokio::test]
async fn output_that_isnt_a_book_counts_as_a_failure() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let junk = tools(script("junk", r#"echo rubbish > "$2""#));
    drain(&server, &junk).await;

    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.kepub_state, "failed");
    assert!(!server.state.layout.kepub(id).exists());
    assert!(
        is_empty(&server.state.layout.tmp_dir()),
        "scratch space cleared"
    );
}

#[tokio::test]
async fn a_conversion_that_hangs_is_killed() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let mut hanging = tools(script("hang", "exec sleep 600"));
    hanging.timeout = Duration::from_millis(200);
    let started = std::time::Instant::now();
    drain(&server, &hanging).await;
    assert!(started.elapsed() < Duration::from_secs(30));

    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.kepub_state, "failed");
}

/// Deleted while converting: there's nothing to finish, and no file may reappear.
#[tokio::test]
async fn a_book_deleted_during_conversion_stays_deleted() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let dir = server.state.layout.book_dir(id);
    let slow = tools(script("slow", r#"sleep 1; exec cat "$3" > "$2""#));
    let state = server.state.clone();
    let worker = tokio::spawn(async move { convert_next(&state, &slow, None).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let delete = format!("/books/{id}/delete");
    let reply = server
        .request(
            Method::POST,
            &delete,
            &[("cookie", &cookie), ("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await;
    assert_eq!(reply.status, 303);
    worker.await.unwrap().unwrap();

    assert!(!dir.exists(), "no files came back");
    assert!(scope.readable_book(id).await.unwrap().is_none());
    assert_eq!(server.sync(&token, None).await.json(), json!([]));
}

/// The sandbox itself, on whatever this kernel supports.
#[tokio::test]
async fn kepubify_runs_limited_with_a_clean_environment_and_cannot_write_outside_its_scratch() {
    if !Path::new("/sys/kernel/security/landlock").exists()
        && !fs::read_to_string("/sys/kernel/security/lsm").is_ok_and(|lsm| lsm.contains("landlock"))
    {
        eprintln!("skipped: no Landlock on this kernel");
        return;
    }

    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let outside = server.dir.path().join("escaped.txt");
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "this test needs a variable that a leak would pass on"
    );
    let body = format!(
        r#"echo "pwned" > "{outside}" 2>/dev/null
[ -z "$CARGO_MANIFEST_DIR" ] || exit 9
[ "$GOMAXPROCS" = 1 ] || exit 9
[ "$(ulimit -c)" = 0 ] || exit 9
[ "$(ulimit -t)" = 300 ] || exit 9
[ "$(ulimit -n)" = 256 ] || exit 9
[ "$(ulimit -f)" != unlimited ] || exit 9
grep -q '^NoNewPrivs:.*1$' /proc/self/status || exit 9
exec cat "$3" > "$2""#,
        outside = outside.display(),
    );
    let attempt = tools(script(&format!("escape-{}", Uuid::new_v4()), &body));
    drain(&server, &attempt).await;

    assert!(!outside.exists(), "wrote outside its scratch directory");
    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(
        book.kepub_state, "ready",
        "and its real work still succeeded"
    );
}

/// Only runs where a real kepubify is available: set `KEPUBIFY_TEST_PATH`.
#[tokio::test]
async fn real_kepubify_converts_under_the_full_sandbox() {
    let Some(path) = std::env::var_os("KEPUBIFY_TEST_PATH") else {
        eprintln!("skipped: KEPUBIFY_TEST_PATH isn't set");
        return;
    };
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    // No extra access: exactly what it gets in production.
    let real = Tools::new(
        PathBuf::from(path),
        PathBuf::from(env!("CARGO_BIN_EXE_simple-books")),
    );
    let version = real.kepubify_version().await;
    assert!(
        version.as_deref().is_some_and(|v| v.starts_with('v')),
        "{version:?}"
    );
    drain(&server, &real).await;

    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.kepub_state, "ready");
    let kepub = fs::read(server.state.layout.kepub(id)).unwrap();
    let parsed = epub::parse(Cursor::new(kepub), &epub::Limits::default()).unwrap();
    assert_eq!(parsed.metadata.title.as_deref(), Some("A Real Book"));
    let mode = fs::metadata(server.state.layout.kepub(id))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "kepubify sets its own mode on its output"
    );
}

#[tokio::test]
async fn hostile_uploads_are_refused_and_leave_nothing_behind() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    let not_a_book = b"<html>hello</html>".repeat(100);
    let doctype = OPF.replace(
        "<package ",
        "<!DOCTYPE p [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><package ",
    );
    let bad_xml = epub_with(&doctype, &png(10, 10), b"");
    for (what, bytes, message) in [
        ("not a zip", not_a_book.as_slice(), "isn't an EPUB"),
        ("entity attack", bad_xml.as_slice(), "unsafe"),
    ] {
        let reply = server
            .post_form("/books/upload", &cookie, &[file_part(bytes)])
            .await;
        assert_eq!(reply.status, 422, "{what}");
        let error = reply.json()["error"].as_str().unwrap().to_owned();
        assert!(error.contains(message), "{what}: {error}");
        assert_nothing_left(&server);
    }
    assert_eq!(scope.used_bytes().await.unwrap(), 0);
}

#[tokio::test]
async fn a_bad_cover_doesnt_stop_a_good_book() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    let bytes = epub_with(OPF, b"not an image at all", b"");
    let id = upload(&server, &cookie, &bytes).await;
    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.cover_id, None);
}

#[tokio::test]
async fn a_book_with_no_title_is_named_after_its_file() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    let opf = OPF.replace("<dc:title>A Real Book</dc:title>", "");
    let bytes = epub_with(&opf, &png(10, 10), b"");
    let reply = server
        .post_form(
            "/books/upload",
            &cookie,
            &[Part {
                name: "file",
                filename: Some("My Holiday Read.epub"),
                body: &bytes,
            }],
        )
        .await;
    assert_eq!(reply.status, 201);
    assert_eq!(reply.json()["title"], "My Holiday Read");
}

#[tokio::test]
async fn uploads_need_a_session_and_the_same_site() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let bytes = epub("x");

    let reply = server
        .post_form("/books/upload", "__Host-sb=forged", &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 401);

    let cross = server
        .request(
            Method::POST,
            "/books/upload",
            &[("cookie", &cookie), ("sec-fetch-site", "cross-site")],
            bytes.clone(),
        )
        .await;
    assert_eq!(cross.status, 403);
    assert_nothing_left(&server);
}

#[tokio::test]
async fn strange_forms_are_refused() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let bytes = epub("x");

    let extra = Part {
        name: "owner_id",
        filename: None,
        body: b"1",
    };
    let bad_visibility = Part {
        name: "visibility",
        filename: None,
        body: b"public",
    };
    for (what, parts) in [
        ("unknown field", vec![extra, file_part(&bytes)]),
        ("bad visibility", vec![bad_visibility, file_part(&bytes)]),
        ("two files", vec![file_part(&bytes), file_part(&bytes)]),
        ("no file", vec![shared_part()]),
        ("empty file", vec![file_part(b"")]),
    ] {
        let reply = server.post_form("/books/upload", &cookie, &parts).await;
        assert_eq!(reply.status, 400, "{what}");
        assert_nothing_left(&server);
    }
}

#[tokio::test]
async fn too_large_for_the_server_is_cut_off_and_cleaned_up() {
    let server = TestServer::start_with(&[("MAX_UPLOAD_MB", "1")]).await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    let bytes = vec![7u8; 2 * 1024 * 1024];
    let reply = server
        .post_form("/books/upload", &cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 413);
    assert_nothing_left(&server);
}

#[tokio::test]
async fn a_nearly_full_disk_refuses_uploads() {
    let server = TestServer::start_with(&[("MIN_FREE_DISK_MB", "999999999999")]).await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let reply = server
        .post_form("/books/upload", &cookie, &[file_part(&epub("x"))])
        .await;
    assert_eq!(reply.status, 507);
    assert_nothing_left(&server);
}

#[tokio::test]
async fn quota_stops_the_upload_and_counts_only_the_uploader() {
    let server = TestServer::start().await;
    let small = server.state.db.create_user("tiny", 100, false).await;
    let small = simple_books::server::testkit::scope(&server.state.db, small.unwrap());
    let cookie = server.cookie(&small).await;

    let reply = server
        .post_form("/books/upload", &cookie, &[file_part(&epub("x"))])
        .await;
    assert_eq!(reply.status, 413);
    assert!(
        reply.json()["error"]
            .as_str()
            .unwrap()
            .contains("storage limit")
    );
    assert_nothing_left(&server);

    // Adding someone else's shared book costs nothing.
    let (ann, _) = server.user("ann").await;
    let ann_cookie = server.cookie(&ann).await;
    let shared = upload_shared(&server, &ann_cookie, &epub("x")).await;
    small.add_to_library(shared).await.unwrap();
    assert_eq!(small.used_bytes().await.unwrap(), 0);
    assert!(ann.used_bytes().await.unwrap() > 0);
}

#[tokio::test]
async fn the_same_file_twice_is_one_book() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let bytes = epub("x");

    let id = upload(&server, &cookie, &bytes).await;
    let used = scope.used_bytes().await.unwrap();

    let reply = server
        .post_form("/books/upload", &cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["outcome"], "already_yours");
    assert_eq!(reply.json()["id"], id.to_string());
    assert_eq!(scope.used_bytes().await.unwrap(), used);
    assert!(is_empty(&server.state.layout.tmp_dir()));
}

#[tokio::test]
async fn sharing_a_file_someone_shares_adds_their_book() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, bob_token) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let bytes = epub("x");

    let id = upload_shared(&server, &ann_cookie, &bytes).await;
    drain(&server, &working()).await;

    let reply = server
        .post_form(
            "/books/upload",
            &bob_cookie,
            &[shared_part(), file_part(&bytes)],
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["outcome"], "added_from_shared");
    assert_eq!(reply.json()["id"], id.to_string());

    assert_eq!(bob.used_bytes().await.unwrap(), 0, "stored nothing new");
    let books = fs::read_dir(server.state.layout.books_dir())
        .unwrap()
        .count();
    assert_eq!(books, 1);
    assert_eq!(
        sync_ids(&server.sync(&bob_token, None).await),
        [id.to_string()]
    );
}

/// "Just for me" means nobody else can take it away.
#[tokio::test]
async fn just_for_me_keeps_a_private_copy_of_a_file_someone_shares() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, bob_token) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let bytes = epub("x");

    let shared = upload_shared(&server, &ann_cookie, &bytes).await;
    let reply = server
        .post_form("/books/upload", &bob_cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 201);
    assert_eq!(reply.json()["outcome"], "added");
    let copy: Uuid = reply.json()["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(copy, shared);

    let row = bob.library_book(copy).await.unwrap().unwrap();
    assert_eq!(row.visibility, "private");
    assert!(server.state.layout.original(copy).exists());
    assert!(
        bob.used_bytes().await.unwrap() > 0,
        "the copy counts against its uploader"
    );
    assert_eq!(bob.library_book(shared).await.unwrap().map(|_| ()), None);

    drain(&server, &working()).await;
    let synced = server.sync(&bob_token, None).await;
    assert_eq!(sync_ids(&synced), [copy.to_string()]);
    let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

    // The shared book going away changes nothing for the private copy.
    assert_eq!(post_delete(&server, shared, &ann_cookie).await, 303);
    assert_eq!(
        server.sync(&bob_token, Some(&cursor)).await.json(),
        json!([])
    );
    assert!(server.state.layout.original(copy).exists());
}

/// Already holding the shared book, a second one would just show up twice.
#[tokio::test]
async fn just_for_me_doesnt_duplicate_a_shared_book_already_held() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let bytes = epub("x");

    let id = upload_shared(&server, &ann_cookie, &bytes).await;
    bob.add_to_library(id).await.unwrap();

    let reply = server
        .post_form("/books/upload", &bob_cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["outcome"], "already_yours");
    assert_eq!(reply.json()["id"], id.to_string());
    assert_eq!(bob.used_bytes().await.unwrap(), 0);
}

/// An easy slip with drag and drop, and neither upload should look like a failure.
#[tokio::test]
async fn dropping_the_same_file_twice_at_once_makes_one_book() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    for round in 0..8 {
        let bytes = epub(&format!("round {round}"));
        let part = [file_part(&bytes)];
        let (one, two) = tokio::join!(
            server.post_form("/books/upload", &cookie, &part),
            server.post_form("/books/upload", &cookie, &part),
        );
        let mut statuses = [one.status.as_u16(), two.status.as_u16()];
        statuses.sort_unstable();
        assert_eq!(statuses, [200, 201], "round {round}");
        assert_eq!(one.json()["id"], two.json()["id"]);
    }
    let books = fs::read_dir(server.state.layout.books_dir())
        .unwrap()
        .count();
    assert_eq!(books, 8, "no directory left over from the loser");
    assert!(is_empty(&server.state.layout.tmp_dir()));
}

/// Nothing may reveal that somebody else privately holds the same file.
#[tokio::test]
async fn someone_elses_private_copy_stays_invisible() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let bytes = epub("x");

    let private = upload(&server, &ann_cookie, &bytes).await;
    let reply = server
        .post_form("/books/upload", &bob_cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 201);
    assert_eq!(reply.json()["outcome"], "added");
    assert_ne!(reply.json()["id"], private.to_string());
}

/// Sharing a file someone else already shares lands on the existing book, not a clash.
#[tokio::test]
async fn sharing_a_file_someone_already_shares_just_adds_it() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let bytes = epub("x");

    let id = upload_shared(&server, &ann_cookie, &bytes).await;
    let reply = server
        .post_form(
            "/books/upload",
            &bob_cookie,
            &[shared_part(), file_part(&bytes)],
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["outcome"], "added_from_shared");
    assert_eq!(reply.json()["id"], id.to_string());
}

async fn web_get(server: &TestServer, path: &str, cookie: &str) -> common::Reply {
    server
        .request(Method::GET, path, &[("cookie", cookie)], Vec::new())
        .await
}

#[tokio::test]
async fn downloads_follow_the_permission_rules() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let admin = server.admin("root").await.0;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let admin_cookie = server.cookie(&admin).await;

    let private_bytes = epub("private");
    let private = upload(&server, &ann_cookie, &private_bytes).await;
    let shared = upload_shared(&server, &ann_cookie, &epub("shared")).await;

    let path = format!("/books/{private}/download");
    let own = web_get(&server, &path, &ann_cookie).await;
    assert_eq!(own.status, 200);
    assert_eq!(own.body, private_bytes, "exactly as uploaded");
    assert_eq!(own.header("content-type"), Some("application/epub+zip"));
    let disposition = own.header("content-disposition").unwrap();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(
        disposition.contains("A%20Real%20Book.epub"),
        "{disposition}"
    );
    assert_eq!(own.header("x-content-type-options"), Some("nosniff"));
    assert!(own.header("cache-control").unwrap().contains("no-store"));

    for (who, cookie) in [("bob", &bob_cookie), ("admin", &admin_cookie)] {
        assert_eq!(web_get(&server, &path, cookie).await.status, 404, "{who}");
    }
    assert_eq!(server.get(&path).await.status, 401, "no session");

    let path = format!("/books/{shared}/download");
    for cookie in [&ann_cookie, &bob_cookie, &admin_cookie] {
        assert_eq!(web_get(&server, &path, cookie).await.status, 200);
    }
}

#[tokio::test]
async fn covers_are_served_to_those_who_can_read_the_book() {
    let server = TestServer::start().await;
    let (ann, ann_token) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;

    let id = upload(&server, &ann_cookie, &epub_with(OPF, &png(1200, 1800), b"")).await;
    let row = ann.library_book(id).await.unwrap().unwrap();
    let cover = row.cover_id.unwrap();

    let thumb = web_get(
        &server,
        &format!("/books/{id}/cover/{cover}/thumb.jpg"),
        &ann_cookie,
    )
    .await;
    let full = web_get(
        &server,
        &format!("/books/{id}/cover/{cover}/full.jpg"),
        &ann_cookie,
    )
    .await;
    assert_eq!(thumb.status, 200);
    assert_eq!(full.status, 200);
    assert!(thumb.body.len() < full.body.len());
    assert_eq!(thumb.header("content-type"), Some("image/jpeg"));
    assert!(thumb.header("cache-control").unwrap().contains("private"));
    assert!(thumb.header("cache-control").unwrap().contains("immutable"));

    for path in [
        format!("/books/{id}/cover/{cover}/other.jpg"),
        format!("/books/{id}/cover/{cover}/../../original.epub"),
        format!("/books/{id}/cover/{}/full.jpg", Uuid::new_v4()),
    ] {
        assert_eq!(
            web_get(&server, &path, &ann_cookie).await.status,
            404,
            "{path}"
        );
    }
    let path = format!("/books/{id}/cover/{cover}/full.jpg");
    assert_eq!(web_get(&server, &path, &bob_cookie).await.status, 404);

    // A Kobo gets the smaller file when that's enough, and never an upscale.
    drain(&server, &working()).await;
    let small = server
        .get(&format!(
            "/kobo/{ann_token}/covers/{cover}/149/223/70/False/image.jpg"
        ))
        .await;
    assert_eq!(small.body, thumb.body);
    let edge = server
        .get(&format!(
            "/kobo/{ann_token}/covers/{cover}/355/530/80/False/image.jpg"
        ))
        .await;
    assert_eq!(edge.body, thumb.body);
    let large = server
        .get(&format!(
            "/kobo/{ann_token}/covers/{cover}/1072/1448/80/False/image.jpg"
        ))
        .await;
    assert_eq!(large.body, full.body);
}

#[tokio::test]
async fn replacing_a_cover() {
    let server = TestServer::start().await;
    let (ann, ann_token) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let admin = server.admin("root").await.0;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let admin_cookie = server.cookie(&admin).await;
    let layout = &server.state.layout;

    let id = upload_shared(&server, &ann_cookie, &epub("x")).await;
    drain(&server, &working()).await;
    let old: Uuid = ann
        .library_book(id)
        .await
        .unwrap()
        .unwrap()
        .cover_id
        .unwrap()
        .parse()
        .unwrap();
    let synced = server.sync(&ann_token, None).await;
    let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

    let image = png(300, 450);
    let path = format!("/books/{id}/cover");

    // Refused before any image work, and for the right reason.
    let reply = server
        .post_form(&path, &bob_cookie, &[file_part(&image)])
        .await;
    assert_eq!(reply.status, 403);

    // Not an image: refused, and the old cover stays.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
    let reply = server
        .post_form(&path, &ann_cookie, &[file_part(svg)])
        .await;
    assert_eq!(reply.status, 422);
    assert!(layout.cover(id, old).exists());

    let reply = server
        .post_form(&path, &ann_cookie, &[file_part(&image)])
        .await;
    assert_eq!(reply.status, 200);
    let new: Uuid = reply.json()["cover_id"].as_str().unwrap().parse().unwrap();
    assert_ne!(new, old);
    assert!(layout.cover(id, new).exists());
    assert!(layout.thumb(id, new).exists());
    assert!(!layout.cover(id, old).exists(), "the old one is gone");
    assert!(!layout.thumb(id, old).exists());

    // A Kobo hears about it, as an edit.
    let reply = server.sync(&ann_token, Some(&cursor)).await;
    let entries: Value = reply.json();
    assert_eq!(
        entries[0]["ChangedProductMetadata"]["BookMetadata"]["CoverImageId"],
        new.to_string()
    );

    // Admins can fix shared books.
    let reply = server
        .post_form(&path, &admin_cookie, &[file_part(&image)])
        .await;
    assert_eq!(reply.status, 200);
}

async fn post_delete(server: &TestServer, id: Uuid, cookie: &str) -> u16 {
    server
        .request(
            Method::POST,
            &format!("/books/{id}/delete"),
            &[("cookie", cookie), ("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await
        .status
        .as_u16()
}

#[tokio::test]
async fn deleting_follows_the_rules_and_removes_the_files() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, bob_token) = server.user("bob").await;
    let admin = server.admin("root").await.0;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let admin_cookie = server.cookie(&admin).await;
    let layout = &server.state.layout;

    let private = upload(&server, &ann_cookie, &epub("private")).await;
    assert_eq!(post_delete(&server, private, &bob_cookie).await, 404);
    assert_eq!(post_delete(&server, private, &admin_cookie).await, 404);
    assert!(layout.book_dir(private).exists());
    assert_eq!(post_delete(&server, private, &ann_cookie).await, 303);
    assert!(!layout.book_dir(private).exists());
    assert_eq!(
        post_delete(&server, private, &ann_cookie).await,
        404,
        "already gone"
    );

    let shared = upload_shared(&server, &ann_cookie, &epub("shared")).await;
    drain(&server, &working()).await;
    bob.add_to_library(shared).await.unwrap();
    let synced = server.sync(&bob_token, None).await;
    let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

    assert_eq!(post_delete(&server, shared, &bob_cookie).await, 403);
    assert!(layout.book_dir(shared).exists());
    assert_eq!(post_delete(&server, shared, &admin_cookie).await, 303);
    assert!(!layout.book_dir(shared).exists());

    let reply = server.sync(&bob_token, Some(&cursor)).await.json();
    assert_eq!(
        reply[0]["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"],
        true
    );
}

#[tokio::test]
async fn startup_repairs_what_a_crash_can_leave() {
    let server = TestServer::start().await;
    let (scope, token): (UserScope, String) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let layout = &server.state.layout;
    let db = &server.state.db;

    let id = upload(&server, &cookie, &epub("x")).await;
    drain(&server, &working()).await;
    let book = scope.library_book(id).await.unwrap().unwrap();
    let cover: Uuid = book.cover_id.as_deref().unwrap().parse().unwrap();

    // Stray things a crash could leave.
    fs::write(layout.tmp_dir().join("half.upload"), b"x").unwrap();
    let orphan = layout.book_dir(Uuid::new_v4());
    fs::create_dir(&orphan).unwrap();
    fs::write(orphan.join("original.epub"), b"x").unwrap();
    let old_cover = layout.cover(id, Uuid::new_v4());
    fs::write(&old_cover, b"x").unwrap();
    let half_written = layout.book_dir(id).join("cover-1.jpg.tmp");
    fs::write(&half_written, b"x").unwrap();
    // And derived files gone missing.
    fs::remove_file(layout.kepub(id)).unwrap();
    fs::remove_file(layout.thumb(id, cover)).unwrap();

    let report = reconcile::run(layout, db).await.unwrap();
    assert_eq!(report.removed_dirs, 1);
    assert_eq!(report.removed_files, 2);
    assert_eq!(report.requeued_conversions, 1);
    assert_eq!(report.rebuilt_thumbs, 1);
    assert_eq!(report.missing_originals, 0);

    assert!(is_empty(&layout.tmp_dir()));
    assert!(!orphan.exists());
    assert!(!old_cover.exists());
    assert!(!half_written.exists());
    assert!(layout.original(id).exists(), "the book itself is untouched");
    assert!(layout.thumb(id, cover).exists());

    // Still on the Kobo meanwhile, as the EPUB, until the KEPUB is rebuilt.
    let synced = server.sync(&token, None).await.json();
    let url = synced[0]["NewEntitlement"]["BookMetadata"]["DownloadUrls"][0]["Url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(url.ends_with("/epub"), "{url}");

    drain(&server, &working()).await;
    assert!(layout.kepub(id).exists());
    assert_eq!(
        scope.library_book(id).await.unwrap().unwrap().kepub_state,
        "ready"
    );

    // With nothing wrong, it changes nothing.
    let second = reconcile::run(layout, db).await.unwrap();
    assert_eq!(second, reconcile::Report::default());
}

#[tokio::test]
async fn startup_reports_a_missing_original_and_keeps_the_book() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let layout = &server.state.layout;

    let id = upload(&server, &cookie, &epub("x")).await;
    fs::remove_file(layout.original(id)).unwrap();

    let report = reconcile::run(layout, &server.state.db).await.unwrap();
    assert_eq!(report.missing_originals, 1);
    assert!(scope.library_book(id).await.unwrap().is_some());
}

#[tokio::test]
async fn startup_makes_a_new_data_directory_private() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

    let layout = simple_books::server::storage::layout::Layout::new(&root);
    let db = simple_books::server::db::Db::open(&layout.database())
        .await
        .unwrap();
    reconcile::run(&layout, &db).await.unwrap();

    for path in [root.clone(), layout.books_dir(), layout.tmp_dir()] {
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", path.display());
    }
}

/// With no books at all, the likelier story is a lost database, and these files may
/// be the only copy of someone's library.
#[tokio::test]
async fn startup_keeps_book_files_when_the_database_has_no_books() {
    let server = TestServer::start().await;
    let layout = &server.state.layout;
    let dir = layout.book_dir(Uuid::new_v4());
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("original.epub"), b"x").unwrap();

    let report = reconcile::run(layout, &server.state.db).await.unwrap();
    assert_eq!(report, reconcile::Report::default());
    assert!(dir.join("original.epub").exists());
}

/// A real image with a page hidden after it must not survive into what's served.
#[tokio::test]
async fn a_cover_polyglot_loses_its_payload() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let mut image = png(300, 450);
    image.extend_from_slice(b"<html><script>alert(1)</script></html>");
    let reply = server
        .post_form(&format!("/books/{id}/cover"), &cookie, &[file_part(&image)])
        .await;
    assert_eq!(reply.status, 200);
    let cover: Uuid = reply.json()["cover_id"].as_str().unwrap().parse().unwrap();

    let layout = &server.state.layout;
    for path in [layout.cover(id, cover), layout.thumb(id, cover)] {
        let stored = fs::read(path).unwrap();
        assert_eq!(&stored[..3], [0xFF, 0xD8, 0xFF], "a fresh JPEG");
        assert!(!stored.windows(7).any(|window| window == b"<script"));
    }
}

/// Holds requests open with a body that never ends, as a slow connection would.
#[tokio::test]
async fn a_third_upload_at_once_is_refused_for_that_user_only() {
    use axum::body::Body;
    use tower::ServiceExt;

    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;

    let stuck = |cookie: String| {
        let router = server.router.clone();
        tokio::spawn(async move {
            let endless = futures_util::stream::pending::<Result<Vec<u8>, std::io::Error>>();
            let request = axum::http::Request::post("/books/upload")
                .header("cookie", cookie)
                .header("content-type", "multipart/form-data; boundary=b")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from_stream(endless))
                .unwrap();
            router.oneshot(request).await
        })
    };
    let first = stuck(ann_cookie.clone());
    let second = stuck(ann_cookie.clone());
    tokio::time::sleep(Duration::from_millis(200)).await;

    let bytes = epub("x");
    let reply = server
        .post_form("/books/upload", &ann_cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 429);
    let reply = server
        .post_form("/books/upload", &bob_cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 201, "someone else's uploads don't count");

    // A dropped connection gives its place back.
    first.abort();
    second.abort();
    let _ = first.await;
    let _ = second.await;
    let reply = server
        .post_form("/books/upload", &ann_cookie, &[file_part(&bytes)])
        .await;
    assert_eq!(reply.status, 201);
}

/// The real background worker, woken by the upload rather than by its idle recheck.
#[tokio::test]
async fn the_worker_converts_an_upload_without_being_asked() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;

    let worker = tokio::spawn(convert::run(server.state.clone(), working()));
    // Let it reach its idle wait, so that only the upload can wake it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let mut state = String::new();
    for _ in 0..100 {
        state = scope.library_book(id).await.unwrap().unwrap().kepub_state;
        if state == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    worker.abort();
    assert_eq!(state, "ready");
}

#[tokio::test]
async fn uploading_a_shared_file_again_changes_nothing() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, bob_token) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;
    let bytes = epub("x");

    upload_shared(&server, &ann_cookie, &bytes).await;
    drain(&server, &working()).await;
    let form = [shared_part(), file_part(&bytes)];
    assert_eq!(
        server
            .post_form("/books/upload", &bob_cookie, &form)
            .await
            .status,
        200
    );
    let synced = server.sync(&bob_token, None).await;
    assert_eq!(synced.json().as_array().unwrap().len(), 1);
    let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

    assert_eq!(
        server
            .post_form("/books/upload", &bob_cookie, &form)
            .await
            .status,
        200
    );
    let again = server.sync(&bob_token, Some(&cursor)).await.json();
    assert_eq!(again, json!([]), "nothing new for the Kobo");
}

/// The shared page will show covers of books people haven't added yet.
#[tokio::test]
async fn a_shared_books_cover_is_visible_before_adding_it() {
    let server = TestServer::start().await;
    let (ann, _) = server.user("ann").await;
    let (bob, _) = server.user("bob").await;
    let ann_cookie = server.cookie(&ann).await;
    let bob_cookie = server.cookie(&bob).await;

    let id = upload_shared(&server, &ann_cookie, &epub("x")).await;
    let cover = ann
        .library_book(id)
        .await
        .unwrap()
        .unwrap()
        .cover_id
        .unwrap();
    assert_eq!(bob.library_book(id).await.unwrap().map(|_| ()), None);

    let path = format!("/books/{id}/cover/{cover}/thumb.jpg");
    assert_eq!(web_get(&server, &path, &bob_cookie).await.status, 200);
}

/// kepubify sets the mode of what it writes, and that mustn't reach the disk.
#[tokio::test]
async fn converted_files_are_private_whatever_mode_the_converter_chose() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let open = tools(script("open-mode", r#"cat "$3" > "$2"; chmod 644 "$2""#));
    drain(&server, &open).await;

    assert_eq!(
        scope.library_book(id).await.unwrap().unwrap().kepub_state,
        "ready"
    );
    let mode = fs::metadata(server.state.layout.kepub(id))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// A compromised converter can't read outside its sandbox, but the server can. If
/// the server followed a link here it would hand out whatever the link points at,
/// and another user's book would even pass the check that the output is an EPUB.
#[tokio::test]
async fn a_converter_cant_pass_off_a_link_as_its_output() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &epub("x")).await;

    let linking = tools(script("link", r#"exec ln -s "$3" "$2""#));
    drain(&server, &linking).await;

    assert_eq!(
        scope.library_book(id).await.unwrap().unwrap().kepub_state,
        "failed"
    );
    let kepub = server.state.layout.kepub(id);
    assert!(
        fs::symlink_metadata(kepub).is_err(),
        "no link was installed"
    );
}

/// Roughly what real books look like, as opposed to what's convenient: EPUB 2 with an
/// NCX, spaces and escapes in paths, entities, non-ASCII text, nested markup, images.
fn messy_epub() -> Vec<u8> {
    let image = png(400, 300);
    let chapters = 30;
    let mut manifest = String::new();
    let mut spine = String::new();
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default();
    zip.start_file("mimetype", stored).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    zip.start_file("META-INF/container.xml", deflated).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    )
    .unwrap();
    for n in 1..=chapters {
        write!(
            manifest,
            r#"<item id="c{n}" href="Text/Chapter%20{n:02}.xhtml" media-type="application/xhtml+xml"/>"#
        )
        .unwrap();
        write!(spine, r#"<itemref idref="c{n}"/>"#).unwrap();
        zip.start_file(format!("OPS/Text/Chapter {n:02}.xhtml"), deflated)
            .unwrap();
        zip.write_all(format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Kapitel {n}</title>
<link rel="stylesheet" type="text/css" href="../Styles/main.css"/></head>
<body><h1 class="t">Kapitel {n} &#8212; Größe</h1>
<p>«Déjà vu», said Åse. 日本語のテキスト. <em>Nested <b>markup</b></em> &amp; more.<br/>Second line.</p>
<table><tr><td>cell</td></tr></table>
<p><img src="../Images/pic%201.png" alt="pic"/></p></body></html>"#
        ).as_bytes())
        .unwrap();
    }
    zip.start_file("OPS/Styles/main.css", deflated).unwrap();
    zip.write_all(b"h1.t { font-weight: bold; }").unwrap();
    zip.start_file("OPS/Images/pic 1.png", stored).unwrap();
    zip.write_all(&image).unwrap();
    zip.start_file("OPS/toc.ncx", deflated).unwrap();
    zip.write_all(br#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head/><docTitle><text>x</text></docTitle><navMap><navPoint id="n" playOrder="1"><navLabel><text>One</text></navLabel><content src="Text/Chapter%2001.xhtml"/></navPoint></navMap></ncx>"#).unwrap();
    zip.start_file("OPS/book.opf", deflated).unwrap();
    zip.write_all(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="id" version="2.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:identifier id="id">urn:uuid:7f3f6a52-0d3a-4b53-9a53-3d1c9b1e2a11</dc:identifier>
<dc:title>Größe &amp; Déjà vu</dc:title><dc:creator opf:role="aut">Åse Müller</dc:creator><dc:language>de</dc:language>
<meta name="cover" content="img"/></metadata>
<manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
<item id="css" href="Styles/main.css" media-type="text/css"/>
<item id="img" href="Images/pic%201.png" media-type="image/png"/>{manifest}</manifest>
<spine toc="ncx">{spine}</spine></package>"#
    ).as_bytes())
    .unwrap();
    zip.finish().unwrap().into_inner()
}

/// Only runs where a real kepubify is available: set `KEPUBIFY_TEST_PATH`.
#[tokio::test]
async fn real_kepubify_handles_a_messy_book() {
    let Some(path) = std::env::var_os("KEPUBIFY_TEST_PATH") else {
        eprintln!("skipped: KEPUBIFY_TEST_PATH isn't set");
        return;
    };
    let server = TestServer::start().await;
    let (scope, token) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let id = upload(&server, &cookie, &messy_epub()).await;

    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.title, "Größe & Déjà vu");
    assert_eq!(book.authors_json, r#"["Åse Müller"]"#);
    assert!(book.cover_id.is_some(), "found through the EPUB 2 meta tag");

    let real = Tools::new(
        PathBuf::from(path),
        PathBuf::from(env!("CARGO_BIN_EXE_simple-books")),
    );
    drain(&server, &real).await;
    let book = scope.library_book(id).await.unwrap().unwrap();
    assert_eq!(book.kepub_state, "ready");

    let synced = server.sync(&token, None).await.json();
    let url = synced[0]["NewEntitlement"]["BookMetadata"]["DownloadUrls"][0]["Url"]
        .as_str()
        .unwrap();
    let reply = server
        .get(url.strip_prefix(common::PUBLIC_URL).unwrap())
        .await;

    let mut kepub = zip::ZipArchive::new(Cursor::new(reply.body)).unwrap();
    assert_eq!(
        kepub.by_index(0).unwrap().name().unwrap(),
        "mimetype",
        "must stay first"
    );
    for n in 1..=30 {
        let mut chapter = String::new();
        std::io::Read::read_to_string(
            &mut kepub
                .by_name(&format!("OPS/Text/Chapter {n:02}.xhtml"))
                .unwrap(),
            &mut chapter,
        )
        .unwrap();
        assert!(chapter.contains("koboSpan"), "chapter {n} wasn't converted");
        assert!(chapter.contains("Größe"), "chapter {n} lost its text");
        assert!(chapter.contains("日本語"), "chapter {n} lost its text");
    }
    let mut picture = Vec::new();
    std::io::Read::read_to_end(
        &mut kepub.by_name("OPS/Images/pic 1.png").unwrap(),
        &mut picture,
    )
    .unwrap();
    assert_eq!(picture, png(400, 300), "images come through untouched");
}

/// A browser with no JavaScript posts this form, and gets sent back to the page.
#[tokio::test]
async fn a_plain_form_upload_comes_back_to_the_library() {
    let server = TestServer::start().await;
    let (scope, _) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let bytes = epub("x");

    let mut body = Vec::new();
    body.extend_from_slice(
        b"--b\r\nContent-Disposition: form-data; name=\"visibility\"\r\n\r\nprivate\r\n--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.epub\"\r\n\r\n",
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(b"\r\n--b--\r\n");
    let reply = server
        .request(
            Method::POST,
            "/books/upload",
            &[
                ("cookie", &cookie),
                ("content-type", "multipart/form-data; boundary=b"),
                ("accept", "text/html,application/xhtml+xml"),
                ("sec-fetch-site", "same-origin"),
            ],
            body,
        )
        .await;
    assert_eq!(reply.status, 303);
    assert_eq!(reply.header("location"), Some("/?done=uploaded"));
    let page = web_get(&server, "/?done=uploaded", &cookie).await;
    let html = String::from_utf8(page.body).unwrap();
    assert!(html.contains("A Real Book"));
    assert!(html.contains("tap sync"));
    assert!(html.contains("Getting it ready"), "until it's converted");
}

async fn drain_reader(server: &TestServer, tools: &Tools) -> usize {
    let mut attempts = 0;
    while reader::convert_next(&server.state, tools).await.unwrap() {
        attempts += 1;
    }
    attempts
}

/// The real thing, as a child of the test binary, with its limits and all.
#[tokio::test]
async fn books_are_converted_for_the_reader_after_their_kepub() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    let layout = &server.state.layout;
    let id = upload(&server, &cookie, &epub("x")).await;

    assert_eq!(
        drain_reader(&server, &working()).await,
        0,
        "the KEPUB goes first"
    );
    drain(&server, &working()).await;
    assert_eq!(drain_reader(&server, &working()).await, 1);
    assert!(is_empty(&layout.tmp_dir()), "no scratch left behind");

    let file = fs::File::open(layout.reader(id)).unwrap();
    let mut book = book_format::BookReader::open(file, book_format::Limits::default()).unwrap();
    assert_eq!(book.manifest().title, "A Real Book");
    let chapter = book.chapter(0).unwrap();
    assert_eq!(chapter.blocks[0].spans[0].text, "Hello");
    let mode = fs::metadata(layout.reader(id))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // Readers check what they download against its hash.
    let changes = scope.reader_changes(0, 10).await.unwrap();
    let Some((_, ReaderChange::Book(listed))) = changes.changes.first() else {
        panic!("the book should be listed");
    };
    let bytes = fs::read(layout.reader(id)).unwrap();
    assert_eq!(
        listed.reader_sha256.as_deref(),
        Some(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes)).as_str())
    );
    assert_eq!(listed.reader_size, Some(bytes.len() as i64));

    assert_eq!(drain_reader(&server, &working()).await, 0, "done once");
    let synced = server.sync(&token, None).await.json();
    assert_eq!(
        synced.as_array().unwrap().len(),
        1,
        "the Kobo sync is as before"
    );

    // Gone from disk: noticed at startup, and made again.
    fs::remove_file(layout.reader(id)).unwrap();
    let report = reconcile::run(layout, &server.state.db).await.unwrap();
    assert_eq!(report.requeued_reader_conversions, 1);
    assert_eq!(drain_reader(&server, &working()).await, 1);
    assert!(layout.reader(id).exists());

    // Deleted with the book.
    assert_eq!(post_delete(&server, id, &cookie).await, 303);
    assert_nothing_left(&server);
}

#[tokio::test]
async fn a_book_the_reader_cant_have_still_reaches_the_kobo() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("ann").await;
    let cookie = server.cookie(&scope).await;
    // Valid as an EPUB, but with no text at all.
    let opf = OPF.replace("c1.xhtml", "empty.xhtml");
    let mut bytes = epub_with(&opf, &png(600, 900), b"x");
    {
        let mut zip = zip::ZipWriter::new_append(Cursor::new(&mut bytes)).unwrap();
        zip.start_file("OEBPS/empty.xhtml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"<html><body><p> </p></body></html>")
            .unwrap();
        zip.finish().unwrap();
    }
    let id = upload(&server, &cookie, &bytes).await;
    drain(&server, &working()).await;

    let attempts = drain_reader(&server, &working()).await;
    assert_eq!(attempts, 3, "tried, then given up on");
    assert!(!server.state.layout.reader(id).exists());
    assert!(is_empty(&server.state.layout.tmp_dir()));

    let synced = server.sync(&token, None).await.json();
    assert_eq!(synced.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_book_off_the_kobo_leaves_nickels_library_and_comes_back() {
    let server = TestServer::start().await;
    let (scope, token): (UserScope, String) = server.user("ann").await;
    let book = server.add_book(&scope, "Done With").await;
    let synced = server.sync(&token, None).await;
    assert_eq!(sync_ids(&synced), [book.to_string()]);
    let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

    scope.set_on_device(book, false).await.unwrap();
    let off = server.sync(&token, Some(&cursor)).await;
    let reply = off.json();
    assert_eq!(
        reply[0]["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"],
        true
    );
    let cursor = off.header("x-kobo-synctoken").unwrap().to_owned();
    assert!(
        scope.library_book(book).await.unwrap().is_some(),
        "still in the library"
    );

    scope.set_on_device(book, true).await.unwrap();
    let back = server.sync(&token, Some(&cursor)).await;
    assert!(sync_ids(&back).contains(&book.to_string()));
    let fresh = server.sync(&token, None).await;
    assert_eq!(sync_ids(&fresh), [book.to_string()]);
}
