//! Connecting a Kobo. Its address has a secret in it that can't be shown twice, so
//! the answer to the form is the page itself, never a redirect with it in the URL.

use axum::{
    extract::{Path, State},
    http::request::Parts,
    response::{IntoResponse, Response},
};

use super::{Revealed, render_with, to};
use crate::server::{
    AppState,
    auth::{device, session::CurrentUser},
    error::AppError,
};

/// `POST /device/connect`
pub async fn connect(
    user: CurrentUser,
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Response, AppError> {
    let endpoint = device::connect(&state.db, &state.config.public_url, user.id).await?;
    Ok(render_with(
        state,
        user,
        parts,
        "/device",
        Revealed::KoboEndpoint(endpoint),
    )
    .await)
}

/// `POST /device/{id}/reconnect`
pub async fn reconnect(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    parts: Parts,
) -> Result<Response, AppError> {
    let endpoint = device::reconnect(&state.db, &state.config.public_url, user.id, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(render_with(
        state,
        user,
        parts,
        "/device",
        Revealed::KoboEndpoint(endpoint),
    )
    .await)
}

/// `POST /device/{id}/resync`
pub async fn resync(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if !state.db.request_full_sync(user.id, id).await? {
        return Err(AppError::NotFound);
    }
    Ok(to("/device?done=resync").into_response())
}

/// `POST /device/{id}/remove`
pub async fn remove(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if !state.db.revoke_device(user.id, id).await? {
        return Err(AppError::NotFound);
    }
    Ok(to("/device?done=device_removed").into_response())
}
