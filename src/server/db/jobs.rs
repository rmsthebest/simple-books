//! Background work: converting books and checking storage. These act for the
//! server rather than for a user, so they aren't on `UserScope`, and none of them
//! returns anything a user could be handed directly.

use uuid::Uuid;

use super::Db;

/// How many times a book gets converted before falling back to its EPUB.
pub const MAX_CONVERSION_ATTEMPTS: i64 = 3;

/// A book waiting for its KEPUB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conversion {
    pub book: Uuid,
    pub owner_id: i64,
}

/// What startup needs to know to check a book's files.
#[derive(Debug, Clone)]
pub struct StoredBook {
    pub id: Uuid,
    pub cover_id: Option<Uuid>,
    pub kepub_ready: bool,
}

impl Db {
    /// The oldest book still waiting to be converted.
    pub async fn next_conversion(&self) -> Result<Option<Conversion>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT id AS "id!", owner_id FROM books WHERE kepub_state = 'pending'
               ORDER BY created_at, id LIMIT 1"#
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.and_then(|row| {
            Some(Conversion {
                book: Uuid::parse_str(&row.id).ok()?,
                owner_id: row.owner_id,
            })
        }))
    }

    /// Counts an attempt and returns how many there have been.
    pub async fn count_conversion_attempt(&self, book: Uuid) -> Result<i64, sqlx::Error> {
        let id = book.to_string();
        let row = sqlx::query!(
            r#"UPDATE books SET kepub_attempts = kepub_attempts + 1 WHERE id = ?1
               RETURNING kepub_attempts AS "attempts!""#,
            id
        )
        .fetch_optional(&self.write)
        .await?;
        Ok(row.map_or(MAX_CONVERSION_ATTEMPTS, |row| row.attempts))
    }

    pub async fn record_kepubify_version(
        &self,
        book: Uuid,
        version: &str,
    ) -> Result<(), sqlx::Error> {
        let id = book.to_string();
        sqlx::query!(
            "UPDATE books SET kepubify_version = ?1 WHERE id = ?2",
            version,
            id
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Queues a book whose KEPUB has gone missing. It stays on people's Kobos, and
    /// they get the EPUB until the conversion finishes.
    pub async fn requeue_conversion(&self, book: Uuid) -> Result<(), sqlx::Error> {
        let id = book.to_string();
        sqlx::query!(
            "UPDATE books SET kepub_state = 'pending', kepub_size = NULL, kepub_attempts = 0
             WHERE id = ?1",
            id
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn stored_books(&self) -> Result<Vec<StoredBook>, sqlx::Error> {
        let rows = sqlx::query!(r#"SELECT id AS "id!", cover_id, kepub_state FROM books"#)
            .fetch_all(&self.read)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(StoredBook {
                    id: Uuid::parse_str(&row.id).ok()?,
                    cover_id: row.cover_id.and_then(|id| Uuid::parse_str(&id).ok()),
                    kepub_ready: row.kepub_state == "ready",
                })
            })
            .collect())
    }
}
