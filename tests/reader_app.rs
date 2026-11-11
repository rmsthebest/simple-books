//! The reader app, tapped through as a person would, syncing with the real
//! server over a socket: pairing, books arriving, and places going both
//! ways.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::{
    fs,
    io::Cursor,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use book_format::{Align, Block, BlockKind, Chapter, Position, Span};
use common::TestServer;
use reader_app::{
    AlwaysOnline, Battery, Device, Event, Folder, Light, Refresh, Session, SyncSetup, Turn,
};
use reader_core::Canvas;
use simple_books::server::{
    convert::reader::REVISION,
    db::{
        reader::{PlaceRow, ReaderChange},
        scope::UserScope,
    },
    now,
};
use uuid::Uuid;

/// A device whose files are in a folder, as on the Kobo.
struct Disk {
    dir: PathBuf,
}

impl Device for Disk {
    fn show(&mut self, _: &Canvas, _: Refresh) {}
    fn battery(&mut self) -> Option<Battery> {
        None
    }
    fn light(&mut self) -> Light {
        Light {
            brightness: 0,
            warmth: 0,
        }
    }
    fn set_light(&mut self, _: Light) {}
    fn suspend(&mut self) -> bool {
        false
    }
    fn load(&mut self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.dir.join(name)).ok()
    }
    fn save(&mut self, name: &str, bytes: &[u8]) {
        fs::write(self.dir.join(name), bytes).unwrap();
    }
    fn clock(&mut self) -> u64 {
        now() as u64
    }
}

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

/// A real book in the reader's format: two chapters of a few pages each.
fn book_file(title: &str) -> Vec<u8> {
    let paragraph = |n: usize| Block {
        kind: BlockKind::Paragraph { indent_first: true },
        indent: 0,
        align: Align::Default,
        spans: vec![Span::plain(
            format!("Paragraph {n} of a book that goes on for a while. ").repeat(12),
        )],
    };
    let chapters = [
        Chapter {
            blocks: (0..30).map(paragraph).collect(),
        },
        Chapter {
            blocks: (30..60).map(paragraph).collect(),
        },
    ];
    let mut out = Cursor::new(Vec::new());
    book_format::write(
        &mut out,
        book_format::About {
            title: title.into(),
            language: Some("en".into()),
            ..book_format::About::default()
        },
        Vec::new(),
        &chapters,
        &[],
    )
    .unwrap();
    out.into_inner()
}

async fn convert(server: &TestServer, book: Uuid, bytes: &[u8]) {
    use sha2::Digest;
    fs::write(server.state.layout.reader(book), bytes).unwrap();
    let sha256 = hex::encode(sha2::Sha256::digest(bytes));
    server
        .state
        .db
        .finish_reader_conversion(book, REVISION, Some((bytes.len() as i64, &sha256)))
        .await
        .unwrap();
}

/// Ticks the session until `done` says so, letting the server run.
async fn until(
    session: &mut Session<fs::File>,
    device: &mut Disk,
    what: &str,
    done: impl Fn(&Session<fs::File>) -> bool,
) {
    let give_up = Instant::now() + Duration::from_secs(30);
    while !done(session) {
        assert!(Instant::now() < give_up, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        session.handle(Event::Tick, Instant::now(), device).unwrap();
    }
}

fn tap(session: &mut Session<fs::File>, device: &mut Disk, (x, y): (f32, f32)) {
    session
        .handle(Event::Tap { x, y }, Instant::now(), device)
        .unwrap();
}

const HOME_SETTINGS: (f32, f32) = (776.0, 1376.0);
const SETTINGS_SYNC: (f32, f32) = (216.0, 1376.0);
/// Pair, or Sync now once paired: Pair is first along the bottom, and Sync
/// now second after Unpair; Back is last.
const SYNC_GO: (f32, f32) = (450.0, 1376.0);

/// Opens `file` and turns four pages. Returns where it got to.
fn read_a_little(session: &mut Session<fs::File>, device: &mut Disk, file: &str) -> Position {
    let opened = Instant::now();
    session.open(file, device, opened).unwrap();
    for turn in 1..=4 {
        // Apart, so the turn guard lets each through.
        let at = opened + Duration::from_secs(turn);
        session.handle(Event::Turn(Turn::Next), at, device).unwrap();
    }
    let here = session.reader().unwrap().position().unwrap();
    assert_ne!((here.chapter, here.block), (0, 0), "it turned");
    here
}

/// From an open book: the menu (the middle of the page), its Settings
/// (before Close on its bottom row), Sync, then Sync now.
async fn sync_from_the_book(session: &mut Session<fs::File>, device: &mut Disk) {
    tap(session, device, (536.0, 900.0));
    tap(session, device, (610.0, 780.0));
    assert!(session.in_settings(), "settings, to reach Sync now");
    tap(session, device, SETTINGS_SYNC);
    tap(session, device, SYNC_GO);
    until(session, device, "a sync", |s| !s.syncing()).await;
}

fn start(dir: &Path, url: &str, device: &mut Disk) -> Session<fs::File> {
    let books = Arc::new(Folder::new(dir.join("books")));
    let mut session = Session::start(books, device, Instant::now()).unwrap();
    session.enable_sync(
        SyncSetup {
            dir: dir.to_owned(),
            server: Some(url.to_owned()),
            name: "Kobo Clara BW".to_owned(),
            network: Arc::new(AlwaysOnline),
            ssh: None,
            updates: false,
        },
        device,
        Instant::now(),
    );
    session
}

/// Reads on, then sleeps: the new place goes up before it suspends, and
/// nothing else is synced.
async fn sleeping_sends_the_place(
    session: &mut Session<fs::File>,
    device: &mut Disk,
    scope: &UserScope,
    file: &str,
) {
    let further = read_a_little(session, device, file);
    session
        .handle(Event::Power, Instant::now(), device)
        .unwrap();
    assert!(session.syncing(), "sending the place before sleeping");
    until(session, device, "the place sent", |s| !s.syncing()).await;
    assert_eq!(session.last_sync(), Some("1 place sent."));
    let changes = scope.reader_changes(0, 100).await.unwrap();
    let last = changes
        .changes
        .iter()
        .rev()
        .find_map(|(_, change)| match change {
            ReaderChange::Place(place) => Some(place.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (last.chapter, last.block),
        (i64::from(further.chapter), i64::from(further.block))
    );
    // Waking and sleeping again with nothing new read sends nothing.
    session
        .handle(
            Event::Power,
            Instant::now() + Duration::from_secs(3),
            device,
        )
        .unwrap();
    session
        .handle(
            Event::Power,
            Instant::now() + Duration::from_secs(6),
            device,
        )
        .unwrap();
    assert!(!session.syncing(), "nothing new to send");
    session
        .handle(
            Event::Power,
            Instant::now() + Duration::from_secs(9),
            device,
        )
        .unwrap();
    sync_from_the_book(session, device).await;
    assert_eq!(session.last_sync(), Some("everything was up to date."));
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_then_books_and_places_go_back_and_forth() {
    let server = TestServer::start().await;
    let url = listen(&server).await;
    let (scope, _) = server.user("sam").await;
    let book = server.add_book(&scope, "Website Title").await;
    convert(&server, book, &book_file("File Title")).await;

    let dir = tempfile::tempdir().unwrap();
    let mut device = Disk {
        dir: dir.path().to_owned(),
    };
    let mut session = start(dir.path(), &url, &mut device);
    assert!(!session.syncing(), "not paired, so nothing to do");
    assert_eq!(session.library().books(), []);

    // Settings, Sync, Pair: a code comes up.
    tap(&mut session, &mut device, HOME_SETTINGS);
    tap(&mut session, &mut device, SETTINGS_SYNC);
    assert!(session.in_sync());
    tap(&mut session, &mut device, SYNC_GO);
    until(&mut session, &mut device, "a code", |s| {
        s.pairing_code().is_some()
    })
    .await;
    let code = session.pairing_code().unwrap().replace('-', "");

    // Typed on the website; the reader notices and gets the library.
    server
        .state
        .db
        .finish_pairing(scope.user_id(), &code, now())
        .await
        .unwrap();
    until(&mut session, &mut device, "the first sync", |s| {
        !s.syncing()
    })
    .await;
    assert_eq!(session.last_sync(), Some("1 book downloaded."));
    let entry = session.library().books()[0].clone();
    assert_eq!(
        entry.title, "Website Title",
        "the website's, not the file's"
    );
    assert_eq!(entry.id, Some(book.to_string()));
    assert_eq!(entry.revision, Some(REVISION));

    // Read a few pages, then sync: the place goes to the server.
    let here = read_a_little(&mut session, &mut device, &entry.file);
    sync_from_the_book(&mut session, &mut device).await;
    let changes = scope.reader_changes(0, 100).await.unwrap();
    let sent = changes
        .changes
        .iter()
        .find_map(|(_, change)| match change {
            ReaderChange::Place(place) => Some(place.clone()),
            _ => None,
        })
        .expect("the place reached the server");
    assert_eq!(
        (sent.chapter, sent.block),
        (i64::from(here.chapter), i64::from(here.block))
    );
    assert_eq!(sent.status, "reading");
    assert!(sent.percent > 0.0);

    sleeping_sends_the_place(&mut session, &mut device, &scope, &entry.file).await;

    // Read further on another reader: this one moves there at its next sync.
    let device_id = server.state.db.list_devices(scope.user_id()).await.unwrap()[1].id;
    scope
        .put_reader_place(
            book,
            device_id,
            &PlaceRow {
                book_id: book.to_string(),
                chapter: 1,
                block: 20,
                offset_chars: 0,
                revision: REVISION,
                percent: 85.0,
                status: "reading".to_owned(),
                read_at: now() + 5,
            },
        )
        .await
        .unwrap();
    tap(&mut session, &mut device, SYNC_GO);
    until(&mut session, &mut device, "a sync", |s| !s.syncing()).await;
    assert_eq!(session.last_sync(), Some("1 place from elsewhere."));
    session
        .open(&entry.file, &mut device, Instant::now())
        .unwrap();
    let there = session.reader().unwrap().position().unwrap();
    // The page holding it, which may start a little before.
    assert!(
        there.chapter == 1 && (19..=20).contains(&there.block),
        "{there:?}"
    );

    // Starting again later: still paired, and up to date.
    drop(session);
    let mut session = start(dir.path(), &url, &mut device);
    assert!(session.syncing(), "syncs on starting");
    until(&mut session, &mut device, "a sync", |s| !s.syncing()).await;
    assert_eq!(session.library().books().len(), 1);
}
