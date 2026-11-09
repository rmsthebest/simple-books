//! Book downloads and covers. `ServeFile` provides `Content-Length` and `Range`
//! support, and paths come only from parsed UUIDs.

use axum::{
    Extension,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use super::{
    KoboContext, empty_object, internal_error,
    resources::{API_TOKEN, API_TOKEN_HEADER},
};
use crate::server::{AppState, storage::covers::THUMB_WIDTH};

#[derive(Debug, Deserialize)]
pub struct DownloadPath {
    id: String,
    format: String,
}

#[derive(Debug, Deserialize)]
pub struct CoverPath {
    cover: String,
    width: Option<String>,
}

async fn serve(path: std::path::PathBuf, request: Request, content_type: &'static str) -> Response {
    let Ok(response) = ServeFile::new(path).oneshot(request).await;
    if response.status() == StatusCode::NOT_FOUND {
        return empty_object(StatusCode::NOT_FOUND);
    }
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

pub async fn download(
    State(state): State<AppState>,
    Extension(ctx): Extension<KoboContext>,
    Path(path): Path<DownloadPath>,
    request: Request,
) -> Response {
    let Ok(id) = Uuid::parse_str(&path.id) else {
        return empty_object(StatusCode::NOT_FOUND);
    };
    let book = match ctx.scope.library_book(id).await {
        Ok(Some(book)) => book,
        Ok(None) => return empty_object(StatusCode::NOT_FOUND),
        Err(error) => return internal_error(&error),
    };

    let (file, extension) = match path.format.as_str() {
        "kepub" if book.ready_kepub_size().is_some() => (state.layout.kepub(id), "kepub.epub"),
        "epub" => (state.layout.original(id), "epub"),
        _ => return empty_object(StatusCode::NOT_FOUND),
    };

    let mut response = serve(file, request, "application/octet-stream").await;
    if response.status().is_success() {
        let headers = response.headers_mut();
        headers.insert(API_TOKEN_HEADER, HeaderValue::from_static(API_TOKEN));
        // The name is built from a UUID, so it can't need escaping.
        if let Ok(value) =
            HeaderValue::from_str(&format!("attachment; filename=\"{id}.{extension}\""))
        {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    response
}

/// Serves the smaller stored cover if that's big enough for what's asked, and
/// the full one otherwise. Nickel scales it to fit.
pub async fn cover(
    State(state): State<AppState>,
    Extension(ctx): Extension<KoboContext>,
    Path(path): Path<CoverPath>,
    request: Request,
) -> Response {
    let Ok(cover) = Uuid::parse_str(&path.cover) else {
        return empty_object(StatusCode::NOT_FOUND);
    };
    let book = match ctx.scope.book_id_by_cover(cover).await {
        Ok(Some(book)) => book,
        Ok(None) => return empty_object(StatusCode::NOT_FOUND),
        Err(error) => return internal_error(&error),
    };
    let wide = path
        .width
        .and_then(|width| width.parse::<u32>().ok())
        .is_none_or(|width| width > THUMB_WIDTH);
    let thumb = state.layout.thumb(book, cover);
    let file = if !wide && tokio::fs::try_exists(&thumb).await.unwrap_or(false) {
        thumb
    } else {
        state.layout.cover(book, cover)
    };
    serve(file, request, "image/jpeg").await
}
