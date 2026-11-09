//! Shortcuts for the integration tests, so that a test about sync needn't go through
//! the website first. Nothing in the server reaches this, or the network.

use std::path::Path;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    auth::device::{generate_secret, hash_secret},
    db::{
        Db,
        models::{BookDetails, NewBook, Visibility},
        scope::UserScope,
    },
    error::AppError,
    storage::layout::Layout,
};

const DEV_QUOTA: i64 = 100 * 1024 * 1024 * 1024;

#[derive(Debug)]
pub struct DevImport<'a> {
    pub epub: &'a Path,
    pub kepub: Option<&'a Path>,
    pub cover: Option<&'a Path>,
    pub title: &'a str,
    pub authors: Vec<String>,
    /// Added to the file hash, so one file can seed many distinct test books.
    pub dedupe_salt: Option<String>,
    pub visibility: Visibility,
}

/// Returns the user's ID, creating the user if needed.
pub async fn ensure_user(db: &Db, name: &str) -> Result<i64, AppError> {
    if let Some(id) = db.user_id_by_name(name).await? {
        return Ok(id);
    }
    Ok(db.create_user(name, DEV_QUOTA, false).await?)
}

/// Creates a Kobo device and returns its secret, which is shown only once.
pub async fn create_kobo_device(
    db: &Db,
    user_id: i64,
    name: &str,
) -> Result<(i64, String), AppError> {
    let secret = generate_secret();
    let id = db
        .create_device(user_id, name, &hash_secret(&secret))
        .await?;
    Ok((id, secret))
}

pub fn scope(db: &Db, user_id: i64) -> UserScope {
    db.scope(user_id)
}

fn file_len(path: &Path) -> Result<i64, AppError> {
    Ok(std::fs::metadata(path)?.len() as i64)
}

/// Copies files into place and creates a book that's immediately sync-visible.
pub async fn import(
    scope: &UserScope,
    layout: &Layout,
    import: DevImport<'_>,
) -> Result<Uuid, AppError> {
    let id = Uuid::new_v4();
    std::fs::create_dir_all(layout.book_dir(id))?;

    let bytes = std::fs::read(import.epub)?;
    let mut sha = Sha256::new();
    sha.update(&bytes);
    if let Some(salt) = &import.dedupe_salt {
        sha.update(salt.as_bytes());
    }
    let original_sha256 = hex::encode(sha.finalize());
    std::fs::write(layout.original(id), &bytes)?;

    let kepub_size = match import.kepub {
        Some(path) => {
            std::fs::copy(path, layout.kepub(id))?;
            Some(file_len(path)?)
        }
        None => None,
    };

    let cover_id = match import.cover {
        Some(path) => {
            let cover = Uuid::new_v4();
            std::fs::copy(path, layout.cover(id, cover))?;
            Some(cover)
        }
        None => None,
    };

    let new = NewBook {
        id,
        title: import.title.to_owned(),
        authors: import.authors,
        cover_id,
        original_sha256,
        original_size: bytes.len() as i64,
        visibility: import.visibility,
        kepub_size,
        details: BookDetails::default(),
    };
    scope.create_book(&new, true).await?;
    Ok(id)
}

/// Copies a new cover into place and points the book at it.
pub async fn replace_cover(
    scope: &UserScope,
    layout: &Layout,
    book: Uuid,
    cover: &Path,
) -> Result<Uuid, AppError> {
    let id = Uuid::new_v4();
    std::fs::copy(cover, layout.cover(book, id))?;
    scope.set_cover(book, id).await?;
    Ok(id)
}

/// Deletes a book. The database commits first, so a crash leaves only a stray
/// directory, which the startup reconcile removes.
pub async fn delete(scope: &UserScope, layout: &Layout, book: Uuid) -> Result<(), AppError> {
    scope.delete_book(book).await?;
    std::fs::remove_dir_all(layout.book_dir(book))?;
    Ok(())
}
