//! `GET kobo` and `GET kobo/{name}`: the reader app's own files, so a
//! paired Kobo can update itself to what this server would install. They're
//! the app folder's files among the installer's (see [`super::install`]).

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use super::{internal_error, problem};
use crate::server::AppState;

const NONE: &str = "this server has no Kobo files";

pub async fn manifest(State(state): State<AppState>) -> Response {
    match state.kobo_files.app().await {
        Ok(Some(app)) => Json(app.manifest.clone()).into_response(),
        Ok(None) => problem(StatusCode::NOT_FOUND, NONE),
        Err(error) => internal_error(&error),
    }
}

pub async fn file(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let app = match state.kobo_files.app().await {
        Ok(Some(app)) => app,
        Ok(None) => return problem(StatusCode::NOT_FOUND, NONE),
        Err(error) => return internal_error(&error),
    };
    match app.file(&name) {
        Some(bytes) => {
            ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
        }
        None => problem(StatusCode::NOT_FOUND, "no such file"),
    }
}
