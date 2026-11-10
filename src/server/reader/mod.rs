//! The simple-books reader's own sync protocol, under `/device/v1`. The types
//! are in `sync-proto`, shared with the reader. Unlike the Kobo routes, a
//! reader's secret comes in the `Authorization` header, so it's never in a
//! path or a log.

pub mod books;
pub mod pair;

use std::fmt;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use sync_proto::Problem;
use tower_http::set_header::SetResponseHeaderLayer;

use super::{AppState, auth::device::hash_secret, db::scope::UserScope, http::client_ip};

/// Who a reader request belongs to, from its secret.
#[derive(Clone)]
pub struct ReaderContext {
    pub scope: UserScope,
    pub device_id: i64,
    pub force_full_sync: bool,
}

impl fmt::Debug for ReaderContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReaderContext")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// Resolves the bearer secret to a paired reader, for every route but
/// pairing's own.
async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let ip = client_ip::limit_key(client_ip::from_request(
        &state.config,
        request.headers(),
        request.extensions(),
    ));
    if state.limits.device_failures.exceeded(&ip) {
        return problem(StatusCode::TOO_MANY_REQUESTS, "too many wrong secrets");
    }
    let Some(secret) = bearer(request.headers()) else {
        return problem(StatusCode::UNAUTHORIZED, "no secret given");
    };
    let device = match state
        .db
        .reader_device_by_token_hash(&hash_secret(secret))
        .await
    {
        Ok(Some(device)) => device,
        Ok(None) => {
            state.limits.device_failures.hit(&ip);
            return problem(StatusCode::UNAUTHORIZED, "not paired; pair it again");
        }
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = state.db.touch_device(device.device_id).await {
        tracing::warn!(%error, "couldn't record that a device was seen");
    }
    request.extensions_mut().insert(ReaderContext {
        scope: state.db.scope(device.user_id),
        device_id: device.device_id,
        force_full_sync: device.force_full_sync,
    });
    next.run(request).await
}

/// Every request body here is small JSON.
const BODY_LIMIT: usize = 16 * 1024;

/// The router mounted at [`sync_proto::PREFIX`].
pub fn router(state: &AppState) -> Router<AppState> {
    let synced = Router::new()
        .route("/changes", get(books::changes))
        .route("/books/{id}", get(books::download))
        .route("/books/{id}/cover", get(books::cover))
        .route("/books/{id}/place", put(books::put_place))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/pair", post(pair::start).get(pair::status))
        .merge(synced)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

/// A failure, as JSON the reader can show.
pub(crate) fn problem(status: StatusCode, error: &str) -> Response {
    (
        status,
        Json(Problem {
            error: error.to_owned(),
        }),
    )
        .into_response()
}

pub(crate) fn internal_error(error: &dyn std::error::Error) -> Response {
    tracing::error!(%error, "reader request failed");
    problem(
        StatusCode::INTERNAL_SERVER_ERROR,
        "the server had a problem",
    )
}

/// The secret in `Authorization: Bearer <secret>`.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
}
