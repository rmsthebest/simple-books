//! Decides who can see which page, before any of it renders. A page finds its user
//! in the request, so a signed-in page can't render without one.

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};

use crate::server::{AppState, auth::session::CurrentUser};

fn is_public(path: &str) -> bool {
    path == "/login" || path.starts_with("/setup/")
}

/// Whether the path is written the one way this app's own links write it. The page
/// router tidies `/./admin`, `/x/../admin` or `/%2e/admin` into `/admin`, so the
/// checks below would see one path and the router render another. No page's address
/// needs any of these.
fn is_plain(path: &str) -> bool {
    !path.contains(['%', '\\'])
        && !path.contains("//")
        && path
            .split('/')
            .all(|segment| segment != "." && segment != "..")
}

pub async fn pages(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    if !is_plain(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Pages and static files only answer these. Anything else that gets here found
    // no route of its own.
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if path.starts_with("/pkg/") {
        return next.run(request).await;
    }

    let (mut parts, body) = request.into_parts();
    match CurrentUser::from_request_parts(&mut parts, &state).await {
        Ok(user) => {
            if path == "/login" {
                return Redirect::to("/").into_response();
            }
            if path.starts_with("/admin") && !user.is_admin {
                return StatusCode::FORBIDDEN.into_response();
            }
            parts.extensions.insert(user);
        }
        Err(refused) if refused.status() != StatusCode::UNAUTHORIZED => return refused,
        Err(_) if !is_public(&path) => return Redirect::to("/login").into_response(),
        Err(_) => {}
    }
    next.run(Request::from_parts(parts, body)).await
}
