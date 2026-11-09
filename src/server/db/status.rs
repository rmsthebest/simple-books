//! What a user is waiting on, so that a page they have open can show it when it's done.

use super::scope::UserScope;

impl UserScope {
    /// A short summary of what's still under way for this user: books in their
    /// library being got ready for the Kobo, and a Hardcover sign-in waiting for its
    /// code. Empty when there's nothing. It changes whenever any of it finishes,
    /// which is all an open page needs to know.
    pub async fn waiting_on(&self) -> Result<String, sqlx::Error> {
        let user_id = self.user_id();
        let now = crate::server::now();
        let db = self.db();
        let books = sqlx::query_scalar!(
            r#"SELECT b.id AS "id!" FROM library_entries e JOIN books b ON b.id = e.book_id
               WHERE e.user_id = ?1 AND b.kepub_state = 'pending' ORDER BY b.id"#,
            user_id
        )
        .fetch_all(&db.read)
        .await?;
        let signing_in = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM hardcover_sign_ins
                   WHERE user_id = ?1 AND outcome IS NULL AND expires_at > ?2
               ) AS "waiting!: i64""#,
            user_id,
            now
        )
        .fetch_one(&db.read)
        .await?
            != 0;

        let mut parts = Vec::new();
        if !books.is_empty() {
            parts.push(format!("books={}", books.join(",")));
        }
        if signing_in {
            parts.push("hardcover=waiting".to_owned());
        }
        Ok(parts.join(";"))
    }
}
