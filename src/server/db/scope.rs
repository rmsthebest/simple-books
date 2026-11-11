//! All access to books goes through here, and every query is bound to the
//! user. A handler can't reach what its user shouldn't, because it never has a
//! way to issue an unscoped query.
//!
//! Three levels of access, kept as separate methods so that a query can't pick
//! up the wrong check by accident:
//!
//! - **Library** ([`UserScope::library_book`]): books the user has added. This
//!   is all a device can reach.
//! - **Readable** ([`UserScope::readable_book`]): their own books and anyone's
//!   shared books.
//! - **Editable** (`require_edit`): their own books, plus shared books for
//!   admins. Another user's private book reads as not found, admins included.

use sqlx::{Sqlite, Transaction};
use uuid::Uuid;

use super::{
    Db,
    models::{
        BookCard, BookRow, MetadataEdit, NewBook, Reading, ReadingStateRow, SyncRow, SyncRowKind,
        Visibility,
    },
};
use crate::server::now;

/// How long reading progress waits before going to Hardcover, so that a few
/// updates in a row go as one.
pub const HARDCOVER_PROGRESS_DELAY: i64 = 60;

#[derive(Clone, Debug)]
pub struct UserScope {
    db: Db,
    user_id: i64,
}

/// Why a change to a book didn't happen.
#[derive(Debug, thiserror::Error)]
pub enum BookError {
    #[error("no such book")]
    NotFound,
    #[error("not allowed")]
    Forbidden,
    #[error("another user already shares this file")]
    AlreadyShared,
    #[error("this user already has this file")]
    AlreadyYours,
    #[error("this would go over the storage limit")]
    QuotaExceeded,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// An existing book with the same file as an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Duplicate {
    /// The user already uploaded it.
    Own(Uuid),
    /// Someone else shares it.
    Shared(Uuid),
}

/// A consistent view of one sync page.
#[derive(Debug)]
pub struct SyncPage {
    pub rows: Vec<SyncPageRow>,
    pub has_more: bool,
    /// `users.change_seq`, read in the same transaction as the rows.
    pub high_water: i64,
}

#[derive(Debug)]
pub enum SyncPageRow {
    Book {
        book: BookRow,
        state: Option<ReadingStateRow>,
        seq: i64,
    },
    Tombstone {
        id: String,
        seq: i64,
    },
    ReadingState {
        book: BookRow,
        state: ReadingStateRow,
        seq: i64,
    },
}

impl SyncPageRow {
    pub fn seq(&self) -> i64 {
        match self {
            Self::Book { seq, .. }
            | Self::Tombstone { seq, .. }
            | Self::ReadingState { seq, .. } => *seq,
        }
    }
}

impl UserScope {
    pub(super) fn new(db: Db, user_id: i64) -> Self {
        Self { db, user_id }
    }

    pub fn user_id(&self) -> i64 {
        self.user_id
    }

    pub(super) fn db(&self) -> &Db {
        &self.db
    }

    /// The highest sync sequence allocated to this user.
    pub async fn high_water(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!("SELECT change_seq FROM users WHERE id = ?1", self.user_id)
            .fetch_one(&self.db.read)
            .await?;
        Ok(row.change_seq)
    }

    /// A book in this user's library. Device routes use this, so a device can
    /// reach only what its user has added.
    pub async fn library_book(&self, id: Uuid) -> Result<Option<BookRow>, sqlx::Error> {
        let mut conn = self.db.read.acquire().await?;
        fetch_library_book(&mut conn, self.user_id, &id.to_string()).await
    }

    /// A book this user may look at: their own, or anyone's shared book, whether
    /// or not it's in their library.
    pub async fn readable_book(&self, id: Uuid) -> Result<Option<BookRow>, sqlx::Error> {
        let id = id.to_string();
        sqlx::query_as!(
            BookRow,
            r#"SELECT b.id AS "id!", b.owner_id, b.visibility, b.title, b.authors_json, b.series,
                      b.series_index, b.description, b.publisher, b.language, b.published_on, b.isbn,
                      b.cover_id, b.original_size, b.kepub_state, b.kepub_size,
                      e.first_seq AS "first_seq?: i64",
                      COALESCE(e.change_seq, 0) AS "change_seq!: i64",
                      b.created_at, b.updated_at
               FROM books b
               LEFT JOIN library_entries e ON e.book_id = b.id AND e.user_id = ?1
               WHERE b.id = ?2 AND (b.owner_id = ?1 OR b.visibility = 'shared')"#,
            self.user_id,
            id
        )
        .fetch_optional(&self.db.read)
        .await
    }

    /// This user's library, newest first, with how far they are through
    /// each book: the reader's place or Nickel's, whichever is newer.
    pub async fn library_cards(&self) -> Result<Vec<BookCard>, sqlx::Error> {
        sqlx::query_as!(
            BookCard,
            r#"SELECT b.id AS "id!", b.owner_id, b.visibility, b.title, b.authors_json, b.cover_id,
                      b.kepub_state, u.username AS shared_by, 1 AS "in_library!: bool",
                      e.on_device AS "on_device!: bool",
                      CASE WHEN p.read_at IS NOT NULL AND p.read_at >= COALESCE(s.last_modified, 0)
                                THEN p.status
                           WHEN s.status = 'Finished' THEN 'finished'
                           WHEN s.status = 'Reading' THEN 'reading'
                           ELSE 'unread' END AS "read_status!: String",
                      CASE WHEN p.read_at IS NOT NULL AND p.read_at >= COALESCE(s.last_modified, 0)
                                THEN p.percent
                           ELSE s.percent END AS "read_percent: f64"
               FROM library_entries e
               JOIN books b ON b.id = e.book_id
               JOIN users u ON u.id = b.owner_id
               LEFT JOIN reader_places p ON p.user_id = e.user_id AND p.book_id = e.book_id
               LEFT JOIN kobo_reading_states s ON s.user_id = e.user_id AND s.book_id = e.book_id
               WHERE e.user_id = ?1
               ORDER BY e.added_at DESC, b.title"#,
            self.user_id
        )
        .fetch_all(&self.db.read)
        .await
    }

    /// Every shared book, whether or not it's in this user's library, newest first.
    pub async fn shared_cards(&self) -> Result<Vec<BookCard>, sqlx::Error> {
        sqlx::query_as!(
            BookCard,
            r#"SELECT b.id AS "id!", b.owner_id, b.visibility, b.title, b.authors_json, b.cover_id,
                      b.kepub_state, u.username AS shared_by,
                      e.user_id IS NOT NULL AS "in_library!: bool",
                      COALESCE(e.on_device, 0) AS "on_device!: bool",
                      CASE WHEN p.read_at IS NOT NULL AND p.read_at >= COALESCE(s.last_modified, 0)
                                THEN p.status
                           WHEN s.status = 'Finished' THEN 'finished'
                           WHEN s.status = 'Reading' THEN 'reading'
                           ELSE 'unread' END AS "read_status!: String",
                      CASE WHEN p.read_at IS NOT NULL AND p.read_at >= COALESCE(s.last_modified, 0)
                                THEN p.percent
                           ELSE s.percent END AS "read_percent: f64"
               FROM books b JOIN users u ON u.id = b.owner_id
               LEFT JOIN library_entries e ON e.book_id = b.id AND e.user_id = ?1
               LEFT JOIN reader_places p ON p.user_id = e.user_id AND p.book_id = e.book_id
               LEFT JOIN kobo_reading_states s ON s.user_id = e.user_id AND s.book_id = e.book_id
               WHERE b.visibility = 'shared'
               ORDER BY b.created_at DESC, b.title"#,
            self.user_id
        )
        .fetch_all(&self.db.read)
        .await
    }

    /// How many other people have this book in their library, to warn whoever is
    /// about to take it away from them.
    pub async fn other_holders(&self, book: Uuid) -> Result<i64, sqlx::Error> {
        let id = book.to_string();
        let row = sqlx::query!(
            r#"SELECT COUNT(*) AS "count!: i64" FROM library_entries
               WHERE book_id = ?1 AND user_id <> ?2"#,
            id,
            self.user_id
        )
        .fetch_one(&self.db.read)
        .await?;
        Ok(row.count)
    }

    /// This user's storage limit.
    pub async fn quota_bytes(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!("SELECT quota_bytes FROM users WHERE id = ?1", self.user_id)
            .fetch_one(&self.db.read)
            .await?;
        Ok(row.quota_bytes)
    }

    /// The library book that a cover ID currently belongs to.
    pub async fn book_id_by_cover(&self, cover: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
        let cover = cover.to_string();
        let row = sqlx::query!(
            r#"SELECT b.id AS "id!" FROM books b
               JOIN library_entries e ON e.book_id = b.id AND e.user_id = ?1
               WHERE b.cover_id = ?2"#,
            self.user_id,
            cover
        )
        .fetch_optional(&self.db.read)
        .await?;
        Ok(row.and_then(|r| Uuid::parse_str(&r.id).ok()))
    }

    /// How far this user is through a book in their library, and whether
    /// it's on their readers. `None` if it isn't in their library.
    pub async fn reading(&self, book: Uuid) -> Result<Option<(Reading, bool)>, sqlx::Error> {
        let id = book.to_string();
        let row = sqlx::query!(
            r#"SELECT e.on_device AS "on_device!: bool",
                      CASE WHEN p.read_at IS NOT NULL AND p.read_at >= COALESCE(s.last_modified, 0)
                                THEN p.status
                           WHEN s.status = 'Finished' THEN 'finished'
                           WHEN s.status = 'Reading' THEN 'reading'
                           ELSE 'unread' END AS "read_status!: String",
                      CASE WHEN p.read_at IS NOT NULL AND p.read_at >= COALESCE(s.last_modified, 0)
                                THEN p.percent
                           ELSE s.percent END AS "read_percent: f64"
               FROM library_entries e
               LEFT JOIN reader_places p ON p.user_id = e.user_id AND p.book_id = e.book_id
               LEFT JOIN kobo_reading_states s ON s.user_id = e.user_id AND s.book_id = e.book_id
               WHERE e.user_id = ?1 AND e.book_id = ?2"#,
            self.user_id,
            id
        )
        .fetch_optional(&self.db.read)
        .await?;
        Ok(row.map(|row| {
            (
                Reading::from_row(&row.read_status, row.read_percent),
                row.on_device,
            )
        }))
    }

    /// Takes a book in this user's library off their readers, or puts it
    /// back. It stays in the library either way, with its places. Their
    /// readers remove it, or get it again, at their next sync.
    pub async fn set_on_device(&self, book: Uuid, on: bool) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        let entry = sqlx::query!(
            r#"SELECT on_device AS "on_device!: bool" FROM library_entries
               WHERE user_id = ?1 AND book_id = ?2"#,
            self.user_id,
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(BookError::NotFound)?;
        if entry.on_device != on {
            let seq = next_seq(&mut tx, self.user_id).await?;
            sqlx::query!(
                "UPDATE library_entries SET on_device = ?1, change_seq = ?2
                 WHERE user_id = ?3 AND book_id = ?4",
                on,
                seq,
                self.user_id,
                id
            )
            .execute(&mut *tx)
            .await?;
            if on {
                // Places go out again, after the book, so a reader getting
                // it back opens it where it was left. Each its own number,
                // as changes are paged by them.
                let seq = next_seq(&mut tx, self.user_id).await?;
                sqlx::query!(
                    "UPDATE reader_places SET change_seq = ?1 WHERE user_id = ?2 AND book_id = ?3",
                    seq,
                    self.user_id,
                    id
                )
                .execute(&mut *tx)
                .await?;
                let seq = next_seq(&mut tx, self.user_id).await?;
                sqlx::query!(
                    "UPDATE kobo_reading_states SET change_seq = ?1
                     WHERE user_id = ?2 AND book_id = ?3",
                    seq,
                    self.user_id,
                    id
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn reading_state(&self, book: Uuid) -> Result<Option<ReadingStateRow>, sqlx::Error> {
        let mut conn = self.db.read.acquire().await?;
        fetch_state(&mut conn, self.user_id, &book.to_string()).await
    }

    /// Reads one page of changes after `pos`, using `base` to decide what the
    /// device already had when this paginated run started.
    ///
    /// A tombstone is sent whenever `first_seq <= pos`, meaning the device could
    /// have received the book. That occasionally removes a book the device never
    /// got, which is harmless. The stricter rule, "only if the device certainly
    /// has it", loses deletes for a book delivered and then edited mid-run.
    pub async fn sync_page(
        &self,
        base: i64,
        pos: i64,
        limit: i64,
    ) -> Result<SyncPage, sqlx::Error> {
        let mut tx = self.db.read.begin().await?;

        let high_water = sqlx::query!("SELECT change_seq FROM users WHERE id = ?1", self.user_id)
            .fetch_one(&mut *tx)
            .await?
            .change_seq;

        let fetch = limit + 1;
        let rows = sqlx::query!(
            r#"SELECT kind AS "kind!: String", id AS "id!: String", seq AS "seq!: i64" FROM (
                   SELECT 'book' AS kind, book_id AS id, change_seq AS seq FROM library_entries
                    WHERE user_id = ?1 AND change_seq > ?3 AND first_seq IS NOT NULL
                      AND on_device = 1
                   UNION ALL
                   SELECT 'tombstone', book_id, change_seq FROM book_tombstones
                    WHERE user_id = ?1 AND change_seq > ?3 AND first_seq <= ?3
                   UNION ALL
                   -- Taken off the user's devices, but still in their library.
                   SELECT 'tombstone', book_id, change_seq FROM library_entries
                    WHERE user_id = ?1 AND change_seq > ?3 AND first_seq <= ?3
                      AND on_device = 0
                   UNION ALL
                   SELECT 'state', s.book_id, s.change_seq
                     FROM kobo_reading_states s
                     JOIN library_entries e ON e.user_id = s.user_id AND e.book_id = s.book_id
                    WHERE s.user_id = ?1 AND s.change_seq > ?3
                      AND e.first_seq IS NOT NULL AND e.on_device = 1
                      AND (e.first_seq <= ?2 OR e.change_seq < s.change_seq)
               ) ORDER BY seq LIMIT ?4"#,
            self.user_id,
            base,
            pos,
            fetch
        )
        .fetch_all(&mut *tx)
        .await?;

        let has_more = rows.len() as i64 > limit;
        let mut page = Vec::with_capacity(rows.len());
        for row in rows.into_iter().take(limit as usize) {
            let row = SyncRow {
                kind: match row.kind.as_str() {
                    "book" => SyncRowKind::Book,
                    "tombstone" => SyncRowKind::Tombstone,
                    _ => SyncRowKind::ReadingState,
                },
                id: row.id,
                seq: row.seq,
            };
            page.push(self.resolve_row(&mut tx, row).await?);
        }
        tx.rollback().await?;

        Ok(SyncPage {
            rows: page.into_iter().flatten().collect(),
            has_more,
            high_water,
        })
    }

    async fn resolve_row(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        row: SyncRow,
    ) -> Result<Option<SyncPageRow>, sqlx::Error> {
        Ok(match row.kind {
            SyncRowKind::Tombstone => Some(SyncPageRow::Tombstone {
                id: row.id,
                seq: row.seq,
            }),
            SyncRowKind::Book => {
                let book = fetch_library_book(&mut *tx, self.user_id, &row.id).await?;
                let state = fetch_state(&mut *tx, self.user_id, &row.id).await?;
                book.map(|book| SyncPageRow::Book {
                    book,
                    state,
                    seq: row.seq,
                })
            }
            SyncRowKind::ReadingState => {
                let book = fetch_library_book(&mut *tx, self.user_id, &row.id).await?;
                let state = fetch_state(&mut *tx, self.user_id, &row.id).await?;
                book.zip(state)
                    .map(|(book, state)| SyncPageRow::ReadingState {
                        book,
                        state,
                        seq: row.seq,
                    })
            }
        })
    }

    /// How much of their quota the user has used.
    pub async fn used_bytes(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT COALESCE(SUM(original_size + COALESCE(kepub_size, 0)), 0) AS "used!: i64"
               FROM books WHERE owner_id = ?1"#,
            self.user_id
        )
        .fetch_one(&self.db.read)
        .await?;
        Ok(row.used)
    }

    /// Whether an upload is a file that's already here for this user: their own
    /// copy first, then one that's shared.
    pub async fn find_duplicate(&self, sha256: &str) -> Result<Option<Duplicate>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", owner_id FROM books
               WHERE original_sha256 = ?1 AND (owner_id = ?2 OR visibility = 'shared')
               ORDER BY owner_id = ?2 DESC LIMIT 1"#,
            sha256,
            self.user_id
        )
        .fetch_optional(&self.db.read)
        .await?;
        Ok(rows.and_then(|row| {
            let id = Uuid::parse_str(&row.id).ok()?;
            Some(if row.owner_id == self.user_id {
                Duplicate::Own(id)
            } else {
                Duplicate::Shared(id)
            })
        }))
    }

    /// A book this user may change, for work that has to happen before the change
    /// itself. The change is still checked again when it's made.
    pub async fn editable_book(&self, book: Uuid) -> Result<BookRow, BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        self.require_edit(&mut tx, &id).await?;
        tx.commit().await?;
        self.readable_book(book).await?.ok_or(BookError::NotFound)
    }

    /// Creates a book and puts it in the uploader's library. If `visible` is
    /// true, it joins the sync stream at once; otherwise it stays hidden until
    /// [`Self::mark_kepub`] is called.
    pub async fn create_book(&self, new: &NewBook, visible: bool) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;

        let id = new.id.to_string();
        let authors = serde_json::to_string(&new.authors).unwrap_or_else(|_| "[]".to_owned());
        let cover = new.cover_id.map(|c| c.to_string());
        let visibility = new.visibility.as_str();
        let kepub_state = match (new.kepub_size, visible) {
            (Some(_), _) => "ready",
            (None, true) => "failed",
            (None, false) => "pending",
        };
        let at = now();

        // Checked here, in the one write transaction, so that two uploads at once
        // can't both slip under the limit.
        let usage = sqlx::query!(
            r#"SELECT u.quota_bytes,
                      (SELECT COALESCE(SUM(b.original_size + COALESCE(b.kepub_size, 0)), 0)
                       FROM books b WHERE b.owner_id = u.id) AS "used!: i64"
               FROM users u WHERE u.id = ?1"#,
            self.user_id
        )
        .fetch_one(&mut *tx)
        .await?;
        if usage.used.saturating_add(new.original_size) > usage.quota_bytes {
            return Err(BookError::QuotaExceeded);
        }

        sqlx::query!(
            "INSERT INTO books (
                 id, owner_id, visibility, title, authors_json, series, series_index,
                 description, publisher, language, published_on, isbn, cover_id,
                 original_sha256, original_size, kepub_state, kepub_size,
                 created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                       ?17, ?18, ?18)",
            id,
            self.user_id,
            visibility,
            new.title,
            authors,
            new.details.series,
            new.details.series_index,
            new.details.description,
            new.details.publisher,
            new.details.language,
            new.details.published_on,
            new.details.isbn,
            cover,
            new.original_sha256,
            new.original_size,
            kepub_state,
            new.kepub_size,
            at
        )
        .execute(&mut *tx)
        .await
        .map_err(already_shared)?;

        insert_entry(&mut tx, self.user_id, &id, visible).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records a finished conversion attempt and makes the book sync-visible to
    /// everyone holding it. `kepub_size` is `None` when conversion failed and
    /// EPUB should be served. Only the uploader's conversion job calls this.
    pub async fn mark_kepub(&self, book: Uuid, kepub_size: Option<i64>) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        let state = if kepub_size.is_some() {
            "ready"
        } else {
            "failed"
        };
        let at = now();
        let done = sqlx::query!(
            "UPDATE books SET kepub_state = ?1, kepub_size = ?2, updated_at = ?3
             WHERE owner_id = ?4 AND id = ?5",
            state,
            kepub_size,
            at,
            self.user_id,
            id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !done {
            return Err(BookError::NotFound);
        }
        notify_holders(&mut tx, &id, Reveal::Yes).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Changes a book's title for everyone holding it.
    pub async fn set_title(&self, book: Uuid, title: &str) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        self.require_edit(&mut tx, &id).await?;
        let at = now();
        sqlx::query!(
            "UPDATE books SET title = ?1, updated_at = ?2 WHERE id = ?3",
            title,
            at,
            id
        )
        .execute(&mut *tx)
        .await?;
        notify_holders(&mut tx, &id, Reveal::No).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Changes a book's metadata for everyone holding it. Only the database changes:
    /// a Kobo shows what the sync says, whatever is in the file.
    pub async fn set_metadata(&self, book: Uuid, edit: &MetadataEdit) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        self.require_edit(&mut tx, &id).await?;
        let at = now();
        let authors = serde_json::to_string(&edit.authors).unwrap_or_else(|_| "[]".to_owned());
        let old_isbn = sqlx::query_scalar!("SELECT isbn FROM books WHERE id = ?1", id)
            .fetch_one(&mut *tx)
            .await?;
        if old_isbn != edit.details.isbn {
            // A corrected ISBN is the best way to find it on Hardcover, so look again
            // for everyone, unless they chose the book themselves.
            sqlx::query!(
                "UPDATE hardcover_books
                 SET match = 'pending', due_at = ?1, rev = rev + 1, attempts = 0
                 WHERE book_id = ?2 AND chosen_slug IS NULL",
                at,
                id
            )
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query!(
            "UPDATE books SET title = ?1, authors_json = ?2, series = ?3, series_index = ?4,
                              description = ?5, publisher = ?6, language = ?7, published_on = ?8,
                              isbn = ?9, updated_at = ?10
             WHERE id = ?11",
            edit.title,
            authors,
            edit.details.series,
            edit.details.series_index,
            edit.details.description,
            edit.details.publisher,
            edit.details.language,
            edit.details.published_on,
            edit.details.isbn,
            at,
            id
        )
        .execute(&mut *tx)
        .await?;
        notify_holders(&mut tx, &id, Reveal::No).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Points a book at a new cover file, for everyone holding it.
    pub async fn set_cover(&self, book: Uuid, cover: Uuid) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        self.require_edit(&mut tx, &id).await?;
        let cover = cover.to_string();
        let at = now();
        sqlx::query!(
            "UPDATE books SET cover_id = ?1, updated_at = ?2 WHERE id = ?3",
            cover,
            at,
            id
        )
        .execute(&mut *tx)
        .await?;
        notify_holders(&mut tx, &id, Reveal::No).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Makes a book private or shared. Going private takes it out of everyone
    /// else's library, including their reading states.
    pub async fn set_visibility(
        &self,
        book: Uuid,
        visibility: Visibility,
    ) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        self.require_edit(&mut tx, &id).await?;

        let value = visibility.as_str();
        let at = now();
        sqlx::query!(
            "UPDATE books SET visibility = ?1, updated_at = ?2 WHERE id = ?3",
            value,
            at,
            id
        )
        .execute(&mut *tx)
        .await
        .map_err(already_shared)?;

        if visibility == Visibility::Private {
            let others = sqlx::query!(
                r#"SELECT e.user_id AS "user_id!", e.first_seq FROM library_entries e
                   JOIN books b ON b.id = e.book_id
                   WHERE e.book_id = ?1 AND e.user_id <> b.owner_id"#,
                id
            )
            .fetch_all(&mut *tx)
            .await?;
            for other in others {
                remove_entry(&mut tx, other.user_id, &id, other.first_seq).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Adds a shared book to this user's library. Doing so twice changes nothing.
    pub async fn add_to_library(&self, book: Uuid) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        let row = sqlx::query!(
            "SELECT kepub_state FROM books
             WHERE id = ?1 AND (visibility = 'shared' OR owner_id = ?2)",
            id,
            self.user_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(BookError::NotFound)?;

        let held = sqlx::query!(
            r#"SELECT 1 AS "found!: i64" FROM library_entries WHERE user_id = ?1 AND book_id = ?2"#,
            self.user_id,
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if !held {
            insert_entry(&mut tx, self.user_id, &id, row.kepub_state != "pending").await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Takes someone else's shared book out of this user's library. Uploaders
    /// delete their own books instead.
    pub async fn remove_from_library(&self, book: Uuid) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        let row = sqlx::query!(
            "SELECT owner_id FROM books
             WHERE id = ?1 AND (visibility = 'shared' OR owner_id = ?2)",
            id,
            self.user_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(BookError::NotFound)?;
        if row.owner_id == self.user_id {
            return Err(BookError::Forbidden);
        }

        let entry = sqlx::query!(
            "SELECT first_seq FROM library_entries WHERE user_id = ?1 AND book_id = ?2",
            self.user_id,
            id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(entry) = entry {
            remove_entry(&mut tx, self.user_id, &id, entry.first_seq).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Deletes a book for everyone. The caller removes the files after this
    /// commits.
    pub async fn delete_book(&self, book: Uuid) -> Result<(), BookError> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();
        self.require_edit(&mut tx, &id).await?;

        let holders = sqlx::query!(
            r#"SELECT user_id AS "user_id!", first_seq FROM library_entries WHERE book_id = ?1"#,
            id
        )
        .fetch_all(&mut *tx)
        .await?;
        for holder in holders {
            remove_entry(&mut tx, holder.user_id, &id, holder.first_seq).await?;
        }
        sqlx::query!("DELETE FROM books WHERE id = ?1", id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Passes only if this user may change the book: its uploader, or an admin
    /// for a shared book. A private book of someone else's doesn't exist as far
    /// as they can tell, admins included.
    async fn require_edit(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        book: &str,
    ) -> Result<(), BookError> {
        let row = sqlx::query!(
            "SELECT b.owner_id, b.visibility, u.is_admin
             FROM books b JOIN users u ON u.id = ?1 WHERE b.id = ?2",
            self.user_id,
            book
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(BookError::NotFound)?;

        if row.owner_id == self.user_id {
            Ok(())
        } else if row.visibility != "shared" {
            Err(BookError::NotFound)
        } else if row.is_admin != 0 {
            Ok(())
        } else {
            Err(BookError::Forbidden)
        }
    }

    /// Stores a device's reading state. Returns the new `last_modified`, or
    /// `None` if the user has no such book.
    pub async fn put_reading_state(
        &self,
        book: Uuid,
        status: &str,
        percent: Option<f64>,
        state_json: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        let mut tx = self.db.write.begin().await?;
        let id = book.to_string();

        let exists = sqlx::query!(
            r#"SELECT 1 AS "found!: i64" FROM library_entries WHERE user_id = ?1 AND book_id = ?2"#,
            self.user_id,
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if !exists {
            return Ok(None);
        }

        let seq = next_seq(&mut tx, self.user_id).await?;
        let at = now();
        sqlx::query!(
            "INSERT INTO kobo_reading_states
                 (user_id, book_id, status, percent, state_json, last_modified, change_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (user_id, book_id) DO UPDATE SET
                 status = excluded.status, percent = excluded.percent, state_json = excluded.state_json,
                 last_modified = excluded.last_modified, change_seq = excluded.change_seq",
            self.user_id,
            id,
            status,
            percent,
            state_json,
            at,
            seq
        )
        .execute(&mut *tx)
        .await?;

        let want = crate::server::hardcover::rules::status_from_kobo(status);
        tell_hardcover(&mut tx, self.user_id, &id, want, percent, at).await?;

        tx.commit().await?;
        Ok(Some(at))
    }
}

/// Queues what a device said about a book for Hardcover, if the user is
/// connected. Progress waits a minute, so a few page turns go as one update;
/// finishing goes straight away. `want` is Hardcover's status number.
pub(super) async fn tell_hardcover(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: i64,
    id: &str,
    want: i64,
    percent: Option<f64>,
    at: i64,
) -> Result<(), sqlx::Error> {
    let due = if want == crate::server::hardcover::rules::READ {
        at
    } else {
        at + HARDCOVER_PROGRESS_DELAY
    };
    sqlx::query!(
        "INSERT INTO hardcover_books (user_id, book_id, want_status, want_percent, due_at, rev)
             SELECT ?1, ?2, ?3, ?4, ?5, 1
             WHERE EXISTS (SELECT 1 FROM hardcover_accounts WHERE user_id = ?1)
             ON CONFLICT (user_id, book_id) DO UPDATE SET
                 want_status = MAX(want_status, excluded.want_status),
                 want_percent = excluded.want_percent,
                 due_at = MIN(COALESCE(due_at, excluded.due_at), excluded.due_at),
                 rev = rev + 1",
        user_id,
        id,
        want,
        percent,
        due
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Allocates the next value of the user's sync sequence. Must run inside the
/// write transaction that uses it, so sequence order equals commit order.
pub(super) async fn next_seq(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: i64,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(
        "UPDATE users SET change_seq = change_seq + 1 WHERE id = ?1 RETURNING change_seq",
        user_id
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.change_seq)
}

/// Whether a change also makes a book sync-visible for those holding it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reveal {
    Yes,
    No,
}

/// Queues a book's change for everyone holding it, each in their own sequence.
pub(super) async fn notify_holders(
    tx: &mut Transaction<'_, Sqlite>,
    book: &str,
    reveal: Reveal,
) -> Result<(), sqlx::Error> {
    let holders = sqlx::query!(
        r#"SELECT user_id AS "user_id!" FROM library_entries WHERE book_id = ?1"#,
        book
    )
    .fetch_all(&mut **tx)
    .await?;
    let reveal = reveal == Reveal::Yes;
    for holder in holders {
        let seq = next_seq(tx, holder.user_id).await?;
        sqlx::query!(
            "UPDATE library_entries
             SET change_seq = ?1, first_seq = CASE WHEN ?2 THEN COALESCE(first_seq, ?1) ELSE first_seq END
             WHERE user_id = ?3 AND book_id = ?4",
            seq,
            reveal,
            holder.user_id,
            book
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_entry(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: i64,
    book: &str,
    visible: bool,
) -> Result<(), sqlx::Error> {
    let first_seq = if visible {
        Some(next_seq(tx, user_id).await?)
    } else {
        None
    };
    let change_seq = first_seq.unwrap_or(0);
    let at = now();
    sqlx::query!(
        "INSERT INTO library_entries (user_id, book_id, first_seq, change_seq, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        user_id,
        book,
        first_seq,
        change_seq,
        at
    )
    .execute(&mut **tx)
    .await?;
    // "Want to read" on Hardcover, if connected.
    sqlx::query!(
        "INSERT INTO hardcover_books (user_id, book_id, want_status, due_at, rev)
         SELECT ?1, ?2, 1, ?3, 1
         WHERE EXISTS (SELECT 1 FROM hardcover_accounts WHERE user_id = ?1)
         ON CONFLICT (user_id, book_id) DO NOTHING",
        user_id,
        book,
        at
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Takes a book out of a user's library, leaving a tombstone if it was ever
/// sync-visible to them. On a repeat removal the tombstone keeps the earliest
/// `first_seq`: a device that slept through add, remove, re-add and remove still
/// holds the book from the first add, and must still be told.
async fn remove_entry(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: i64,
    book: &str,
    first_seq: Option<i64>,
) -> Result<(), sqlx::Error> {
    if let Some(first_seq) = first_seq {
        let seq = next_seq(tx, user_id).await?;
        let at = now();
        sqlx::query!(
            "INSERT INTO book_tombstones (user_id, book_id, first_seq, change_seq, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (user_id, book_id) DO UPDATE SET
                 first_seq = MIN(first_seq, excluded.first_seq),
                 change_seq = excluded.change_seq,
                 deleted_at = excluded.deleted_at",
            user_id,
            book,
            first_seq,
            seq,
            at
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query!(
        "DELETE FROM library_entries WHERE user_id = ?1 AND book_id = ?2",
        user_id,
        book
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Turns a clash on `books_shared_file` into [`BookError::AlreadyShared`]. SQLite
/// names the columns of the constraint it hit, and only that index is on the hash
/// alone.
fn already_shared(error: sqlx::Error) -> BookError {
    if let sqlx::Error::Database(db) = &error
        && db.message().contains("books.original_sha256")
    {
        // The index on the hash alone is the one for shared books.
        return if db.message().ends_with(": books.original_sha256") {
            BookError::AlreadyShared
        } else {
            BookError::AlreadyYours
        };
    }
    BookError::Db(error)
}

async fn fetch_library_book(
    conn: &mut sqlx::SqliteConnection,
    user_id: i64,
    id: &str,
) -> Result<Option<BookRow>, sqlx::Error> {
    sqlx::query_as!(
        BookRow,
        r#"SELECT b.id AS "id!", b.owner_id, b.visibility, b.title, b.authors_json, b.series,
                  b.series_index, b.description, b.publisher, b.language, b.published_on, b.isbn,
                  b.cover_id, b.original_size, b.kepub_state, b.kepub_size,
                  e.first_seq, e.change_seq, b.created_at, b.updated_at
           FROM library_entries e JOIN books b ON b.id = e.book_id
           WHERE e.user_id = ?1 AND e.book_id = ?2"#,
        user_id,
        id
    )
    .fetch_optional(conn)
    .await
}

async fn fetch_state(
    conn: &mut sqlx::SqliteConnection,
    user_id: i64,
    id: &str,
) -> Result<Option<ReadingStateRow>, sqlx::Error> {
    sqlx::query_as!(
        ReadingStateRow,
        "SELECT state_json, last_modified FROM kobo_reading_states WHERE user_id = ?1 AND book_id = ?2",
        user_id,
        id
    )
    .fetch_optional(conn)
    .await
}
