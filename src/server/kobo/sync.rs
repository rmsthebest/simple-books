//! `GET /v1/library/sync`.
//!
//! Each change a user makes takes one value from a per-user sequence. A device
//! holds a token `sb1-<base>-<pos>`:
//!
//! - `pos` is the last sequence value it has received.
//! - `base` is what `pos` was when the current paginated run started. It stays
//!   pinned for the whole run, because whether a book is new to the device
//!   can't be decided from a moving cursor: a book first seen at 5 and edited
//!   to 9 mid-run would otherwise arrive as a change to something unknown.
//!
//! The final page advances both to the user's current sequence.

use axum::{
    Extension,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{Value, json};

use super::{
    KoboContext,
    dto::{BookEntitlement, book_metadata, reading_state, removed_entitlement},
    internal_error, kobo_json,
};
use crate::server::{
    AppState,
    db::{models::BookRow, scope::SyncPageRow},
    now,
};

pub const PAGE_SIZE: i64 = 100;
const SYNC_TOKEN: HeaderName = HeaderName::from_static("x-kobo-synctoken");
const SYNC_CONTINUE: HeaderName = HeaderName::from_static("x-kobo-sync");

/// A device's position in the sync stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub base: i64,
    pub pos: i64,
}

impl Cursor {
    pub const START: Self = Self { base: 0, pos: 0 };

    pub fn encode(self) -> String {
        format!("sb1-{}-{}", self.base, self.pos)
    }

    /// Parses a token. Anything unrecognised, including a token left over from
    /// Kobo's own store, returns `None`, which means a full sync.
    pub fn parse(token: &str) -> Option<Self> {
        let (base, pos) = token.strip_prefix("sb1-")?.split_once('-')?;
        let cursor = Self {
            base: base.parse().ok()?,
            pos: pos.parse().ok()?,
        };
        (cursor.base >= 0 && cursor.base <= cursor.pos).then_some(cursor)
    }

    /// Resolves the request's token against the user's current sequence. A
    /// token from the future, such as after restoring a backup, starts over.
    pub fn resolve(token: Option<&str>, high_water: i64) -> Self {
        token
            .and_then(Self::parse)
            .filter(|cursor| cursor.pos <= high_water)
            .unwrap_or(Self::START)
    }
}

fn book_entry(
    book: &BookRow,
    state: Option<&crate::server::db::models::ReadingStateRow>,
    base: i64,
    url: &str,
    at: i64,
) -> Value {
    let entitlement = BookEntitlement::for_book(book, at);
    let metadata = book_metadata(book, url);
    let is_new = book.first_seq.is_none_or(|first| first > base);
    if is_new {
        json!({ "NewEntitlement": {
            "BookEntitlement": entitlement,
            "BookMetadata": metadata,
            "ReadingState": reading_state(book, state),
        }})
    } else {
        // Firmware 6 ignores the metadata in `ChangedEntitlement`: a Clara BW
        // kept its old title and never fetched the new cover. Reading state
        // isn't needed here, because for a book the device already has it's
        // always sent as its own `ChangedReadingState` row.
        json!({ "ChangedProductMetadata": {
            "BookEntitlement": entitlement,
            "BookMetadata": metadata,
        }})
    }
}

pub async fn sync(
    State(state): State<AppState>,
    Extension(ctx): Extension<KoboContext>,
    headers: HeaderMap,
) -> Response {
    match run(&state, &ctx, &headers).await {
        Ok(response) => response,
        Err(error) => internal_error(&error),
    }
}

async fn run(
    state: &AppState,
    ctx: &KoboContext,
    headers: &HeaderMap,
) -> Result<Response, sqlx::Error> {
    let token = headers.get(&SYNC_TOKEN).and_then(|v| v.to_str().ok());
    let high_water = ctx.scope.high_water().await?;

    let cursor = if ctx.force_full_sync {
        state.db.clear_force_full_sync(ctx.device_id).await?;
        Cursor::START
    } else {
        Cursor::resolve(token, high_water)
    };

    let page = ctx
        .scope
        .sync_page(cursor.base, cursor.pos, PAGE_SIZE)
        .await?;
    let at = now();

    let body: Vec<Value> = page
        .rows
        .iter()
        .map(|row| match row {
            SyncPageRow::Book { book, state, .. } => {
                book_entry(book, state.as_ref(), cursor.base, &ctx.base_url, at)
            }
            SyncPageRow::Tombstone { id, .. } => removed_entitlement(id, at),
            SyncPageRow::ReadingState { book, state, .. } => {
                json!({ "ChangedReadingState": { "ReadingState": reading_state(book, Some(state)) } })
            }
        })
        .collect();

    let next = if page.has_more {
        let pos = page.rows.last().map_or(cursor.pos, SyncPageRow::seq);
        Cursor {
            base: cursor.base,
            pos,
        }
    } else {
        Cursor {
            base: page.high_water,
            pos: page.high_water,
        }
    };

    let mut response = kobo_json(StatusCode::OK, &body);
    if let Ok(value) = HeaderValue::from_str(&next.encode()) {
        response.headers_mut().insert(SYNC_TOKEN, value);
    }
    if page.has_more {
        response
            .headers_mut()
            .insert(SYNC_CONTINUE, HeaderValue::from_static("continue"));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::Cursor;

    #[test]
    fn tokens_round_trip() {
        let cursor = Cursor { base: 3, pos: 7 };
        assert_eq!(Cursor::parse(&cursor.encode()), Some(cursor));
    }

    #[test]
    fn foreign_and_malformed_tokens_are_rejected() {
        for bad in [
            "",
            "sb1-",
            "sb1-1",
            "sb1-a-b",
            "sb1-5-3",
            "sb1--1-3",
            "eyJhIjoxfQ==",
            "sb2-1-1",
        ] {
            assert_eq!(Cursor::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn resolve_starts_over_when_unusable() {
        assert_eq!(Cursor::resolve(None, 10), Cursor::START);
        assert_eq!(Cursor::resolve(Some("junk"), 10), Cursor::START);
        assert_eq!(Cursor::resolve(Some("sb1-11-11"), 10), Cursor::START);
        assert_eq!(
            Cursor::resolve(Some("sb1-4-9"), 10),
            Cursor { base: 4, pos: 9 }
        );
    }
}
