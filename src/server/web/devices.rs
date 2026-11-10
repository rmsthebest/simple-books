//! Connecting a Kobo. Its address has a secret in it that can't be shown twice, so
//! the answer to the form is the page itself, never a redirect with it in the URL.

use axum::{
    Form, Json,
    extract::{Path, State},
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};

use serde::Deserialize;

use super::{Revealed, render_with, to};
use crate::server::{
    AppState,
    auth::{device, session::CurrentUser},
    error::AppError,
    reader::{
        install,
        pair::{self, Entered},
    },
};

#[derive(Debug, Deserialize)]
pub struct CodeForm {
    code: String,
}

/// `POST /device/pair`: the code a simple-books reader shows.
pub async fn pair(
    user: CurrentUser,
    State(state): State<AppState>,
    Form(form): Form<CodeForm>,
) -> Result<Response, AppError> {
    Ok(match pair::enter(&state, user.id, &form.code).await? {
        Entered::Paired(_) => to("/device?done=paired"),
        Entered::Wrong => to("/device?error=pair_code"),
        Entered::Slow => to("/device?error=slow"),
    }
    .into_response())
}

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

/// `GET /device/kobo-files`: what the installer writes onto a Kobo, as a
/// tar. See `server::reader::install`.
pub async fn kobo_files(
    _user: CurrentUser,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let Some(tar) = state.kobo_files.tar().await? else {
        return Ok((
            StatusCode::NOT_FOUND,
            "This server has no Kobo files to install: build them with \
             scripts/build-kobo-files.sh and point KOBO_FILES at them.",
        )
            .into_response());
    };
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        tar,
    )
        .into_response())
}

/// `POST /device/kobo-install`: pairs the Kobo the installer just wrote
/// to. The answer has its secret, so it's JSON for the installer and
/// never stored by the browser.
pub async fn kobo_install(
    user: CurrentUser,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let installed = install::install(&state.db, &state.config.public_url, user.id).await?;
    tracing::info!(user = user.id, "a Kobo was installed from the website");
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(installed)).into_response())
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
