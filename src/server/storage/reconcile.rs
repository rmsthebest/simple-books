//! Runs at startup, and puts the disk and the database back in agreement after a
//! crash. Files are written before the commit that refers to them and removed
//! after the commit that stops referring to them, so stray files are the only
//! damage a crash can leave, unless something else has been at the data directory.

use std::{collections::HashMap, io, path::Path};

use uuid::Uuid;

use super::{
    covers,
    files::{create_private_dir, make_private, write_atomic},
    layout::Layout,
};
use crate::server::db::{Db, jobs::StoredBook};

/// What a run changed, for the log and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub removed_dirs: usize,
    pub removed_files: usize,
    pub requeued_conversions: usize,
    pub rebuilt_thumbs: usize,
    pub missing_originals: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Prepares the data directory. Call this before serving anything.
pub async fn run(layout: &Layout, db: &Db) -> Result<Report, ReconcileError> {
    create_private_dir(layout.root()).await?;
    if let Err(error) = make_private(layout.root()).await {
        // A mounted volume may be owned by someone else; that's for them to decide.
        tracing::warn!(%error, "couldn't make the data directory private");
    }
    create_private_dir(&layout.books_dir()).await?;

    let tmp = layout.tmp_dir();
    if tokio::fs::try_exists(&tmp).await? {
        tokio::fs::remove_dir_all(&tmp).await?;
    }
    create_private_dir(&tmp).await?;

    let books: HashMap<Uuid, StoredBook> = db
        .stored_books()
        .await?
        .into_iter()
        .map(|book| (book.id, book))
        .collect();

    // Files on disk with no books at all means a lost or wrong database, not a
    // library's worth of leftovers. Removing them could destroy everything.
    let mut entries = tokio::fs::read_dir(layout.books_dir()).await?;
    if books.is_empty() && entries.next_entry().await?.is_some() {
        tracing::warn!(
            path = %layout.books_dir().display(),
            "there are book files but the database has no books, so nothing was removed"
        );
        return Ok(Report::default());
    }

    let mut report = Report::default();
    let mut entries = tokio::fs::read_dir(layout.books_dir()).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let known = name
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
            .and_then(|id| books.get(&id));
        if let Some(book) = known {
            remove_stale_files(&entry.path(), book, &mut report).await?;
        } else {
            let path = entry.path();
            if entry.file_type().await?.is_dir() {
                tokio::fs::remove_dir_all(&path).await?;
            } else {
                tokio::fs::remove_file(&path).await?;
            }
            report.removed_dirs += 1;
        }
    }

    for book in books.values() {
        check_book(layout, db, book, &mut report).await?;
    }
    Ok(report)
}

/// Removes leftovers in a book's directory: old covers, and half-written files.
async fn remove_stale_files(
    dir: &Path,
    book: &StoredBook,
    report: &mut Report,
) -> Result<(), ReconcileError> {
    let cover = book.cover_id.map(|id| id.to_string());
    let keep = |name: &str| {
        name == "original.epub"
            || name == "kobo.kepub.epub"
            || cover.as_deref().is_some_and(|cover| {
                name == format!("cover-{cover}.jpg") || name == format!("thumb-{cover}.jpg")
            })
    };
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_name().to_str().is_some_and(keep) {
            tokio::fs::remove_file(entry.path()).await?;
            report.removed_files += 1;
        }
    }
    Ok(())
}

async fn check_book(
    layout: &Layout,
    db: &Db,
    book: &StoredBook,
    report: &mut Report,
) -> Result<(), ReconcileError> {
    if !tokio::fs::try_exists(layout.original(book.id)).await? {
        // Nothing can bring this back, and removing the row would take the book
        // off someone's Kobo. It needs a person to look at it.
        tracing::error!(book = %book.id, "a book's original file is missing");
        report.missing_originals += 1;
        return Ok(());
    }
    if book.kepub_ready && !tokio::fs::try_exists(layout.kepub(book.id)).await? {
        db.requeue_conversion(book.id).await?;
        report.requeued_conversions += 1;
    }
    if let Some(cover) = book.cover_id {
        let thumb = layout.thumb(book.id, cover);
        let full = layout.cover(book.id, cover);
        if !tokio::fs::try_exists(&thumb).await? && tokio::fs::try_exists(&full).await? {
            let bytes = tokio::fs::read(&full).await?;
            let rebuilt = tokio::task::spawn_blocking(move || covers::process(&bytes))
                .await
                .map_err(io::Error::other)?;
            if let Ok(cover) = rebuilt {
                write_atomic(&thumb, &cover.thumb).await?;
                report.rebuilt_thumbs += 1;
            }
        }
    }
    Ok(())
}
