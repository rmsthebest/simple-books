//! Everything that only runs on the server.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod auth;
pub mod config;
pub mod convert;
pub mod db;
pub mod error;
pub mod hardcover;
pub mod http;
pub mod kobo;
pub mod library;
pub mod reader;
pub mod router;
pub mod storage;
#[doc(hidden)]
pub mod testkit;
pub mod web;

use std::sync::Arc;

use axum::extract::FromRef;
use leptos::config::LeptosOptions;
use tokio::sync::Notify;

/// Shared state for every request.
#[derive(Clone, Debug)]
pub struct AppState {
    pub config: Arc<config::Config>,
    pub db: db::Db,
    pub layout: storage::layout::Layout,
    pub kobo_resources: Arc<kobo::resources::Resources>,
    pub leptos: LeptosOptions,
    /// Woken when a book is waiting to be converted.
    pub conversions: Arc<Notify>,
    pub uploads: Arc<library::upload::UploadGate>,
    pub limits: Arc<http::limit::Limits>,
    /// `None` when Hardcover is turned off.
    pub hardcover: Option<Arc<hardcover::Hardcover>>,
    /// The link that makes the first admin, while there are no users.
    pub first_run: Arc<auth::first_run::FirstRun>,
    /// What the website's installer puts on a Kobo.
    pub kobo_files: Arc<reader::install::KoboFiles>,
}

impl FromRef<AppState> for LeptosOptions {
    fn from_ref(state: &AppState) -> Self {
        state.leptos.clone()
    }
}

/// Current time as Unix seconds.
pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
