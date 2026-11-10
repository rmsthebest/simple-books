//! What the pages' forms post to. Each is a plain form followed by a redirect, or a
//! re-render when it has a secret to show, so that nothing here needs JavaScript and
//! no secret ever sits in a URL.

mod accounts;
mod admin;
mod books;
mod devices;
pub mod gate;
mod hardcover;
mod status;

use axum::{
    Router,
    body::Body,
    extract::Request,
    middleware,
    response::{Redirect, Response},
    routing::{get, post},
};
use leptos::prelude::provide_context;
use tower_http::limit::RequestBodyLimitLayer;

use super::{AppState, auth::session::CurrentUser, http};
use crate::app::shell;

const FORM_BODY_LIMIT: usize = 256 * 1024;

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/status", get(status::waiting_on))
        .route("/login", post(accounts::log_in))
        .route("/logout", post(accounts::log_out))
        .route("/setup/{token}", post(accounts::set_password))
        .route("/books/{id}/edit", post(books::edit))
        .route("/books/{id}/visibility", post(books::visibility))
        .route("/books/{id}/add", post(books::add))
        .route("/books/{id}/remove", post(books::remove))
        .route("/device/connect", post(devices::connect))
        .route("/device/pair", post(devices::pair))
        .route("/device/{id}/reconnect", post(devices::reconnect))
        .route("/device/{id}/resync", post(devices::resync))
        .route("/device/{id}/remove", post(devices::remove))
        .route("/hardcover/connect", post(hardcover::connect))
        .route("/hardcover/cancel", post(hardcover::cancel))
        .route("/hardcover/disconnect", post(hardcover::disconnect))
        .route("/books/{id}/hardcover", post(hardcover::choose))
        .route("/admin/users", post(admin::create_user))
        .route("/admin/users/{id}/setup-link", post(admin::setup_link))
        .route("/admin/users/{id}/disable", post(admin::disable))
        .route("/admin/users/{id}/enable", post(admin::enable))
        .route("/admin/users/{id}/quota", post(admin::quota))
        .layer(RequestBodyLimitLayer::new(FORM_BODY_LIMIT))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            http::origin::reject_cross_origin,
        ))
}

/// Something only a form's answer shows, because it can't be shown again.
#[derive(Debug, Clone)]
pub enum Revealed {
    /// The address for a Kobo's config, with its secret.
    KoboEndpoint(String),
    /// A link for someone to set their password.
    SetupLink { username: String, link: String },
}

/// Renders a page in answer to a form, with something for it to reveal.
async fn render_with(
    state: AppState,
    user: CurrentUser,
    mut parts: axum::http::request::Parts,
    path: &'static str,
    revealed: Revealed,
) -> Response {
    parts.uri = axum::http::Uri::from_static(path);
    parts.extensions.insert(user);
    parts.extensions.insert(revealed);
    let request = Request::from_parts(parts, Body::empty());

    let options = state.leptos.clone();
    let handler = leptos_axum::render_app_async_with_context(
        move || provide_context(state.clone()),
        move || shell(options.clone()),
    );
    handler(request).await
}

fn to(path: &str) -> Redirect {
    Redirect::to(path)
}
