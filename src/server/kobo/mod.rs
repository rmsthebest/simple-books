//! The Kobo store protocol, as spoken by Nickel. Kobo doesn't document this;
//! the rules here come from community reverse engineering and device traces.
//!
//! Every response must be JSON with an explicit `Content-Length`, or Nickel
//! reports "sync failed". The middleware below enforces this for every path.
//!
//! A future store proxy would hook in at exactly three places:
//! [`resources::Resources::build`], [`auth::device`] and [`stubs::fallback`].

pub mod auth;
pub mod dto;
pub mod files;
pub mod library;
pub mod resources;
pub mod stubs;
pub mod sync;
pub mod trace;

use std::{any::Any, fmt};

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, OriginalUri, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{any, delete, get, post},
};
use serde::Serialize;
use tower_http::{catch_panic::CatchPanicLayer, set_header::SetResponseHeaderLayer};

use super::{AppState, auth::device::hash_secret, db::scope::UserScope, http::client_ip};

const BODY_LIMIT: usize = 256 * 1024;

/// Who a Kobo request belongs to, resolved from the token in its path.
#[derive(Clone)]
pub struct KoboContext {
    pub scope: UserScope,
    pub device_id: i64,
    pub force_full_sync: bool,
    /// The device's `api_endpoint`. It contains the token, so `Debug` hides it.
    pub base_url: String,
}

impl fmt::Debug for KoboContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KoboContext")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// Serializes a Kobo JSON response with an explicit `Content-Length`.
pub fn kobo_json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => kobo_bytes(status, body),
        Err(error) => {
            tracing::error!(%error, "failed to serialize a Kobo response");
            kobo_bytes(StatusCode::INTERNAL_SERVER_ERROR, b"{}".to_vec())
        }
    }
}

fn kobo_bytes(status: StatusCode, body: Vec<u8>) -> Response {
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// An empty JSON object, which Nickel accepts for anything it doesn't need.
pub fn empty_object(status: StatusCode) -> Response {
    kobo_bytes(status, b"{}".to_vec())
}

fn panic_response(_: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!("a Kobo handler panicked");
    empty_object(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Turns the store-side failure of a handler into a JSON response.
pub fn internal_error(error: &dyn std::error::Error) -> Response {
    tracing::error!(%error, "Kobo request failed");
    empty_object(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Pulls the token out of `/kobo/<token>/...`.
fn token_from_path(path: &str) -> Option<&str> {
    path.strip_prefix("/kobo/")?
        .split('/')
        .next()
        .filter(|t| !t.is_empty())
}

/// The full request path. Inside a nested router, `Request::uri` has the
/// `/kobo/{token}` prefix stripped, so the token has to come from here.
pub(crate) fn original_path(request: &Request) -> String {
    request.extensions().get::<OriginalUri>().map_or_else(
        || request.uri().path().to_owned(),
        |uri| uri.path().to_owned(),
    )
}

/// Resolves the path token to a device, for every route including the fallback.
async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let path = original_path(&request);
    let Some(token) = token_from_path(&path).map(str::to_owned) else {
        return empty_object(StatusCode::UNAUTHORIZED);
    };

    // A real token can't be guessed. This keeps someone trying from costing lookups,
    // set high enough not to trouble a Kobo sharing an address with them.
    let ip = client_ip::limit_key(client_ip::from_request(
        &state.config,
        request.headers(),
        request.extensions(),
    ));
    if state.limits.device_failures.exceeded(&ip) {
        return empty_object(StatusCode::UNAUTHORIZED);
    }

    let device = match state
        .db
        .kobo_device_by_token_hash(&hash_secret(&token))
        .await
    {
        Ok(device) => device,
        Err(error) => return internal_error(&error),
    };
    let Some(device) = device else {
        state.limits.device_failures.hit(&ip);
        return empty_object(StatusCode::UNAUTHORIZED);
    };

    if let Err(error) = state.db.touch_device(device.device_id).await {
        tracing::warn!(%error, "couldn't record that a device was seen");
    }
    // A reconnected device has now proved its new token works.
    if device.replaces_device_id.is_some()
        && let Err(error) = state.db.finish_reconnect(device.device_id).await
    {
        tracing::warn!(%error, "couldn't retire a replaced device");
    }

    request.extensions_mut().insert(KoboContext {
        scope: state.db.scope(device.user_id),
        device_id: device.device_id,
        force_full_sync: device.force_full_sync,
        base_url: format!("{}/kobo/{token}", state.config.public_url),
    });
    next.run(request).await
}

/// The router mounted at `/kobo/{token}`.
pub fn router(state: AppState) -> Router<AppState> {
    let trace_enabled = state.config.kobo_trace;

    let router = Router::new()
        .route("/", any(stubs::fallback))
        .route("/v1/initialization", get(resources::initialization))
        .route("/v1/auth/device", post(auth::device))
        .route("/v1/auth/refresh", post(auth::device))
        .route("/v1/library/sync", get(sync::sync))
        .route("/v1/library/{id}/metadata", get(library::metadata))
        .route(
            "/v1/library/{id}/state",
            get(library::get_state).put(library::put_state),
        )
        .route("/v1/library/{id}", delete(library::delete_entitlement))
        .route(
            "/covers/{cover}/{width}/{height}/{quality}/{greyscale}/image.jpg",
            get(files::cover),
        )
        .route(
            "/covers/{cover}/{width}/{height}/{greyscale}/image.jpg",
            get(files::cover),
        )
        .route("/download/{id}/{format}", get(files::download))
        .merge(stubs::router())
        .fallback(stubs::fallback)
        .method_not_allowed_fallback(stubs::fallback);

    let router = router.layer(middleware::from_fn_with_state(state, authenticate));
    let router = if trace_enabled {
        router.layer(middleware::from_fn(trace::trace))
    } else {
        router
    };

    router
        // Handlers that read a body handle the rejection themselves, so an
        // oversized request still gets a JSON answer.
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(CatchPanicLayer::custom(panic_response))
        // A proxy must never compress these (Cloudflare would drop
        // Content-Length, which Nickel needs) or cache them.
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-transform"),
        ))
}

#[cfg(test)]
mod tests {
    use super::token_from_path;

    #[test]
    fn extracts_the_token() {
        assert_eq!(token_from_path("/kobo/abc/v1/x"), Some("abc"));
        assert_eq!(token_from_path("/kobo/abc"), Some("abc"));
        assert_eq!(token_from_path("/kobo/"), None);
        assert_eq!(token_from_path("/other/abc"), None);
    }
}
