//! Users, setup links, sessions and device credentials. Every secret here is a
//! hash; the secrets themselves are never stored.

use super::Db;
use crate::server::now;

/// What's needed to check a login. Disabled users never get this far.
#[derive(Debug, Clone)]
pub struct LoginCandidate {
    pub id: i64,
    pub password_hash: Option<String>,
}

/// Who a session belongs to.
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
}

/// One row of the admin's user list.
#[derive(Debug, Clone)]
pub struct UserSummary {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub has_password: bool,
    pub quota_bytes: i64,
    pub used_bytes: i64,
}

/// A device as its owner sees it.
#[derive(Debug, Clone)]
pub struct DeviceSummary {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
}

impl Db {
    pub async fn has_users(&self) -> Result<bool, sqlx::Error> {
        let row = sqlx::query!(r#"SELECT EXISTS (SELECT 1 FROM users) AS "any!: i64""#)
            .fetch_one(&self.read)
            .await?;
        Ok(row.any != 0)
    }

    pub async fn login_candidate(
        &self,
        username: &str,
    ) -> Result<Option<LoginCandidate>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT id AS "id!", password_hash FROM users
               WHERE username = ?1 AND disabled_at IS NULL"#,
            username
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.map(|r| LoginCandidate {
            id: r.id,
            password_hash: r.password_hash,
        }))
    }

    pub async fn list_users(&self) -> Result<Vec<UserSummary>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT u.id AS "id!", u.username, u.is_admin, u.disabled_at, u.password_hash,
                      u.quota_bytes,
                      (SELECT COALESCE(SUM(b.original_size + COALESCE(b.kepub_size, 0)), 0)
                       FROM books b WHERE b.owner_id = u.id) AS "used_bytes!: i64"
               FROM users u ORDER BY u.username"#
        )
        .fetch_all(&self.read)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| UserSummary {
                id: r.id,
                username: r.username,
                is_admin: r.is_admin != 0,
                disabled: r.disabled_at.is_some(),
                has_password: r.password_hash.is_some(),
                quota_bytes: r.quota_bytes,
                used_bytes: r.used_bytes,
            })
            .collect())
    }

    /// Disabling also ends their sessions and setup links at once. Their devices
    /// stop working too, because device lookup checks the user.
    pub async fn set_user_disabled(
        &self,
        user_id: i64,
        disabled: bool,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.write.begin().await?;
        let at = disabled.then(now);
        let found = sqlx::query!(
            "UPDATE users SET disabled_at = ?1 WHERE id = ?2",
            at,
            user_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if disabled {
            sqlx::query!("DELETE FROM sessions WHERE user_id = ?1", user_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query!("DELETE FROM setup_tokens WHERE user_id = ?1", user_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(found)
    }

    pub async fn set_user_quota(
        &self,
        user_id: i64,
        quota_bytes: i64,
    ) -> Result<bool, sqlx::Error> {
        let done = sqlx::query!(
            "UPDATE users SET quota_bytes = ?1 WHERE id = ?2",
            quota_bytes,
            user_id
        )
        .execute(&self.write)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Stores a setup link, replacing any earlier one for this user.
    pub async fn replace_setup_token(
        &self,
        user_id: i64,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.write.begin().await?;
        sqlx::query!("DELETE FROM setup_tokens WHERE user_id = ?1", user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query!(
            "INSERT INTO setup_tokens (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
            token_hash,
            user_id,
            expires_at
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The user a setup link is for, if it's still good.
    pub async fn setup_token_user(&self, token_hash: &str) -> Result<Option<String>, sqlx::Error> {
        let at = now();
        let row = sqlx::query!(
            "SELECT u.username FROM setup_tokens t JOIN users u ON u.id = t.user_id
             WHERE t.token_hash = ?1 AND t.expires_at > ?2 AND u.disabled_at IS NULL",
            token_hash,
            at
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.map(|r| r.username))
    }

    /// Uses up a setup link: sets the password and ends every session, so a
    /// reset also signs out whoever else was in. `None` if the link is no good.
    pub async fn redeem_setup_token(
        &self,
        token_hash: &str,
        password_hash: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        let mut tx = self.write.begin().await?;
        let at = now();
        let Some(token) = sqlx::query!(
            r#"DELETE FROM setup_tokens WHERE token_hash = ?1 AND expires_at > ?2
               RETURNING user_id AS "user_id!""#,
            token_hash,
            at
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let updated = sqlx::query!(
            "UPDATE users SET password_hash = ?1 WHERE id = ?2 AND disabled_at IS NULL",
            password_hash,
            token.user_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            return Ok(None);
        }
        sqlx::query!("DELETE FROM sessions WHERE user_id = ?1", token.user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(token.user_id))
    }

    pub async fn create_session(
        &self,
        token_hash: &str,
        user_id: i64,
        expires_at: i64,
    ) -> Result<(), sqlx::Error> {
        let at = now();
        sqlx::query!(
            "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            token_hash,
            user_id,
            at,
            expires_at
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn session_user(&self, token_hash: &str) -> Result<Option<SessionUser>, sqlx::Error> {
        let at = now();
        let row = sqlx::query!(
            r#"SELECT u.id AS "id!", u.username, u.is_admin
               FROM sessions s JOIN users u ON u.id = s.user_id
               WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND u.disabled_at IS NULL"#,
            token_hash,
            at
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.map(|r| SessionUser {
            id: r.id,
            username: r.username,
            is_admin: r.is_admin != 0,
        }))
    }

    /// Extends a session in use, at most every few minutes to limit writes.
    pub async fn touch_session(
        &self,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<(), sqlx::Error> {
        let at = now();
        let stale_before = at - 300;
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = ?1, expires_at = ?2
             WHERE token_hash = ?3 AND last_seen_at < ?4",
            at,
            expires_at,
            token_hash,
            stale_before
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<(), sqlx::Error> {
        sqlx::query!("DELETE FROM sessions WHERE token_hash = ?1", token_hash)
            .execute(&self.write)
            .await?;
        Ok(())
    }

    /// Clears out sessions and setup links past their expiry.
    pub async fn delete_expired_credentials(&self) -> Result<(), sqlx::Error> {
        let at = now();
        sqlx::query!("DELETE FROM sessions WHERE expires_at <= ?1", at)
            .execute(&self.write)
            .await?;
        sqlx::query!("DELETE FROM setup_tokens WHERE expires_at <= ?1", at)
            .execute(&self.write)
            .await?;
        Ok(())
    }

    /// A user's devices. A reconnect that hasn't been used yet isn't one.
    pub async fn list_devices(&self, user_id: i64) -> Result<Vec<DeviceSummary>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", name, created_at, last_seen_at FROM devices
               WHERE user_id = ?1 AND (replaces_device_id IS NULL OR last_seen_at IS NOT NULL)
               ORDER BY created_at"#,
            user_id
        )
        .fetch_all(&self.read)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DeviceSummary {
                id: r.id,
                name: r.name,
                created_at: r.created_at,
                last_seen_at: r.last_seen_at,
            })
            .collect())
    }

    /// Issues a new credential for a device. The old one keeps working until the
    /// new one is first seen, so a patched config that's never applied can't break
    /// a working Kobo. Repeat calls each add a candidate, because any of the files
    /// handed out may be the one that gets applied. `None` if it isn't their device.
    pub async fn begin_reconnect(
        &self,
        user_id: i64,
        device_id: i64,
        secret_hash: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        let at = now();
        let row = sqlx::query!(
            r#"INSERT INTO devices (user_id, name, secret_hash, replaces_device_id, created_at)
               SELECT user_id, name, ?1, id, ?2 FROM devices WHERE id = ?3 AND user_id = ?4
               RETURNING id AS "id!""#,
            secret_hash,
            at,
            device_id,
            user_id
        )
        .fetch_optional(&self.write)
        .await?;
        Ok(row.map(|r| r.id))
    }

    /// A new credential has been seen: it takes over, and the device it replaces
    /// and any other unused candidates go away.
    pub async fn finish_reconnect(&self, device_id: i64) -> Result<(), sqlx::Error> {
        let mut tx = self.write.begin().await?;
        let row = sqlx::query!(
            "SELECT replaces_device_id FROM devices WHERE id = ?1",
            device_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(replaced) = row.and_then(|r| r.replaces_device_id) {
            sqlx::query!(
                "DELETE FROM devices WHERE replaces_device_id = ?1 AND id <> ?2",
                replaced,
                device_id
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!("DELETE FROM devices WHERE id = ?1", replaced)
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "UPDATE devices SET replaces_device_id = NULL WHERE id = ?1",
                device_id
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Removes a device, along with any reconnect in progress for it.
    pub async fn revoke_device(&self, user_id: i64, device_id: i64) -> Result<bool, sqlx::Error> {
        let mut tx = self.write.begin().await?;
        sqlx::query!(
            "DELETE FROM devices WHERE user_id = ?1 AND replaces_device_id = ?2",
            user_id,
            device_id
        )
        .execute(&mut *tx)
        .await?;
        let found = sqlx::query!(
            "DELETE FROM devices WHERE user_id = ?1 AND id = ?2",
            user_id,
            device_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok(found)
    }

    /// Makes a device receive everything again at its next sync.
    pub async fn request_full_sync(
        &self,
        user_id: i64,
        device_id: i64,
    ) -> Result<bool, sqlx::Error> {
        let done = sqlx::query!(
            "UPDATE devices SET force_full_sync = 1 WHERE id = ?1 AND user_id = ?2",
            device_id,
            user_id
        )
        .execute(&self.write)
        .await?;
        Ok(done.rows_affected() == 1)
    }
}
