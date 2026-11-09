//! Files for browsers: the book as uploaded, and its covers. The URLs hold nothing
//! secret, because each request is checked against the session.

use std::fmt::Write as _;

use axum::{
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use crate::server::{AppState, auth::session::CurrentUser, error::AppError};

const MAX_FILENAME_CHARS: usize = 120;

#[derive(Debug, Deserialize)]
pub struct CoverPath {
    id: Uuid,
    cover: Uuid,
    size: String,
}

async fn send(path: std::path::PathBuf, request: Request) -> Response {
    let Ok(response) = ServeFile::new(path).oneshot(request).await;
    response.into_response()
}

/// `filename*` carries the real title. The plain `filename` is for old clients, so
/// it keeps only what can't be misread.
fn attachment(title: &str) -> Option<HeaderValue> {
    let title: String = title.chars().take(MAX_FILENAME_CHARS).collect();
    let plain: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " .-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();

    let mut encoded = String::with_capacity(title.len());
    for byte in title.bytes() {
        if byte.is_ascii_alphanumeric() || b".-_~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{plain}.epub\"; filename*=UTF-8''{encoded}.epub"
    ))
    .ok()
}

/// The original file, exactly as uploaded.
pub async fn download(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Result<Response, AppError> {
    let book = user
        .scope
        .readable_book(id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut response = send(state.layout.original(id), request).await;
    if response.status().is_success() {
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/epub+zip"),
        );
        if let Some(value) = attachment(&book.title) {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    Ok(response)
}

/// A cover. Its ID changes when it's replaced, so a browser can keep it for good.
pub async fn cover(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(path): Path<CoverPath>,
    request: Request,
) -> Result<Response, AppError> {
    let book = user
        .scope
        .readable_book(path.id)
        .await?
        .ok_or(AppError::NotFound)?;
    if book.cover_id.as_deref() != Some(path.cover.to_string().as_str()) {
        return Err(AppError::NotFound);
    }
    let file = match path.size.as_str() {
        "full.jpg" => state.layout.cover(path.id, path.cover),
        "thumb.jpg" => state.layout.thumb(path.id, path.cover),
        _ => return Err(AppError::NotFound),
    };
    let mut response = send(file, request).await;
    if response.status() == StatusCode::OK {
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_cannot_break_out_of_the_header() {
        let value = attachment("a\"; evil=\"x\r\nSet-Cookie: y").unwrap();
        let value = value.to_str().unwrap();
        assert!(value.starts_with("attachment; filename=\"a__ evil__x__Set-Cookie_ y.epub\""));
        assert!(!value.contains('\r') && !value.contains('\n'));
        assert_eq!(value.matches("Set-Cookie:").count(), 0);
    }

    #[test]
    fn real_titles_survive_in_the_encoded_name() {
        let value = attachment("Título 1").unwrap();
        let value = value.to_str().unwrap();
        assert!(
            value.contains("filename*=UTF-8''T%C3%ADtulo%201.epub"),
            "{value}"
        );
    }
}
