//! Pairing a simple-books reader: it asks for a code, and someone signed in
//! types it on the website. See `server::reader::pair`.

use super::Db;

/// How many readers may be waiting for their codes at once, across the
/// server. Asking needs no sign-in, so this keeps the table small.
pub const MAX_PENDING: i64 = 50;

/// What a reader's secret stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairState {
    /// Its code hasn't been entered yet.
    Pending,
    /// It's a device of this user's.
    Paired { username: String },
    /// Nothing: the code ran out, or the device was removed.
    Unknown,
}

/// What a reader's secret resolves to on the reader routes.
#[derive(Debug, Clone)]
pub struct ReaderDevice {
    pub device_id: i64,
    pub user_id: i64,
    pub force_full_sync: bool,
}

/// Why a code wasn't accepted.
#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("no reader is waiting with that code")]
    NoSuchCode,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl Db {
    /// Records a reader waiting for its code. `false` if too many already are,
    /// or the code is taken; expired ones are cleared first.
    pub async fn start_pairing(
        &self,
        secret_hash: &str,
        code: &str,
        name: &str,
        at: i64,
        expires_at: i64,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.write.begin().await?;
        sqlx::query!("DELETE FROM device_pairings WHERE expires_at <= ?1", at)
            .execute(&mut *tx)
            .await?;
        let waiting = sqlx::query!(r#"SELECT COUNT(*) AS "n!: i64" FROM device_pairings"#)
            .fetch_one(&mut *tx)
            .await?
            .n;
        if waiting >= MAX_PENDING {
            return Ok(false);
        }
        let added = sqlx::query!(
            "INSERT INTO device_pairings (secret_hash, code, name, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
            secret_hash,
            code,
            name,
            at,
            expires_at
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok(added)
    }

    pub async fn pair_state(&self, secret_hash: &str, at: i64) -> Result<PairState, sqlx::Error> {
        let paired = sqlx::query!(
            "SELECT u.username FROM devices d JOIN users u ON u.id = d.user_id
             WHERE d.secret_hash = ?1 AND d.kind = 'reader' AND u.disabled_at IS NULL",
            secret_hash
        )
        .fetch_optional(&self.read)
        .await?;
        if let Some(row) = paired {
            return Ok(PairState::Paired {
                username: row.username,
            });
        }
        let waiting = sqlx::query!(
            r#"SELECT 1 AS "found!: i64" FROM device_pairings
               WHERE secret_hash = ?1 AND expires_at > ?2"#,
            secret_hash,
            at
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(if waiting.is_some() {
            PairState::Pending
        } else {
            PairState::Unknown
        })
    }

    /// Makes the reader waiting with `code` one of this user's devices.
    /// Returns its name.
    pub async fn finish_pairing(
        &self,
        user_id: i64,
        code: &str,
        at: i64,
    ) -> Result<String, PairError> {
        let mut tx = self.write.begin().await?;
        let pairing = sqlx::query!(
            "SELECT secret_hash, name FROM device_pairings WHERE code = ?1 AND expires_at > ?2",
            code,
            at
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(PairError::NoSuchCode)?;
        sqlx::query!(
            "INSERT INTO devices (user_id, name, secret_hash, kind, created_at)
             VALUES (?1, ?2, ?3, 'reader', ?4)",
            user_id,
            pairing.name,
            pairing.secret_hash,
            at
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM device_pairings WHERE secret_hash = ?1",
            pairing.secret_hash
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(pairing.name)
    }

    /// Adds a reader that already has its secret, as one installed from
    /// the website does.
    pub async fn add_reader_device(
        &self,
        user_id: i64,
        name: &str,
        secret_hash: &str,
        at: i64,
    ) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!(
            r#"INSERT INTO devices (user_id, name, secret_hash, kind, created_at)
               VALUES (?1, ?2, ?3, 'reader', ?4) RETURNING id AS "id!""#,
            user_id,
            name,
            secret_hash,
            at
        )
        .fetch_one(&self.write)
        .await?;
        Ok(row.id)
    }

    pub async fn reader_device_by_token_hash(
        &self,
        hash: &str,
    ) -> Result<Option<ReaderDevice>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT d.id AS "device_id!", d.user_id, d.force_full_sync
               FROM devices d JOIN users u ON u.id = d.user_id
               WHERE d.secret_hash = ?1 AND d.kind = 'reader' AND u.disabled_at IS NULL"#,
            hash
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.map(|r| ReaderDevice {
            device_id: r.device_id,
            user_id: r.user_id,
            force_full_sync: r.force_full_sync != 0,
        }))
    }
}
