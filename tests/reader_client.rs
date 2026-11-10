//! The reader's sync client against the real server, over a real socket.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::{collections::BTreeMap, fs, net::SocketAddr};

use common::TestServer;
use reader_sync::{Client, Error, PairStatus, Paths, Report, Spot, State, Status, sync};
use simple_books::server::{convert::reader::REVISION, db::scope::UserScope, now, testkit};
use tempfile::TempDir;
use uuid::Uuid;

/// Serves the test server's router on a local port, as a real server would.
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

/// Runs blocking client code off the server's threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Pretends the background conversion made the book's reader file.
async fn convert(server: &TestServer, book: Uuid, bytes: &[u8]) {
    fs::write(server.state.layout.reader(book), bytes).unwrap();
    server
        .state
        .db
        .finish_reader_conversion(book, REVISION, Some((bytes.len() as i64, &sha256(bytes))))
        .await
        .unwrap();
}

/// A reader with its own folder, paired with a user's library.
struct Reader {
    _dir: TempDir,
    client: Client,
    paths: Paths,
    secret: String,
}

impl Reader {
    async fn pair(server: &TestServer, url: &str, scope: &UserScope) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        let client = Client::new(url);
        let start = {
            let client = client.clone();
            blocking(move || client.pair_start("Kobo Clara BW"))
                .await
                .unwrap()
        };
        let secret = start.secret.clone();
        let asked = {
            let (client, secret) = (client.clone(), secret.clone());
            blocking(move || client.pair_status(&secret)).await.unwrap()
        };
        assert_eq!(asked, PairStatus::Pending);
        server
            .state
            .db
            .finish_pairing(scope.user_id(), &start.code.replace('-', ""), now())
            .await
            .unwrap();
        let asked = {
            let (client, secret) = (client.clone(), secret.clone());
            blocking(move || client.pair_status(&secret)).await.unwrap()
        };
        assert!(matches!(asked, PairStatus::Paired { .. }));
        paths.save_secret(&secret).unwrap();
        Self {
            _dir: dir,
            client,
            paths,
            secret,
        }
    }

    async fn sync(&self, local: &[(Uuid, Spot)]) -> Result<Report, Error> {
        let (client, secret, paths) =
            (self.client.clone(), self.secret.clone(), self.paths.clone());
        let local: BTreeMap<String, Spot> = local
            .iter()
            .map(|(id, spot)| (id.to_string(), *spot))
            .collect();
        blocking(move || {
            let mut state = State::load(&paths.state);
            sync(&client, &secret, &paths, &mut state, &local, &mut |_| {})
        })
        .await
    }

    fn file(&self, book: Uuid) -> Option<Vec<u8>> {
        fs::read(self.paths.book(&book.to_string())).ok()
    }

    fn state(&self) -> State {
        State::load(&self.paths.state)
    }
}

fn ids(books: &[Uuid]) -> Vec<String> {
    books.iter().map(ToString::to_string).collect()
}

fn spot(percent: f32, at: i64) -> Spot {
    Spot {
        chapter: 1,
        block: 4,
        offset: 0,
        revision: REVISION,
        percent,
        status: Status::Reading,
        at,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reader_keeps_its_books_in_step_with_the_library() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let older = server.add_book(&scope, "Older").await;
    let newer = server.add_book(&scope, "Newer").await;
    convert(&server, older, b"older book").await;
    let reader = Reader::pair(&server, &url, &scope).await;

    // Only the converted one can be downloaded yet; both are known.
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, ids(&[older]));
    assert_eq!(reader.file(older).unwrap(), b"older book");
    assert_eq!(reader.file(newer), None);
    assert_eq!(reader.state().books.len(), 2);

    // Nothing new, nothing done.
    let report = reader.sync(&[]).await.unwrap();
    assert!(!report.library_changed(), "{report:?}");

    convert(&server, newer, b"newer book").await;
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, ids(&[newer]));

    // An edit on the website changes the details, not the file.
    scope.set_title(older, "Older, Retitled").await.unwrap();
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.changed, ids(&[older]));
    assert_eq!(report.downloaded, Vec::<String>::new());
    assert_eq!(
        reader.state().books[&older.to_string()].book.title,
        "Older, Retitled"
    );

    // Made again, as after a converter upgrade: downloaded again.
    convert(&server, older, b"older book, made better").await;
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, ids(&[older]));
    assert_eq!(reader.file(older).unwrap(), b"older book, made better");

    // Deleted by hand on the reader: fetched again.
    fs::remove_file(reader.paths.book(&older.to_string())).unwrap();
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, ids(&[older]));

    // Deleted on the website: gone from the reader too.
    testkit::delete(&scope, &server.state.layout, newer)
        .await
        .unwrap();
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.removed, ids(&[newer]));
    assert_eq!(reader.file(newer), None);
    assert!(!reader.state().books.contains_key(&newer.to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_carry_on_and_damaged_ones_are_never_used() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let book = server.add_book(&scope, "Book").await;
    let bytes = b"a book that took a while to download".to_vec();
    convert(&server, book, &bytes).await;
    let reader = Reader::pair(&server, &url, &scope).await;
    fs::create_dir_all(&reader.paths.books).unwrap();
    let partial = reader.paths.books.join(format!("{book}.part"));

    // Half of it, from a sync that was cut off.
    fs::write(&partial, &bytes[..10]).unwrap();
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, ids(&[book]));
    assert_eq!(reader.file(book).unwrap(), bytes);
    assert!(!partial.exists());

    // Made again; meanwhile a partial download went wrong on the reader.
    let better = b"a better book, also slow to download".to_vec();
    convert(&server, book, &better).await;
    fs::write(&partial, b"garbage!!!").unwrap();
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, Vec::<String>::new());
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(reader.file(book).unwrap(), bytes, "the old file is kept");
    assert!(!partial.exists(), "the damaged download is dropped");

    // The next sync starts afresh, and it works.
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.downloaded, ids(&[book]));
    assert_eq!(reader.file(book).unwrap(), better);
}

#[tokio::test(flavor = "multi_thread")]
async fn places_go_between_readers_and_the_newest_wins() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let book = server.add_book(&scope, "Book").await;
    convert(&server, book, b"book").await;
    let kobo = Reader::pair(&server, &url, &scope).await;
    let other = Reader::pair(&server, &url, &scope).await;
    kobo.sync(&[]).await.unwrap();
    other.sync(&[]).await.unwrap();
    let at = now() - 1000;

    let report = kobo.sync(&[(book, spot(40.0, at))]).await.unwrap();
    assert_eq!(report.sent, 1);
    let report = kobo.sync(&[(book, spot(40.0, at))]).await.unwrap();
    assert_eq!(report.sent, 0, "sent once");

    // The other reader moves there.
    let report = other.sync(&[]).await.unwrap();
    assert_eq!(report.places, [(book.to_string(), spot(40.0, at))]);

    // An older place, from a reader that was offline, doesn't win, and the
    // newer one comes back to it.
    let report = other.sync(&[(book, spot(10.0, at - 500))]).await.unwrap();
    assert_eq!(report.sent, 1);
    assert_eq!(report.places, [(book.to_string(), spot(40.0, at))]);

    // A newer one does, and reaches the first reader.
    let report = other.sync(&[(book, spot(60.0, at + 10))]).await.unwrap();
    assert_eq!(report.places, []);
    let report = kobo.sync(&[(book, spot(40.0, at))]).await.unwrap();
    assert_eq!(report.places, [(book.to_string(), spot(60.0, at + 10))]);
    assert_eq!(report.sent, 0, "the reader's own place isn't sent again");
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_everything_again_drops_what_the_library_no_longer_has() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let book = server.add_book(&scope, "Kept").await;
    convert(&server, book, b"kept").await;
    let reader = Reader::pair(&server, &url, &scope).await;
    reader.sync(&[]).await.unwrap();

    // A book the reader somehow holds that the server will never mention.
    let mut state = reader.state();
    let mut stray = state.books[&book.to_string()].clone();
    stray.book.id = "stray".to_owned();
    state.books.insert("stray".to_owned(), stray);
    state.save(&reader.paths.state).unwrap();
    fs::write(reader.paths.book("stray"), b"stray").unwrap();

    let device = server
        .state
        .db
        .list_devices(scope.user_id())
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.reader)
        .unwrap();
    server
        .state
        .db
        .request_full_sync(scope.user_id(), device.id)
        .await
        .unwrap();
    let report = reader.sync(&[]).await.unwrap();
    assert_eq!(report.removed, ["stray"]);
    assert!(!reader.paths.book("stray").exists());
    assert_eq!(reader.file(book).unwrap(), b"kept", "what's listed stays");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_reader_is_told_to_pair_again() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let reader = Reader::pair(&server, &url, &scope).await;
    reader.sync(&[]).await.unwrap();

    let device = server.state.db.list_devices(scope.user_id()).await.unwrap()[1].id;
    assert!(
        server
            .state
            .db
            .revoke_device(scope.user_id(), device)
            .await
            .unwrap()
    );
    assert!(matches!(reader.sync(&[]).await, Err(Error::NotPaired)));

    // And a server that isn't there is a network problem, not a lost pairing.
    let gone = Client::new("http://127.0.0.1:9");
    let result = blocking(move || gone.changes("x", 0)).await;
    assert!(matches!(result, Err(Error::Network(_))), "{result:?}");
}
