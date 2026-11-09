//! SQLite access. The pools are private to this module, so the only way to
//! reach book data from elsewhere is through a [`scope::UserScope`].

pub mod accounts;
pub mod hardcover;
pub mod jobs;
pub mod models;
pub mod scope;

use std::{path::Path, time::Duration};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::server::now;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

#[derive(Clone, Debug)]
pub struct Db {
    read: SqlitePool,
    /// One connection, so writers queue here instead of hitting `SQLITE_BUSY`.
    write: SqlitePool,
}

/// What a Kobo device token resolves to.
#[derive(Debug, Clone)]
pub struct KoboDevice {
    pub device_id: i64,
    pub user_id: i64,
    pub force_full_sync: bool,
    /// The older device this one takes over from, once it's first seen.
    pub replaces_device_id: Option<i64>,
}

impl Db {
    pub async fn open(path: &Path) -> Result<Self, sqlx::Error> {
        let options = || {
            SqliteConnectOptions::new()
                .filename(path)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(5))
                .pragma("trusted_schema", "OFF")
        };

        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options().create_if_missing(true))
            .await?;
        MIGRATOR.run(&write).await?;

        let read = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options().read_only(true))
            .await?;

        Ok(Self { read, write })
    }

    pub async fn kobo_device_by_token_hash(
        &self,
        hash: &str,
    ) -> Result<Option<KoboDevice>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT d.id AS "device_id!", d.user_id, d.force_full_sync, d.replaces_device_id
               FROM devices d JOIN users u ON u.id = d.user_id
               WHERE d.secret_hash = ?1 AND u.disabled_at IS NULL"#,
            hash
        )
        .fetch_optional(&self.read)
        .await?;

        Ok(row.map(|r| KoboDevice {
            device_id: r.device_id,
            user_id: r.user_id,
            force_full_sync: r.force_full_sync != 0,
            replaces_device_id: r.replaces_device_id,
        }))
    }

    /// Records that a device was seen, at most once a minute to limit writes.
    pub async fn touch_device(&self, device_id: i64) -> Result<(), sqlx::Error> {
        let at = now();
        let stale_before = at - 60;
        sqlx::query!(
            "UPDATE devices SET last_seen_at = ?1
             WHERE id = ?2 AND (last_seen_at IS NULL OR last_seen_at < ?3)",
            at,
            device_id,
            stale_before
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    pub async fn clear_force_full_sync(&self, device_id: i64) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE devices SET force_full_sync = 0 WHERE id = ?1",
            device_id
        )
        .execute(&self.write)
        .await?;
        Ok(())
    }

    /// Creates a user with no password. They set one through a setup link.
    pub async fn create_user(
        &self,
        username: &str,
        quota_bytes: i64,
        is_admin: bool,
    ) -> Result<i64, sqlx::Error> {
        let at = now();
        let admin = i64::from(is_admin);
        let row = sqlx::query!(
            r#"INSERT INTO users (username, is_admin, quota_bytes, created_at)
               VALUES (?1, ?2, ?3, ?4) RETURNING id AS "id!""#,
            username,
            admin,
            quota_bytes,
            at
        )
        .fetch_one(&self.write)
        .await?;
        Ok(row.id)
    }

    pub async fn user_id_by_name(&self, username: &str) -> Result<Option<i64>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT id AS "id!" FROM users WHERE username = ?1"#,
            username
        )
        .fetch_optional(&self.read)
        .await?;
        Ok(row.map(|r| r.id))
    }

    pub async fn create_device(
        &self,
        user_id: i64,
        name: &str,
        secret_hash: &str,
    ) -> Result<i64, sqlx::Error> {
        let at = now();
        let row = sqlx::query!(
            r#"INSERT INTO devices (user_id, name, secret_hash, created_at)
               VALUES (?1, ?2, ?3, ?4) RETURNING id AS "id!""#,
            user_id,
            name,
            secret_hash,
            at
        )
        .fetch_one(&self.write)
        .await?;
        Ok(row.id)
    }

    /// Builds the scope for a user whose identity has been authenticated.
    /// Keep calls to this inside auth extractors and the CLI.
    pub(crate) fn scope(&self, user_id: i64) -> scope::UserScope {
        scope::UserScope::new(self.clone(), user_id)
    }
}
