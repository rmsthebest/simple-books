//! Hardcover connections and the queue of what to tell Hardcover. The worker acts
//! for every user, so most of this is on `Db`; what a page shows a user is on
//! `UserScope`, so a page can only ever see its own user's connection.

use serde::Deserialize;
use uuid::Uuid;

use super::{Db, scope::UserScope};

/// A connected account, tokens included. Never logged.
#[derive(Clone)]
pub struct Account {
    pub user_id: i64,
    pub hardcover_id: i64,
    pub access_token: String,
    pub access_expires: i64,
    pub refresh_token: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("user_id", &self.user_id)
            .field("hardcover_id", &self.hardcover_id)
            .finish_non_exhaustive()
    }
}

/// A sign-in waiting for its code to be entered.
#[derive(Clone)]
pub struct PendingSignIn {
    pub user_id: i64,
    pub device_code: String,
    pub poll_seconds: i64,
    pub expires_at: i64,
}

impl std::fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSignIn")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// What the Kobo page shows about the user's connection.
#[derive(Debug, Clone, Default)]
pub struct Overview {
    /// Set when connected.
    pub username: Option<String>,
    pub broken: bool,
    /// A code waiting to be entered, and where.
    pub code: Option<(String, String)>,
    /// How the last sign-in ended, if it didn't connect.
    pub outcome: Option<String>,
    /// Books that couldn't be found on Hardcover.
    pub missing: Vec<(Uuid, String)>,
}

/// A book with something to send.
#[derive(Debug, Clone)]
pub struct Due {
    pub user_id: i64,
    pub book_id: String,
    pub rev: i64,
    pub attempts: i64,
    pub title: String,
    pub authors_json: String,
    pub isbn: Option<String>,
    pub matched: bool,
    pub chosen_slug: Option<String>,
    pub hc_book_id: Option<i64>,
    pub hc_edition_id: Option<i64>,
    pub hc_pages: Option<i64>,
    pub want_status: i64,
    pub want_percent: Option<f64>,
    pub sent_pages: Option<i64>,
}

impl Due {
    pub fn authors(&self) -> Vec<String> {
        serde_json::from_str(&self.authors_json).unwrap_or_default()
    }
}

/// A book found on Hardcover.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Found {
    pub book_id: i64,
    pub edition_id: Option<i64>,
    pub slug: Option<String>,
    pub pages: Option<i64>,
}

/// How a book stands on Hardcover, for its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookLink {
    Looking,
    Found { slug: Option<String> },
    Missing,
}

impl Db {
    #[expect(clippy::too_many_arguments, reason = "one row's columns")]
    pub async fn hardcover_save_sign_in(
        &self,
        user_id: i64,
        device_code: &str,
        user_code: &str,
        verification_url: &str,
        poll_seconds: i64,
        expires_at: i64,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        let next = now + poll_seconds;
        sqlx::query!(
            "INSERT INTO hardcover_sign_ins
                 (user_id, device_code, user_code, verification_url, poll_seconds, next_poll_at,
                  expires_at, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT (user_id) DO UPDATE SET
                 device_code = excluded.device_code, user_code = excluded.user_code,
                 verification_url = excluded.verification_url,
                 poll_seconds = excluded.poll_seconds, next_poll_at = excluded.next_poll_at,
                 expires_at = excluded.expires_at, outcome = NULL",
            user_id,
            device_code,
            user_code,
            verification_url,
            poll_seconds,
            next,
            expires_at
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn hardcover_due_sign_ins(
        &self,
        now: i64,
    ) -> Result<Vec<PendingSignIn>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT user_id AS "user_id!", device_code, poll_seconds, expires_at
               FROM hardcover_sign_ins WHERE outcome IS NULL AND next_poll_at <= ?1"#,
            now
        )
        .fetch_all(&self.read)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| PendingSignIn {
                user_id: row.user_id,
                device_code: row.device_code,
                poll_seconds: row.poll_seconds,
                expires_at: row.expires_at,
            })
            .collect())
    }

    /// Checks again after `poll_seconds`, which may have grown if Hardcover asked.
    pub async fn hardcover_poll_later(
        &self,
        user_id: i64,
        device_code: &str,
        poll_seconds: i64,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        let next = now + poll_seconds;
        sqlx::query!(
            "UPDATE hardcover_sign_ins SET poll_seconds = ?1, next_poll_at = ?2
             WHERE user_id = ?3 AND device_code = ?4",
            poll_seconds,
            next,
            user_id,
            device_code
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn hardcover_sign_in_ended(
        &self,
        user_id: i64,
        device_code: &str,
        outcome: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE hardcover_sign_ins SET outcome = ?1 WHERE user_id = ?2 AND device_code = ?3",
            outcome,
            user_id,
            device_code
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Stores a new connection, and queues every book in the library: books being
    /// read or finished go as such, the rest as "want to read". Returns false if
    /// the sign-in was cancelled meanwhile, and stores nothing.
    #[expect(clippy::too_many_arguments, reason = "one row's columns")]
    pub async fn hardcover_connect(
        &self,
        user_id: i64,
        device_code: &str,
        hardcover_id: i64,
        username: &str,
        access_token: &str,
        access_expires: i64,
        refresh_token: &str,
        now: i64,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.write.begin().await?;
        let removed = sqlx::query!(
            "DELETE FROM hardcover_sign_ins WHERE user_id = ?1 AND device_code = ?2",
            user_id,
            device_code
        )
        .execute(&mut *tx)
        .await?;
        if removed.rows_affected() == 0 {
            return Ok(false);
        }
        sqlx::query!(
            "INSERT INTO hardcover_accounts
                 (user_id, hardcover_id, username, access_token, access_expires, refresh_token,
                  broken_at, connected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)
             ON CONFLICT (user_id) DO UPDATE SET
                 hardcover_id = excluded.hardcover_id, username = excluded.username,
                 access_token = excluded.access_token, access_expires = excluded.access_expires,
                 refresh_token = excluded.refresh_token, broken_at = NULL,
                 connected_at = excluded.connected_at",
            user_id,
            hardcover_id,
            username,
            access_token,
            access_expires,
            refresh_token,
            now
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO hardcover_books (user_id, book_id, want_status, want_percent, due_at, rev)
             SELECT e.user_id, e.book_id,
                    CASE s.status WHEN 'Reading' THEN 2 WHEN 'Finished' THEN 3 ELSE 1 END,
                    s.percent, ?2, 1
             FROM library_entries e
             LEFT JOIN kobo_reading_states s ON s.user_id = e.user_id AND s.book_id = e.book_id
             WHERE e.user_id = ?1
             ON CONFLICT (user_id, book_id) DO UPDATE SET
                 due_at = excluded.due_at, rev = hardcover_books.rev + 1, attempts = 0",
            user_id,
            now
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn hardcover_account(&self, user_id: i64) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT user_id AS "user_id!", hardcover_id, access_token, access_expires, refresh_token
               FROM hardcover_accounts WHERE user_id = ?1 AND broken_at IS NULL"#,
            user_id
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.map(|row| Account {
            user_id: row.user_id,
            hardcover_id: row.hardcover_id,
            access_token: row.access_token,
            access_expires: row.access_expires,
            refresh_token: row.refresh_token,
        }))
    }

    /// Keeps the newest tokens. Only replaces the refresh token that was spent, so a
    /// reconnect in the meantime isn't overwritten.
    pub async fn hardcover_save_tokens(
        &self,
        user_id: i64,
        spent_refresh_token: &str,
        access_token: &str,
        access_expires: i64,
        refresh_token: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE hardcover_accounts
             SET access_token = ?1, access_expires = ?2, refresh_token = ?3
             WHERE user_id = ?4 AND refresh_token = ?5",
            access_token,
            access_expires,
            refresh_token,
            user_id,
            spent_refresh_token
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn hardcover_mark_broken(
        &self,
        user_id: i64,
        refresh_token: &str,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE hardcover_accounts SET broken_at = ?1
             WHERE user_id = ?2 AND refresh_token = ?3 AND broken_at IS NULL",
            now,
            user_id,
            refresh_token
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Forgets the connection and everything queued for it. Returns the refresh
    /// token, so it can be revoked on Hardcover too.
    pub async fn hardcover_disconnect(&self, user_id: i64) -> Result<Option<String>, sqlx::Error> {
        let mut tx = self.write.begin().await?;
        let token = sqlx::query_scalar!(
            "DELETE FROM hardcover_accounts WHERE user_id = ?1 RETURNING refresh_token",
            user_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query!("DELETE FROM hardcover_sign_ins WHERE user_id = ?1", user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query!("DELETE FROM hardcover_books WHERE user_id = ?1", user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(token)
    }

    /// The books waiting to be sent, oldest first, for working connections only.
    pub async fn hardcover_due(&self, now: i64, limit: i64) -> Result<Vec<Due>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT h.user_id AS "user_id!", h.book_id AS "book_id!", h.rev, h.attempts,
                      b.title, b.authors_json, b.isbn, h.match AS "matched", h.chosen_slug,
                      h.hc_book_id, h.hc_edition_id, h.hc_pages, h.want_status, h.want_percent,
                      h.sent_pages
               FROM hardcover_books h
               JOIN books b ON b.id = h.book_id
               JOIN hardcover_accounts a ON a.user_id = h.user_id AND a.broken_at IS NULL
               WHERE h.due_at IS NOT NULL AND h.due_at <= ?1
               ORDER BY h.due_at, h.user_id, h.book_id
               LIMIT ?2"#,
            now,
            limit
        )
        .fetch_all(&self.read)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| Due {
                user_id: row.user_id,
                book_id: row.book_id,
                rev: row.rev,
                attempts: row.attempts,
                title: row.title,
                authors_json: row.authors_json,
                isbn: row.isbn,
                matched: row.matched == "found",
                chosen_slug: row.chosen_slug,
                hc_book_id: row.hc_book_id,
                hc_edition_id: row.hc_edition_id,
                hc_pages: row.hc_pages,
                want_status: row.want_status,
                want_percent: row.want_percent,
                sent_pages: row.sent_pages,
            })
            .collect())
    }

    /// Records where a book is on Hardcover. A book that couldn't be found waits
    /// for the user to give a link, so nothing more is sent for it.
    pub async fn hardcover_matched(
        &self,
        due: &Due,
        found: Option<&Found>,
    ) -> Result<(), sqlx::Error> {
        let (matched, book, edition, slug, pages) = match found {
            Some(found) => (
                "found",
                Some(found.book_id),
                found.edition_id,
                found.slug.clone(),
                found.pages,
            ),
            None => ("missing", None, None, None, None),
        };
        sqlx::query!(
            "UPDATE hardcover_books
             SET match = ?1, hc_book_id = ?2, hc_edition_id = ?3, hc_slug = ?4, hc_pages = ?5,
                 due_at = CASE WHEN ?1 = 'missing' AND rev = ?6 THEN NULL ELSE due_at END
             WHERE user_id = ?7 AND book_id = ?8",
            matched,
            book,
            edition,
            slug,
            pages,
            due.rev,
            due.user_id,
            due.book_id
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Records what Hardcover now says. Nothing more is due unless something changed
    /// while this was being sent.
    pub async fn hardcover_sent(
        &self,
        due: &Due,
        status: i64,
        pages: Option<i64>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE hardcover_books
             SET sent_status = ?1, sent_pages = COALESCE(?2, sent_pages), attempts = 0,
                 due_at = CASE WHEN rev = ?3 THEN NULL ELSE due_at END
             WHERE user_id = ?4 AND book_id = ?5",
            status,
            pages,
            due.rev,
            due.user_id,
            due.book_id
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Tries again at `at`, or gives up if `at` is `None`.
    pub async fn hardcover_retry(&self, due: &Due, at: Option<i64>) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE hardcover_books SET attempts = attempts + 1, due_at = ?1
             WHERE user_id = ?2 AND book_id = ?3 AND rev = ?4",
            at,
            due.user_id,
            due.book_id,
            due.rev
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Holds back everything for one user, when Hardcover asks to slow down.
    pub async fn hardcover_hold_user(&self, user_id: i64, until: i64) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE hardcover_books SET due_at = MAX(due_at, ?1)
             WHERE user_id = ?2 AND due_at IS NOT NULL",
            until,
            user_id
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }
}

impl UserScope {
    pub async fn hardcover_overview(&self) -> Result<Overview, sqlx::Error> {
        let user_id = self.user_id();
        let db = self.db();
        let account = sqlx::query!(
            "SELECT username, broken_at FROM hardcover_accounts WHERE user_id = ?1",
            user_id
        )
        .fetch_optional(&db.read)
        .await?;
        let sign_in = sqlx::query!(
            "SELECT user_code, verification_url, outcome, expires_at
             FROM hardcover_sign_ins WHERE user_id = ?1",
            user_id
        )
        .fetch_optional(&db.read)
        .await?;
        let missing = sqlx::query!(
            r#"SELECT h.book_id AS "book_id!", b.title FROM hardcover_books h
               JOIN books b ON b.id = h.book_id
               WHERE h.user_id = ?1 AND h.match = 'missing' ORDER BY b.title"#,
            user_id
        )
        .fetch_all(&db.read)
        .await?;

        let now = crate::server::now();
        let (code, outcome) = match sign_in {
            Some(row) if row.outcome.is_none() && row.expires_at > now => {
                (Some((row.user_code, row.verification_url)), None)
            }
            Some(row) if row.outcome.is_none() => (None, Some("expired".to_owned())),
            Some(row) => (None, row.outcome),
            None => (None, None),
        };
        Ok(Overview {
            broken: account.as_ref().is_some_and(|a| a.broken_at.is_some()),
            username: account.map(|a| a.username),
            code,
            outcome,
            missing: missing
                .into_iter()
                .filter_map(|row| Some((Uuid::parse_str(&row.book_id).ok()?, row.title)))
                .collect(),
        })
    }

    /// Drops a sign-in that hasn't finished, or the note about one that failed.
    pub async fn hardcover_cancel_sign_in(&self) -> Result<(), sqlx::Error> {
        let user_id = self.user_id();
        sqlx::query!("DELETE FROM hardcover_sign_ins WHERE user_id = ?1", user_id)
            .execute(&self.db().write)
            .await?;
        Ok(())
    }

    /// How a book in this user's library stands on Hardcover. `None` if they
    /// haven't connected, or the book isn't in their library.
    pub async fn hardcover_book(&self, book: Uuid) -> Result<Option<BookLink>, sqlx::Error> {
        let user_id = self.user_id();
        let id = book.to_string();
        let row = sqlx::query!(
            "SELECT match AS matched, hc_slug FROM hardcover_books WHERE user_id = ?1 AND book_id = ?2",
            user_id,
            id
        )
        .fetch_optional(&self.db().read)
        .await?;
        Ok(row.map(|row| match row.matched.as_str() {
            "found" => BookLink::Found { slug: row.hc_slug },
            "missing" => BookLink::Missing,
            _ => BookLink::Looking,
        }))
    }

    /// Uses the book the user linked instead of searching. Returns false if the book
    /// isn't in their library or they haven't connected.
    pub async fn hardcover_choose(&self, book: Uuid, slug: &str) -> Result<bool, sqlx::Error> {
        let user_id = self.user_id();
        let id = book.to_string();
        let now = crate::server::now();
        let changed = sqlx::query!(
            "UPDATE hardcover_books
             SET chosen_slug = ?1, match = 'pending', hc_book_id = NULL, hc_edition_id = NULL,
                 hc_slug = NULL, hc_pages = NULL, due_at = ?2, rev = rev + 1, attempts = 0
             WHERE user_id = ?3 AND book_id = ?4",
            slug,
            now,
            user_id,
            id
        )
        .execute(&self.db().write)
        .await?;
        Ok(changed.rows_affected() > 0)
    }
}
