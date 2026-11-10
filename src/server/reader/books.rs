//! `GET changes`, `GET books/{id}`, `GET books/{id}/cover` and
//! `PUT books/{id}/place`.

use axum::{
    Extension, Json,
    extract::{Path, Query, Request, State, rejection::JsonRejection},
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sync_proto::{
    Book, BookFile, Changes, Place, PlaceAnswer, REVISION_HEADER, Series, Spot, Status,
};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use super::{ReaderContext, internal_error, problem};
use crate::server::{
    AppState,
    db::reader::{PlaceRow, ReaderBook, ReaderChange},
    now,
};

/// Changes per page.
pub const PAGE_SIZE: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct Since {
    #[serde(default)]
    since: i64,
}

fn book(row: ReaderBook) -> Book {
    let authors = row.authors();
    Book {
        id: row.id,
        title: row.title,
        authors,
        series: row.series.map(|name| Series {
            name,
            index: row.series_index,
        }),
        file: row
            .reader_size
            .zip(row.reader_revision)
            .map(|(size, revision)| BookFile {
                size: size.max(0) as u64,
                revision,
            }),
        cover: row.cover_id,
        added: row.added_at,
    }
}

fn status(stored: &str) -> Status {
    match stored {
        "finished" => Status::Finished,
        "reading" => Status::Reading,
        _ => Status::Unread,
    }
}

fn spot(row: &PlaceRow) -> Spot {
    Spot {
        chapter: row.chapter as u32,
        block: row.block as u32,
        offset: row.offset_chars as u32,
        revision: row.revision,
        percent: row.percent as f32,
        status: status(&row.status),
        at: row.read_at,
    }
}

pub async fn changes(
    Extension(ctx): Extension<ReaderContext>,
    State(state): State<AppState>,
    Query(query): Query<Since>,
) -> Response {
    match run_changes(&state, &ctx, query.since).await {
        Ok(changes) => Json(changes).into_response(),
        Err(error) => internal_error(&error),
    }
}

async fn run_changes(
    state: &AppState,
    ctx: &ReaderContext,
    since: i64,
) -> Result<Changes, sqlx::Error> {
    let high_water = ctx.scope.high_water().await?;
    // One from the future, such as after restoring a backup, starts over.
    let mut since = if (0..=high_water).contains(&since) {
        since
    } else {
        0
    };
    if ctx.force_full_sync {
        state.db.clear_force_full_sync(ctx.device_id).await?;
        since = 0;
    }
    let page = ctx.scope.reader_changes(since, PAGE_SIZE).await?;
    let next = if page.more {
        page.changes.last().map_or(since, |(seq, _)| *seq)
    } else {
        page.high_water
    };
    let mut changes = Changes {
        books: Vec::new(),
        removed: Vec::new(),
        places: Vec::new(),
        next,
        more: page.more,
        full: since == 0,
    };
    for (_, change) in page.changes {
        match change {
            ReaderChange::Book(row) => changes.books.push(book(row)),
            ReaderChange::Removed(id) => changes.removed.push(id),
            ReaderChange::Place(row) => changes.places.push(Place {
                book: row.book_id.clone(),
                spot: spot(&row),
            }),
        }
    }
    Ok(changes)
}

async fn serve(path: std::path::PathBuf, request: Request, content_type: &'static str) -> Response {
    let Ok(response) = ServeFile::new(path).oneshot(request).await;
    if response.status() == StatusCode::NOT_FOUND {
        return problem(StatusCode::NOT_FOUND, "no such file");
    }
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

/// The book in the reader's format. `ServeFile` gives it a length and lets
/// an interrupted download carry on with `Range`.
pub async fn download(
    Extension(ctx): Extension<ReaderContext>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Response {
    let file = match ctx.scope.reader_file(id).await {
        Ok(Some(file)) => file,
        Ok(None) => return problem(StatusCode::NOT_FOUND, "no such book, or not converted"),
        Err(error) => return internal_error(&error),
    };
    let mut response = serve(state.layout.reader(id), request, "application/octet-stream").await;
    if response.status().is_success() {
        response.headers_mut().insert(
            HeaderName::from_static(REVISION_HEADER),
            HeaderValue::from(file.revision),
        );
    }
    response
}

pub async fn cover(
    Extension(ctx): Extension<ReaderContext>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Response {
    let cover = match ctx.scope.library_book(id).await {
        Ok(Some(book)) => book.cover_id.and_then(|cover| Uuid::parse_str(&cover).ok()),
        Ok(None) => None,
        Err(error) => return internal_error(&error),
    };
    let Some(cover) = cover else {
        return problem(StatusCode::NOT_FOUND, "no cover");
    };
    serve(state.layout.cover(id, cover), request, "image/jpeg").await
}

pub async fn put_place(
    Extension(ctx): Extension<ReaderContext>,
    Path(id): Path<Uuid>,
    body: Result<Json<Spot>, JsonRejection>,
) -> Response {
    let Ok(Json(sent)) = body else {
        return problem(StatusCode::BAD_REQUEST, "that wasn't a place");
    };
    if !sent.percent.is_finite() || sent.at <= 0 {
        return problem(StatusCode::BAD_REQUEST, "that wasn't a place");
    }
    let row = PlaceRow {
        book_id: id.to_string(),
        chapter: i64::from(sent.chapter),
        block: i64::from(sent.block),
        offset_chars: i64::from(sent.offset),
        revision: sent.revision,
        percent: f64::from(sent.percent.clamp(0.0, 100.0)),
        status: match sent.status {
            Status::Unread => "unread",
            Status::Reading => "reading",
            Status::Finished => "finished",
        }
        .to_owned(),
        // A clock set years ahead would otherwise win every time.
        read_at: sent.at.min(now()),
    };
    match ctx.scope.put_reader_place(id, ctx.device_id, &row).await {
        Ok(Some(outcome)) => Json(PlaceAnswer {
            kept: outcome.kept,
            place: spot(&outcome.place),
        })
        .into_response(),
        Ok(None) => problem(StatusCode::NOT_FOUND, "no such book"),
        Err(error) => internal_error(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_statuses_read_back() {
        for status in [Status::Unread, Status::Reading, Status::Finished] {
            let stored = serde_json::to_value(status).unwrap();
            assert_eq!(super::status(stored.as_str().unwrap()), status);
        }
    }
}
