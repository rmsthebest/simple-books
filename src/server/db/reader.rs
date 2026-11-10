//! What a simple-books reader syncs: the books in the library with their
//! reader files, the ones removed, and where readers are in them. Built on
//! the same per-user sequence as the Kobo sync, so one change takes one
//! number whichever kind of device reads it.

use uuid::Uuid;

use super::scope::{UserScope, next_seq, tell_hardcover};
use crate::server::hardcover::rules;

/// A book as a reader is told about it.
#[derive(Debug, Clone)]
pub struct ReaderBook {
    pub id: String,
    pub title: String,
    pub authors_json: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub cover_id: Option<String>,
    /// All set once the reader file exists.
    pub reader_size: Option<i64>,
    pub reader_revision: Option<i64>,
    pub reader_sha256: Option<String>,
    pub added_at: i64,
}

impl ReaderBook {
    pub fn authors(&self) -> Vec<String> {
        serde_json::from_str(&self.authors_json).unwrap_or_default()
    }
}

/// Where a reader is in a book.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceRow {
    pub book_id: String,
    pub chapter: i64,
    pub block: i64,
    pub offset_chars: i64,
    pub revision: i64,
    pub percent: f64,
    /// `unread`, `reading` or `finished`.
    pub status: String,
    pub read_at: i64,
}

#[derive(Debug, Clone)]
pub enum ReaderChange {
    Book(ReaderBook),
    Removed(String),
    Place(PlaceRow),
}

/// One page of changes, read in one transaction.
#[derive(Debug)]
pub struct ReaderPage {
    /// In sequence order, each with its sequence number.
    pub changes: Vec<(i64, ReaderChange)>,
    pub more: bool,
    pub high_water: i64,
}

/// A reader file ready to download.
#[derive(Debug, Clone, Copy)]
pub struct ReaderFile {
    pub size: i64,
    pub revision: i64,
}

/// What became of a place sent by a reader.
#[derive(Debug, Clone)]
pub struct PlaceOutcome {
    pub kept: bool,
    /// What's stored now.
    pub place: PlaceRow,
}

impl UserScope {
    /// The changes after `since`, at most `limit` of them.
    ///
    /// A removal is sent when the book's `first_seq <= since`: the reader may
    /// have had it. One added and removed since then it never saw. A run from 0
    /// sends no removals, since the reader drops whatever the run didn't list.
    pub async fn reader_changes(&self, since: i64, limit: i64) -> Result<ReaderPage, sqlx::Error> {
        let user_id = self.user_id();
        let mut tx = self.db().read.begin().await?;
        let high_water = sqlx::query!("SELECT change_seq FROM users WHERE id = ?1", user_id)
            .fetch_one(&mut *tx)
            .await?
            .change_seq;

        // The first `limit` by sequence are among the first `limit + 1` of each
        // kind, and the one past the limit says there's more.
        let fetch = limit + 1;
        let mut changes: Vec<(i64, ReaderChange)> = Vec::new();
        let books = sqlx::query!(
            r#"SELECT e.change_seq, b.id AS "id!", b.title, b.authors_json, b.series,
                      b.series_index, b.cover_id, b.reader_size, b.reader_revision, b.reader_sha256,
                      e.added_at
               FROM library_entries e JOIN books b ON b.id = e.book_id
               WHERE e.user_id = ?1 AND e.change_seq > ?2 AND e.first_seq IS NOT NULL
               ORDER BY e.change_seq LIMIT ?3"#,
            user_id,
            since,
            fetch
        )
        .fetch_all(&mut *tx)
        .await?;
        changes.extend(books.into_iter().map(|r| {
            (
                r.change_seq,
                ReaderChange::Book(ReaderBook {
                    id: r.id,
                    title: r.title,
                    authors_json: r.authors_json,
                    series: r.series,
                    series_index: r.series_index,
                    cover_id: r.cover_id,
                    reader_size: r.reader_size,
                    reader_revision: r.reader_revision,
                    reader_sha256: r.reader_sha256,
                    added_at: r.added_at,
                }),
            )
        }));
        let removed = sqlx::query!(
            "SELECT change_seq, book_id FROM book_tombstones
             WHERE user_id = ?1 AND change_seq > ?2 AND first_seq <= ?2
             ORDER BY change_seq LIMIT ?3",
            user_id,
            since,
            fetch
        )
        .fetch_all(&mut *tx)
        .await?;
        changes.extend(
            removed
                .into_iter()
                .map(|r| (r.change_seq, ReaderChange::Removed(r.book_id))),
        );
        let places = sqlx::query!(
            "SELECT p.change_seq, p.book_id, p.chapter, p.block, p.offset_chars, p.revision,
                    p.percent, p.status, p.read_at
             FROM reader_places p
             JOIN library_entries e ON e.user_id = p.user_id AND e.book_id = p.book_id
             WHERE p.user_id = ?1 AND p.change_seq > ?2 AND e.first_seq IS NOT NULL
             ORDER BY p.change_seq LIMIT ?3",
            user_id,
            since,
            fetch
        )
        .fetch_all(&mut *tx)
        .await?;
        changes.extend(places.into_iter().map(|r| {
            (
                r.change_seq,
                ReaderChange::Place(PlaceRow {
                    book_id: r.book_id,
                    chapter: r.chapter,
                    block: r.block,
                    offset_chars: r.offset_chars,
                    revision: r.revision,
                    percent: r.percent,
                    status: r.status,
                    read_at: r.read_at,
                }),
            )
        }));
        tx.rollback().await?;

        changes.sort_by_key(|(seq, _)| *seq);
        let more = changes.len() as i64 > limit;
        changes.truncate(limit as usize);
        Ok(ReaderPage {
            changes,
            more,
            high_water,
        })
    }

    /// The reader file of a book in this library, if it's been made.
    pub async fn reader_file(&self, book: Uuid) -> Result<Option<ReaderFile>, sqlx::Error> {
        let id = book.to_string();
        let user_id = self.user_id();
        let row = sqlx::query!(
            "SELECT b.reader_size, b.reader_revision
             FROM library_entries e JOIN books b ON b.id = e.book_id
             WHERE e.user_id = ?1 AND e.book_id = ?2",
            user_id,
            id
        )
        .fetch_optional(&self.db().read)
        .await?;
        Ok(row.and_then(|r| {
            Some(ReaderFile {
                size: r.reader_size?,
                revision: r.reader_revision?,
            })
        }))
    }

    /// Stores where a reader is in a book, unless a newer place is already
    /// there. `None` if the book isn't in this library.
    pub async fn put_reader_place(
        &self,
        book: Uuid,
        device_id: i64,
        place: &PlaceRow,
    ) -> Result<Option<PlaceOutcome>, sqlx::Error> {
        let id = book.to_string();
        let user_id = self.user_id();
        let mut tx = self.db().write.begin().await?;
        let exists = sqlx::query!(
            r#"SELECT 1 AS "found!: i64" FROM library_entries WHERE user_id = ?1 AND book_id = ?2"#,
            user_id,
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if !exists {
            return Ok(None);
        }
        let stored = sqlx::query_as!(
            PlaceRow,
            "SELECT book_id, chapter, block, offset_chars, revision, percent, status, read_at
             FROM reader_places WHERE user_id = ?1 AND book_id = ?2",
            user_id,
            id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(stored) = stored.filter(|stored| stored.read_at > place.read_at) {
            return Ok(Some(PlaceOutcome {
                kept: false,
                place: stored,
            }));
        }

        let seq = next_seq(&mut tx, user_id).await?;
        sqlx::query!(
            "INSERT INTO reader_places (user_id, book_id, chapter, block, offset_chars, revision,
                                        percent, status, read_at, device_id, change_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (user_id, book_id) DO UPDATE SET
                 chapter = excluded.chapter, block = excluded.block,
                 offset_chars = excluded.offset_chars, revision = excluded.revision,
                 percent = excluded.percent, status = excluded.status,
                 read_at = excluded.read_at, device_id = excluded.device_id,
                 change_seq = excluded.change_seq",
            user_id,
            id,
            place.chapter,
            place.block,
            place.offset_chars,
            place.revision,
            place.percent,
            place.status,
            place.read_at,
            device_id,
            seq
        )
        .execute(&mut *tx)
        .await?;
        let want = match place.status.as_str() {
            "finished" => rules::READ,
            "reading" => rules::READING,
            _ => rules::WANT_TO_READ,
        };
        let at = crate::server::now();
        tell_hardcover(&mut tx, user_id, &id, want, Some(place.percent), at).await?;
        tx.commit().await?;
        Ok(Some(PlaceOutcome {
            kept: true,
            place: PlaceRow {
                book_id: id,
                ..place.clone()
            },
        }))
    }
}
