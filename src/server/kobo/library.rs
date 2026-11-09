//! Per-book library endpoints.
//!
//! An unknown ID gets an empty success here, not a 404: the device may ask
//! about Kobo store purchases this server has never seen, and a 404 makes
//! Nickel fail the whole sync. No data is returned, so nothing leaks.

use axum::{
    Extension,
    body::Bytes,
    extract::{Path, rejection::BytesRejection},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    KoboContext,
    dto::{STATE_FIELDS, book_metadata, kobo_time, reading_state},
    empty_object, internal_error, kobo_json,
};

/// The state stored per book is capped, whatever the request limit allows.
const MAX_STATE_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct BookPath {
    id: String,
}

impl BookPath {
    fn uuid(&self) -> Option<Uuid> {
        Uuid::parse_str(&self.id).ok()
    }
}

/// Returned as a one-element array: Nickel silently skips the download if this
/// is a bare object.
pub async fn metadata(
    Extension(ctx): Extension<KoboContext>,
    Path(path): Path<BookPath>,
) -> Response {
    let Some(id) = path.uuid() else {
        return empty_object(StatusCode::OK);
    };
    match ctx.scope.library_book(id).await {
        Ok(Some(book)) => kobo_json(
            StatusCode::OK,
            &json!([book_metadata(&book, &ctx.base_url)]),
        ),
        Ok(None) => empty_object(StatusCode::OK),
        Err(error) => internal_error(&error),
    }
}

pub async fn get_state(
    Extension(ctx): Extension<KoboContext>,
    Path(path): Path<BookPath>,
) -> Response {
    let Some(id) = path.uuid() else {
        return empty_object(StatusCode::OK);
    };
    let book = match ctx.scope.library_book(id).await {
        Ok(Some(book)) => book,
        Ok(None) => return empty_object(StatusCode::OK),
        Err(error) => return internal_error(&error),
    };
    match ctx.scope.reading_state(id).await {
        Ok(state) => kobo_json(
            StatusCode::OK,
            &json!([reading_state(&book, state.as_ref())]),
        ),
        Err(error) => internal_error(&error),
    }
}

/// Reduces a device's state update to the fields worth keeping.
fn extract_state(body: &[u8]) -> Option<(String, Option<f64>, String)> {
    let parsed: Value = serde_json::from_slice(body).ok()?;
    let update = parsed.get("ReadingStates")?.get(0)?.as_object()?;

    let kept: Map<String, Value> = STATE_FIELDS
        .iter()
        .filter_map(|name| {
            update
                .get(*name)
                .map(|value| ((*name).to_owned(), value.clone()))
        })
        .collect();

    let status = kept
        .get("StatusInfo")
        .and_then(|info| info.get("Status"))
        .and_then(Value::as_str)
        .unwrap_or("ReadyToRead")
        .to_owned();
    let percent = kept
        .get("CurrentBookmark")
        .and_then(|bookmark| bookmark.get("ProgressPercent"))
        .and_then(Value::as_f64);

    let json = serde_json::to_string(&kept).ok()?;
    (json.len() <= MAX_STATE_BYTES).then_some((status, percent, json))
}

pub async fn put_state(
    Extension(ctx): Extension<KoboContext>,
    Path(path): Path<BookPath>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(id) = path.uuid() else {
        return empty_object(StatusCode::OK);
    };
    let Ok(body) = body else {
        return empty_object(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let Some((status, percent, state_json)) = extract_state(&body) else {
        return empty_object(StatusCode::BAD_REQUEST);
    };

    let last_modified = match ctx
        .scope
        .put_reading_state(id, &status, percent, &state_json)
        .await
    {
        Ok(Some(at)) => kobo_time(at),
        Ok(None) => return empty_object(StatusCode::OK),
        Err(error) => return internal_error(&error),
    };

    kobo_json(
        StatusCode::OK,
        &json!({
            "RequestResult": "Success",
            "UpdateResults": [{
                "EntitlementId": id,
                "CurrentBookmarkResult": { "Result": "Success" },
                "StatisticsResult": { "Result": "Success" },
                "StatusInfoResult": { "Result": "Success" },
                "LastModified": last_modified,
                "PriorityTimestamp": last_modified,
            }],
        }),
    )
}

/// Always a no-op success. Removing a book from an e-reader must not destroy
/// the only server copy, and a 404 on an unknown ID fails the whole sync.
pub async fn delete_entitlement() -> Response {
    empty_object(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_known_fields_and_extracts_progress() {
        let body = json!({ "ReadingStates": [{
            "EntitlementId": "x",
            "Junk": "dropped",
            "StatusInfo": { "Status": "Reading" },
            "CurrentBookmark": { "ProgressPercent": 37 },
        }]});
        let (status, percent, stored) = extract_state(body.to_string().as_bytes()).unwrap();
        assert_eq!(status, "Reading");
        assert_eq!(percent, Some(37.0));
        assert!(!stored.contains("Junk"));
    }

    #[test]
    fn rejects_malformed_and_oversized_state() {
        assert!(extract_state(b"not json").is_none());
        assert!(extract_state(b"{}").is_none());
        assert!(extract_state(br#"{"ReadingStates":[]}"#).is_none());

        let big = "x".repeat(MAX_STATE_BYTES + 1);
        let body = json!({ "ReadingStates": [{ "CurrentBookmark": { "Note": big } }] });
        assert!(extract_state(body.to_string().as_bytes()).is_none());
    }
}
