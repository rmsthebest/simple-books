//! Shared helpers for tests that talk to the real router.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request, StatusCode},
};
use leptos::config::LeptosOptions;
use serde_json::Value;
use simple_books::server::{
    AppState,
    auth::session::{self, COOKIE_NAME},
    config::Config,
    db::{Db, models::Visibility, scope::UserScope},
    hardcover::Hardcover,
    http::limit::Limits,
    kobo::resources::Resources,
    reader::install::KoboFiles,
    router,
    storage::layout::Layout,
    testkit::{self as dev, DevImport},
};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

pub const PUBLIC_URL: &str = "https://books.example.com";

pub struct TestServer {
    pub dir: TempDir,
    pub router: Router,
    pub state: AppState,
    pub epub: PathBuf,
    pub kepub: PathBuf,
    pub cover: PathBuf,
}

/// One field of a form.
pub struct Part<'a> {
    pub name: &'a str,
    pub filename: Option<&'a str>,
    pub body: &'a [u8],
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body));
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with(&[]).await
    }

    pub async fn start_with(settings: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        std::fs::create_dir_all(root.join("books")).unwrap();
        std::fs::create_dir_all(root.join("tmp")).unwrap();

        let config = Config::from_lookup(|name| {
            if name == "PUBLIC_URL" {
                return Some(PUBLIC_URL.to_owned());
            }
            settings
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        })
        .unwrap();

        let layout = Layout::new(&root);
        let db = Db::open(&layout.database()).await.unwrap();
        let leptos = LeptosOptions::builder()
            .output_name("simple-books")
            .site_root(dir.path().to_string_lossy().to_string())
            .site_pkg_dir("pkg")
            .build();

        let hardcover = config.hardcover_client_id.as_deref().map(|id| {
            Arc::new(
                Hardcover::new(&config.hardcover_api, id)
                    .unwrap()
                    .with_pace(std::time::Duration::ZERO),
            )
        });
        let state = AppState {
            config: Arc::new(config),
            db,
            layout,
            kobo_resources: Arc::new(Resources::load().unwrap()),
            leptos,
            conversions: Arc::default(),
            uploads: Arc::default(),
            limits: Arc::new(Limits::default()),
            hardcover,
            first_run: Arc::default(),
            kobo_files: Arc::new(KoboFiles::new(dir.path().join("kobo-files"))),
        };

        let epub = dir.path().join("book.epub");
        let kepub = dir.path().join("book.kepub.epub");
        let cover = dir.path().join("cover.jpg");
        std::fs::write(&epub, b"pretend epub bytes".repeat(100)).unwrap();
        std::fs::write(&kepub, b"pretend kepub bytes".repeat(50)).unwrap();
        std::fs::write(&cover, b"pretend jpeg").unwrap();

        Self {
            dir,
            router: router::build(state.clone()),
            state,
            epub,
            kepub,
            cover,
        }
    }

    pub async fn user(&self, name: &str) -> (UserScope, String) {
        let id = dev::ensure_user(&self.state.db, name).await.unwrap();
        let (_, token) = dev::create_kobo_device(&self.state.db, id, name)
            .await
            .unwrap();
        (dev::scope(&self.state.db, id), token)
    }

    /// A user who is an admin, with a Kobo.
    pub async fn admin(&self, name: &str) -> (UserScope, String) {
        let id = self
            .state
            .db
            .create_user(name, 1024 * 1024 * 1024, true)
            .await
            .unwrap();
        let (_, token) = dev::create_kobo_device(&self.state.db, id, name)
            .await
            .unwrap();
        (dev::scope(&self.state.db, id), token)
    }

    pub async fn add_book(&self, scope: &UserScope, title: &str) -> Uuid {
        self.add_book_with(scope, title, Visibility::Private).await
    }

    pub async fn add_shared_book(&self, scope: &UserScope, title: &str) -> Uuid {
        self.add_book_with(scope, title, Visibility::Shared).await
    }

    async fn add_book_with(&self, scope: &UserScope, title: &str, visibility: Visibility) -> Uuid {
        dev::import(
            scope,
            &self.state.layout,
            DevImport {
                epub: &self.epub,
                kepub: Some(&self.kepub),
                cover: Some(&self.cover),
                title,
                authors: vec!["Ann".to_owned()],
                dedupe_salt: Some(Uuid::new_v4().to_string()),
                visibility,
            },
        )
        .await
        .unwrap()
    }

    /// A session cookie header for this user.
    pub async fn cookie(&self, scope: &UserScope) -> String {
        let token = session::start(&self.state.db, scope.user_id())
            .await
            .unwrap();
        format!("{COOKIE_NAME}={token}")
    }

    /// Posts a form the way a browser on this site would.
    pub async fn post_form(&self, path: &str, cookie: &str, parts: &[Part<'_>]) -> Reply {
        const BOUNDARY: &str = "----test-boundary-7MA4YWxk";
        let mut body = Vec::new();
        for part in parts {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            let disposition = match part.filename {
                Some(file) => format!("name=\"{}\"; filename=\"{file}\"", part.name),
                None => format!("name=\"{}\"", part.name),
            };
            body.extend_from_slice(
                format!("Content-Disposition: form-data; {disposition}\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(part.body);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        self.request(
            Method::POST,
            path,
            &[
                ("cookie", cookie),
                ("content-type", &content_type),
                ("accept", "application/json"),
                ("sec-fetch-site", "same-origin"),
            ],
            body,
        )
        .await
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Reply {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .clone()
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        Reply {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, path: &str) -> Reply {
        self.request(Method::GET, path, &[], Vec::new()).await
    }

    pub async fn sync(&self, token: &str, sync_token: Option<&str>) -> Reply {
        let path = format!("/kobo/{token}/v1/library/sync");
        match sync_token {
            Some(value) => {
                self.request(
                    Method::GET,
                    &path,
                    &[("x-kobo-synctoken", value)],
                    Vec::new(),
                )
                .await
            }
            None => self.get(&path).await,
        }
    }
}

/// The ID of the book a sync entry refers to.
pub fn entry_id(entry: &Value) -> String {
    let id = entry
        .get("NewEntitlement")
        .or_else(|| entry.get("ChangedEntitlement"))
        .or_else(|| entry.get("ChangedProductMetadata"))
        .and_then(|e| e["BookEntitlement"]["Id"].as_str())
        .or_else(|| entry["ChangedReadingState"]["ReadingState"]["EntitlementId"].as_str());
    id.unwrap_or_else(|| panic!("unrecognised entry: {entry}"))
        .to_owned()
}
