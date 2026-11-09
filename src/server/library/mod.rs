//! The web side of the library: getting files in and out, for a signed-in user.
//! Every route takes its user from the session, and its book through `UserScope`.

pub mod serve;
pub mod upload;
pub mod work;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use tower_http::limit::RequestBodyLimitLayer;

use super::{AppState, http};

/// Room for a cover image and the form around it.
const COVER_BODY_LIMIT: usize = 34 * 1024 * 1024;
/// Room for the form around a book, on top of the book itself.
const FORM_OVERHEAD: usize = 64 * 1024;
const OTHER_BODY_LIMIT: usize = 64 * 1024;

pub fn router(state: &AppState) -> Router<AppState> {
    let upload_limit =
        usize::try_from(state.config.max_upload_bytes).unwrap_or(usize::MAX) + FORM_OVERHEAD;

    let books = Router::new()
        .route("/books/upload", post(upload::upload))
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(upload_limit));
    let covers = Router::new()
        .route("/books/{id}/cover", post(upload::replace_cover))
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(COVER_BODY_LIMIT));
    let rest = Router::new()
        .route("/books/{id}/delete", post(upload::delete))
        .route("/books/{id}/download", get(serve::download))
        .route("/books/{id}/cover/{cover}/{size}", get(serve::cover))
        .layer(RequestBodyLimitLayer::new(OTHER_BODY_LIMIT));

    books
        .merge(covers)
        .merge(rest)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            http::origin::reject_cross_origin,
        ))
}
