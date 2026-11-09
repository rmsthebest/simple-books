//! Hardcover, against a fake Hardcover that behaves like the real one where it
//! matters: refresh tokens rotate and a spent one ends the connection, moving a book
//! to "currently reading" starts a read, and "read" finishes it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(
    clippy::assert_is_empty,
    clippy::struct_excessive_bools,
    clippy::similar_names,
    reason = "a fake server's switches, and \"nothing was sent\" reads best as is_empty"
)]

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use common::TestServer;
use serde_json::{Value, json};
use simple_books::server::{
    db::{
        hardcover::BookLink,
        models::{BookDetails, MetadataEdit},
        scope::{HARDCOVER_PROGRESS_DELAY, UserScope},
    },
    hardcover::worker::run_once_at,
    now,
};
use uuid::Uuid;

const ME: i64 = 42;

#[derive(Clone)]
struct CatalogBook {
    id: i64,
    slug: &'static str,
    title: &'static str,
    author: &'static str,
    pages: i64,
    isbn: Option<(&'static str, i64)>,
}

struct Read {
    id: i64,
    pages: i64,
    finished: bool,
}

struct Shelf {
    id: i64,
    book_id: i64,
    status: i64,
    reads: Vec<Read>,
}

#[derive(Default)]
struct Fake {
    approved: bool,
    denied: bool,
    access: String,
    refresh: String,
    generation: u32,
    /// Refresh tokens are refused, as after the user revokes access.
    revoked_all: bool,
    revoked: Vec<String>,
    catalog: Vec<CatalogBook>,
    search_ids: Vec<i64>,
    shelves: Vec<Shelf>,
    next_id: i64,
    /// Every write, as "operation key=value", for the tests to check.
    writes: Vec<String>,
    rate_limit_once: bool,
    refuse_writes: bool,
}

impl Fake {
    fn issue(&mut self) -> Value {
        self.generation += 1;
        self.access = format!("hc_at_{}", self.generation);
        self.refresh = format!("hc_rt_{}", self.generation);
        json!({ "access_token": self.access, "refresh_token": self.refresh, "expires_in": 604_800 })
    }

    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn book_json(book: &CatalogBook) -> Value {
        json!({
            "id": book.id, "slug": book.slug, "title": book.title, "subtitle": null,
            "pages": book.pages, "contributions": [{ "author": { "name": book.author } }]
        })
    }

    fn set_status(&mut self, shelf: usize, status: i64) {
        let id = self.id();
        let shelf = &mut self.shelves[shelf];
        shelf.status = status;
        let open = shelf.reads.iter().any(|r| !r.finished);
        if status == 2 && !open {
            shelf.reads.push(Read {
                id,
                pages: 0,
                finished: false,
            });
        }
        if status == 3 {
            for read in &mut shelf.reads {
                read.finished = true;
            }
        }
    }
}

type Shared = Arc<Mutex<Fake>>;

fn oauth_error(error: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}

async fn device(Form(form): Form<HashMap<String, String>>) -> Response {
    assert_eq!(
        form.get("client_id").map(String::as_str),
        Some("test-client")
    );
    let scope = form.get("scope").unwrap();
    assert!(scope.contains("write:library") && scope.contains("read:catalog:search"));
    Json(json!({
        "device_code": "device-1", "user_code": "ABCD-1234",
        "verification_uri": "https://hardcover.app/link",
        "verification_uri_complete": "https://hardcover.app/link?code=ABCD-1234",
        "expires_in": 600, "interval": 5
    }))
    .into_response()
}

async fn token(State(fake): State<Shared>, Form(form): Form<HashMap<String, String>>) -> Response {
    let mut fake = fake.lock().unwrap();
    match form.get("grant_type").map(String::as_str) {
        Some("urn:ietf:params:oauth:grant-type:device_code") => {
            if fake.denied {
                oauth_error("access_denied")
            } else if fake.approved {
                Json(fake.issue()).into_response()
            } else {
                oauth_error("authorization_pending")
            }
        }
        Some("refresh_token") => {
            let given = form.get("refresh_token").cloned().unwrap_or_default();
            if fake.revoked_all || given != fake.refresh {
                // A spent one, used again, ends the whole chain on the real thing.
                fake.revoked_all = true;
                oauth_error("invalid_grant")
            } else {
                Json(fake.issue()).into_response()
            }
        }
        _ => oauth_error("unsupported_grant_type"),
    }
}

async fn revoke(State(fake): State<Shared>, Form(form): Form<HashMap<String, String>>) -> Response {
    let mut fake = fake.lock().unwrap();
    fake.revoked
        .push(form.get("token").cloned().unwrap_or_default());
    StatusCode::OK.into_response()
}

#[expect(clippy::too_many_lines, reason = "one fake API")]
async fn graphql(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut fake = fake.lock().unwrap();
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if fake.revoked_all || bearer != format!("Bearer {}", fake.access) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_token" })),
        )
            .into_response();
    }
    if std::mem::take(&mut fake.rate_limit_once) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "120")],
            "slow down",
        )
            .into_response();
    }
    let query = body["query"].as_str().unwrap().to_owned();
    let vars = &body["variables"];
    let int = |name: &str| vars[name].as_i64().unwrap();
    let refused = |field: &str| {
        Json(json!({ "data": { field: { "error": "nope", "id": null } } })).into_response()
    };

    let data = if query.contains("me {") {
        json!({ "me": [{ "id": ME, "username": "reader" }] })
    } else if query.contains("insert_user_book_read(") {
        if fake.refuse_writes {
            return refused("insert_user_book_read");
        }
        let (shelf, pages) = (int("id"), vars["read"]["progress_pages"].as_i64().unwrap());
        let id = fake.id();
        let at = fake.shelves.iter().position(|s| s.id == shelf).unwrap();
        fake.shelves[at].reads.push(Read {
            id,
            pages,
            finished: false,
        });
        fake.writes
            .push(format!("insert_user_book_read pages={pages}"));
        json!({ "insert_user_book_read": { "error": null, "id": id } })
    } else if query.contains("update_user_book_read(") {
        if fake.refuse_writes {
            return refused("update_user_book_read");
        }
        let (id, pages) = (int("id"), int("pages"));
        for shelf in &mut fake.shelves {
            for read in &mut shelf.reads {
                if read.id == id {
                    read.pages = pages;
                }
            }
        }
        fake.writes
            .push(format!("update_user_book_read pages={pages}"));
        json!({ "update_user_book_read": { "error": null, "id": id } })
    } else if query.contains("insert_user_book(") {
        if fake.refuse_writes {
            return refused("insert_user_book");
        }
        let object = &vars["object"];
        let (book, status) = (
            object["book_id"].as_i64().unwrap(),
            object["status_id"].as_i64().unwrap(),
        );
        let id = fake.id();
        fake.shelves.push(Shelf {
            id,
            book_id: book,
            status: 0,
            reads: Vec::new(),
        });
        let at = fake.shelves.len() - 1;
        fake.set_status(at, status);
        let edition = object.get("edition_id").and_then(Value::as_i64);
        fake.writes.push(match edition {
            Some(edition) => {
                format!("insert_user_book book={book} status={status} edition={edition}")
            }
            None => format!("insert_user_book book={book} status={status}"),
        });
        json!({ "insert_user_book": { "error": null, "id": id } })
    } else if query.contains("update_user_book(") {
        if fake.refuse_writes {
            return refused("update_user_book");
        }
        let (id, status) = (int("id"), int("status"));
        let at = fake.shelves.iter().position(|s| s.id == id).unwrap();
        fake.set_status(at, status);
        fake.writes
            .push(format!("update_user_book status={status}"));
        json!({ "update_user_book": { "error": null, "id": id } })
    } else if query.contains("user_book_reads(where: { user_book_id") {
        let id = int("id");
        let reads: Vec<Value> = fake
            .shelves
            .iter()
            .filter(|s| s.id == id)
            .flat_map(|s| {
                s.reads
                    .iter()
                    .filter(|r| !r.finished)
                    .map(|r| json!({ "id": r.id }))
            })
            .rev()
            .take(1)
            .collect();
        json!({ "user_book_reads": reads })
    } else if query.contains("user_books(") {
        assert_eq!(int("user"), ME, "only the user's own shelves");
        let book = int("book");
        let shelves: Vec<Value> = fake
            .shelves
            .iter()
            .filter(|s| s.book_id == book)
            .map(|s| {
                let open: Vec<Value> = s
                    .reads
                    .iter()
                    .filter(|r| !r.finished)
                    .rev()
                    .take(1)
                    .map(|r| json!({ "id": r.id }))
                    .collect();
                json!({ "id": s.id, "status_id": s.status, "user_book_reads": open })
            })
            .collect();
        json!({ "user_books": shelves })
    } else if query.contains("editions(") {
        let isbn = vars["isbn"].as_str().unwrap();
        let editions: Vec<Value> = fake
            .catalog
            .iter()
            .filter_map(|b| {
                let (code, edition) = b.isbn?;
                (code == isbn)
                    .then(|| json!({ "id": edition, "pages": b.pages, "book": Fake::book_json(b) }))
            })
            .collect();
        json!({ "editions": editions })
    } else if query.contains("slug: { _eq") {
        let slug = vars["slug"].as_str().unwrap();
        let books: Vec<Value> = fake
            .catalog
            .iter()
            .filter(|b| b.slug == slug)
            .map(Fake::book_json)
            .collect();
        json!({ "books": books })
    } else if query.contains("search(") {
        // As strings, the way Typesense ids sometimes come.
        let ids: Vec<String> = fake.search_ids.iter().map(ToString::to_string).collect();
        json!({ "search": { "ids": ids } })
    } else if query.contains("_in: $ids") {
        let ids: Vec<i64> = vars["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        // Not in the order asked, as a database might answer.
        let books: Vec<Value> = fake
            .catalog
            .iter()
            .filter(|b| ids.contains(&b.id))
            .map(Fake::book_json)
            .collect();
        json!({ "books": books })
    } else {
        panic!("the fake doesn't know this query: {query}");
    };
    Json(json!({ "data": data })).into_response()
}

async fn fake_hardcover() -> (Shared, String) {
    let fake: Shared = Arc::default();
    {
        let mut f = fake.lock().unwrap();
        f.next_id = 1000;
        f.catalog = vec![
            CatalogBook {
                id: 1,
                slug: "hollywood-ending",
                title: "Hollywood Ending",
                author: "John Green",
                pages: 300,
                isbn: Some(("9781101592434", 77)),
            },
            CatalogBook {
                id: 2,
                slug: "hollywood-ending-companion",
                title: "Hollywood Ending Companion",
                author: "Someone Else",
                pages: 90,
                isbn: None,
            },
            CatalogBook {
                id: 3,
                slug: "folke-dahlbergs-landskapsbocker",
                title: "Folke Dahlbergs landskapsböcker",
                author: "M. Jansson",
                pages: 200,
                isbn: None,
            },
        ];
    }
    let app = Router::new()
        .route("/oauth2/device", post(device))
        .route("/oauth2/token", post(token))
        .route("/oauth2/revoke", post(revoke))
        .route("/v1/graphql", post(graphql))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, url)
}

struct Setup {
    server: TestServer,
    fake: Shared,
}

impl Setup {
    async fn new() -> Self {
        let (fake, url) = fake_hardcover().await;
        let server = TestServer::start_with(&[
            ("HARDCOVER_CLIENT_ID", "test-client"),
            ("HARDCOVER_API_URL", &url),
        ])
        .await;
        Self { server, fake }
    }

    async fn run(&self) {
        self.run_at(now()).await;
    }

    async fn run_at(&self, at: i64) {
        let hardcover = self.server.state.hardcover.clone().unwrap();
        run_once_at(&self.server.state.db, &hardcover, at)
            .await
            .unwrap();
    }

    async fn post(&self, path: &str, cookie: &str, body: &str) -> common::Reply {
        self.server
            .request(
                Method::POST,
                path,
                &[
                    ("cookie", cookie),
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("sec-fetch-site", "same-origin"),
                ],
                body.as_bytes().to_vec(),
            )
            .await
    }

    async fn page(&self, path: &str, cookie: &str) -> String {
        let reply = self
            .server
            .request(Method::GET, path, &[("cookie", cookie)], Vec::new())
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        String::from_utf8(reply.body).unwrap()
    }

    /// Signs `user` in to the fake Hardcover, the way a person would.
    async fn connect(&self, cookie: &str) {
        let reply = self.post("/hardcover/connect", cookie, "").await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER);
        assert_eq!(reply.header("location"), Some("/device"));
        self.fake.lock().unwrap().approved = true;
        self.run_at(now() + 5).await;
    }

    fn writes(&self) -> Vec<String> {
        std::mem::take(&mut self.fake.lock().unwrap().writes)
    }

    async fn with_isbn(
        &self,
        scope: &UserScope,
        book: Uuid,
        title: &str,
        author: &str,
        isbn: Option<&str>,
    ) {
        let edit = MetadataEdit {
            title: title.to_owned(),
            authors: vec![author.to_owned()],
            details: BookDetails {
                isbn: isbn.map(str::to_owned),
                ..Default::default()
            },
        };
        scope.set_metadata(book, &edit).await.unwrap();
    }
}

#[tokio::test]
async fn connecting_with_a_code() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    let page = setup.page("/device", &cookie).await;
    assert!(page.contains("Connect Hardcover"));

    let reply = setup.post("/hardcover/connect", &cookie, "").await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let page = setup.page("/device", &cookie).await;
    assert!(page.contains("ABCD-1234"), "the code is shown");
    assert!(page.contains(r#"href="https://hardcover.app/link?code=ABCD-1234""#));
    assert!(
        page.contains(r#"http-equiv="refresh""#),
        "and the page checks back"
    );
    assert!(
        !page.contains("device-1"),
        "the device code stays on the server"
    );

    // Not entered yet.
    setup.run_at(now() + 5).await;
    assert!(setup.page("/device", &cookie).await.contains("ABCD-1234"));

    setup.fake.lock().unwrap().approved = true;
    setup.run_at(now() + 10).await;
    let page = setup.page("/device", &cookie).await;
    assert!(page.contains("Connected as"));
    assert!(page.contains("reader"));
    assert!(!page.contains(r#"http-equiv="refresh""#));
    assert!(
        !page.contains("hc_at_") && !page.contains("hc_rt_"),
        "no token reaches a page"
    );
}

#[tokio::test]
async fn a_refused_sign_in_says_so() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.post("/hardcover/connect", &cookie, "").await;
    setup.fake.lock().unwrap().denied = true;
    setup.run_at(now() + 5).await;
    let page = setup.page("/device", &cookie).await;
    assert!(page.contains("wasn&#x27;t approved") || page.contains("wasn't approved"));
    assert!(page.contains("Connect Hardcover"));

    // Cancelling clears it.
    setup.post("/hardcover/cancel", &cookie, "").await;
    let page = setup.page("/device", &cookie).await;
    assert!(!page.contains("approved"));
}

#[tokio::test]
async fn books_go_as_want_to_read_then_progress_then_read() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    // Already in the library before connecting: it goes too.
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    setup
        .with_isbn(
            &reader,
            book,
            "Hollywood, Ending",
            "John Green",
            Some("9781101592434"),
        )
        .await;
    setup.connect(&cookie).await;
    assert_eq!(
        setup.writes(),
        ["insert_user_book book=1 status=1 edition=77"]
    );
    assert_eq!(
        reader.hardcover_book(book).await.unwrap(),
        Some(BookLink::Found {
            slug: Some("hollywood-ending".to_owned())
        })
    );
    let page = setup.page(&format!("/books/{book}"), &cookie).await;
    assert!(page.contains(r#"href="https://hardcover.app/books/hollywood-ending""#));

    // Reading: held back a minute, so page turns go together.
    reader
        .put_reading_state(book, "Reading", Some(10.0), "{}")
        .await
        .unwrap();
    reader
        .put_reading_state(book, "Reading", Some(50.0), "{}")
        .await
        .unwrap();
    setup.run().await;
    assert!(setup.writes().is_empty());
    let later = now() + HARDCOVER_PROGRESS_DELAY + 1;
    setup.run_at(later).await;
    assert_eq!(
        setup.writes(),
        [
            "update_user_book status=2",
            "update_user_book_read pages=150"
        ]
    );

    reader
        .put_reading_state(book, "Reading", Some(60.0), "{}")
        .await
        .unwrap();
    setup.run_at(later + HARDCOVER_PROGRESS_DELAY + 1).await;
    assert_eq!(setup.writes(), ["update_user_book_read pages=180"]);

    // The same place again sends nothing.
    reader
        .put_reading_state(book, "Reading", Some(60.0), "{}")
        .await
        .unwrap();
    setup.run_at(later + 2 * HARDCOVER_PROGRESS_DELAY + 2).await;
    assert!(setup.writes().is_empty());

    // Finishing goes straight away.
    reader
        .put_reading_state(book, "Finished", Some(100.0), "{}")
        .await
        .unwrap();
    setup.run().await;
    assert_eq!(setup.writes(), ["update_user_book status=3"]);

    // Opening it again afterwards doesn't take it back off "read".
    reader
        .put_reading_state(book, "Reading", Some(5.0), "{}")
        .await
        .unwrap();
    setup.run_at(now() + HARDCOVER_PROGRESS_DELAY + 1).await;
    assert!(setup.writes().is_empty());
}

#[tokio::test]
async fn a_new_book_goes_as_want_to_read_but_never_replaces_anything() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    {
        // Already read, on Hardcover.
        let mut fake = setup.fake.lock().unwrap();
        fake.shelves.push(Shelf {
            id: 1,
            book_id: 1,
            status: 3,
            reads: Vec::new(),
        });
    }
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    setup
        .with_isbn(
            &reader,
            book,
            "Hollywood, Ending",
            "John Green",
            Some("9781101592434"),
        )
        .await;
    setup.run().await;
    reader
        .put_reading_state(book, "Reading", Some(20.0), "{}")
        .await
        .unwrap();
    setup.run_at(now() + HARDCOVER_PROGRESS_DELAY + 1).await;
    assert!(setup.writes().is_empty(), "a read book is left alone");
}

#[tokio::test]
async fn a_search_takes_only_a_close_match() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    // The search ranks the wrong book first; the title and author check skips it.
    setup.fake.lock().unwrap().search_ids = vec![2, 1];
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    setup
        .with_isbn(&reader, book, "Hollywood, Ending", "Green, John", None)
        .await;
    setup.run().await;
    assert_eq!(setup.writes(), ["insert_user_book book=1 status=1"]);
}

#[tokio::test]
async fn a_book_not_found_can_be_linked_by_hand() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    let book = setup.server.add_book(&reader, "Folke Dahlberg").await;
    setup.run().await;
    assert!(setup.writes().is_empty());
    assert_eq!(
        reader.hardcover_book(book).await.unwrap(),
        Some(BookLink::Missing)
    );
    let page = setup.page(&format!("/books/{book}"), &cookie).await;
    assert!(page.contains("find this book on Hardcover"));
    let page = setup.page("/device", &cookie).await;
    assert!(
        page.contains(&format!("/books/{book}")),
        "the Kobo page lists it"
    );

    // Not a Hardcover link.
    let reply = setup
        .post(
            &format!("/books/{book}/hardcover"),
            &cookie,
            "link=https%3A%2F%2Fevil.example%2Fbooks%2Fx",
        )
        .await;
    assert_eq!(
        reply.header("location"),
        Some(format!("/books/{book}?error=hardcover_link").as_str())
    );

    let reply = setup
        .post(
            &format!("/books/{book}/hardcover"),
            &cookie,
            "link=https%3A%2F%2Fhardcover.app%2Fbooks%2Ffolke-dahlbergs-landskapsbocker",
        )
        .await;
    assert_eq!(
        reply.header("location"),
        Some(format!("/books/{book}?done=hardcover_link").as_str())
    );
    setup.run().await;
    assert_eq!(setup.writes(), ["insert_user_book book=3 status=1"]);
    assert!(
        !setup
            .page("/device", &cookie)
            .await
            .contains(&format!("/books/{book}"))
    );
}

#[tokio::test]
async fn tokens_rotate_and_a_dead_connection_asks_to_reconnect() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    setup
        .with_isbn(
            &reader,
            book,
            "Hollywood, Ending",
            "John Green",
            Some("9781101592434"),
        )
        .await;

    // Hardcover stops taking the access token: it's refreshed, once, and the update
    // goes again shortly after.
    let refreshed_before = setup.fake.lock().unwrap().generation;
    setup.fake.lock().unwrap().access = "rotated-away".to_owned();
    reader
        .put_reading_state(book, "Finished", Some(100.0), "{}")
        .await
        .unwrap();
    setup.run().await;
    assert!(setup.writes().is_empty());
    let generation = setup.fake.lock().unwrap().generation;
    assert_eq!(
        generation,
        refreshed_before + 1,
        "refused once, refreshed once"
    );
    setup.run_at(now() + 10).await;
    assert_eq!(
        setup.writes(),
        ["insert_user_book book=1 status=3 edition=77"]
    );

    // The user revokes access on Hardcover: the next refresh fails, and the page asks
    // to connect again instead of failing quietly.
    {
        let mut fake = setup.fake.lock().unwrap();
        fake.revoked_all = true;
    }
    reader
        .put_reading_state(book, "Reading", Some(1.0), "{}")
        .await
        .unwrap();
    setup.run_at(now() + HARDCOVER_PROGRESS_DELAY + 1).await;
    let page = setup.page("/device", &cookie).await;
    assert!(page.contains("stopped accepting"), "{page}");
    assert!(page.contains("Connect Hardcover"));
}

#[tokio::test]
async fn slowing_down_when_asked() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    setup.fake.lock().unwrap().rate_limit_once = true;
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    setup
        .with_isbn(
            &reader,
            book,
            "Hollywood, Ending",
            "John Green",
            Some("9781101592434"),
        )
        .await;
    setup.run().await;
    assert!(setup.writes().is_empty());
    // Held for the two minutes asked, then sent.
    setup.run_at(now() + 60).await;
    assert!(setup.writes().is_empty());
    setup.run_at(now() + 121).await;
    assert_eq!(
        setup.writes(),
        ["insert_user_book book=1 status=1 edition=77"]
    );
}

#[tokio::test]
async fn a_refused_update_is_tried_a_few_times_then_dropped() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    setup.fake.lock().unwrap().refuse_writes = true;
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    setup
        .with_isbn(
            &reader,
            book,
            "Hollywood, Ending",
            "John Green",
            Some("9781101592434"),
        )
        .await;
    let mut at = now();
    for _ in 0..10 {
        setup.run_at(at).await;
        at += 86_400;
    }
    setup.fake.lock().unwrap().refuse_writes = false;
    setup.run_at(at).await;
    assert!(
        setup.writes().is_empty(),
        "given up on, not retried forever"
    );
}

#[tokio::test]
async fn disconnecting_revokes_and_forgets() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let cookie = setup.server.cookie(&reader).await;
    setup.connect(&cookie).await;
    let reply = setup.post("/hardcover/disconnect", &cookie, "").await;
    assert_eq!(reply.header("location"), Some("/device?done=hardcover_off"));

    // Revoking runs in the background.
    for _ in 0..50 {
        if !setup.fake.lock().unwrap().revoked.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(setup.fake.lock().unwrap().revoked, ["hc_rt_1"]);
    assert!(
        setup
            .page("/device", &cookie)
            .await
            .contains("Connect Hardcover")
    );

    // Nothing is queued any more.
    let book = setup.server.add_book(&reader, "Hollywood, Ending").await;
    reader
        .put_reading_state(book, "Finished", Some(100.0), "{}")
        .await
        .unwrap();
    setup.run_at(now() + 3600).await;
    assert!(setup.writes().is_empty());
    assert_eq!(reader.hardcover_book(book).await.unwrap(), None);
}

#[tokio::test]
async fn one_users_connection_is_theirs_alone() {
    let setup = Setup::new().await;
    let (reader, _) = setup.server.user("reader").await;
    let (other, _) = setup.server.user("other").await;
    let reader_cookie = setup.server.cookie(&reader).await;
    let other_cookie = setup.server.cookie(&other).await;
    setup.connect(&reader_cookie).await;

    let shared = setup
        .server
        .add_shared_book(&other, "Hollywood, Ending")
        .await;
    other
        .put_reading_state(shared, "Finished", Some(100.0), "{}")
        .await
        .unwrap();
    setup.run_at(now() + 3600).await;
    assert!(setup.writes().is_empty(), "the other user hasn't connected");
    let page = setup.page("/device", &other_cookie).await;
    assert!(page.contains("Connect Hardcover") && !page.contains("Connected as"));

    // The other user can't point the reader's books anywhere.
    let mine = setup.server.add_book(&reader, "Folke").await;
    let reply = setup
        .post(
            &format!("/books/{mine}/hardcover"),
            &other_cookie,
            "link=folke-dahlbergs-landskapsbocker",
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Without a cookie, or from another site, nothing happens.
    let reply = setup
        .server
        .request(
            Method::POST,
            "/hardcover/disconnect",
            &[("cookie", &reader_cookie), ("sec-fetch-site", "cross-site")],
            Vec::new(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(
        setup
            .page("/device", &reader_cookie)
            .await
            .contains("Connected as")
    );
}

#[tokio::test]
async fn hardcover_is_only_offered_when_turned_on() {
    let server = TestServer::start().await;
    let (reader, _) = server.user("reader").await;
    let cookie = server.cookie(&reader).await;
    let reply = server
        .request(Method::GET, "/device", &[("cookie", &cookie)], Vec::new())
        .await;
    assert!(!String::from_utf8(reply.body).unwrap().contains("Hardcover"));
    let reply = server
        .request(
            Method::POST,
            "/hardcover/connect",
            &[("cookie", &cookie), ("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}
