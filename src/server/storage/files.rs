//! Writing files so that a crash leaves either the old state or the new one. All
//! of it is private to the server: directories `0700`, files `0600`.

use std::{io, os::unix::fs::PermissionsExt, path::Path};

use tokio::{fs::OpenOptions, io::AsyncWriteExt};

pub const DIR_MODE: u32 = 0o700;
pub const FILE_MODE: u32 = 0o600;

/// Creates a directory, and any missing parents, private to the server.
pub async fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true).mode(DIR_MODE);
    builder.create(path).await
}

/// A new file for writing. It must not exist yet, so a guessed name can't be used
/// to write through an existing link.
pub async fn create_private_file(path: &Path) -> io::Result<tokio::fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
        .await
}

/// Makes a rename or new file in this directory survive a crash.
pub async fn sync_dir(path: &Path) -> io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

/// Writes a whole file: to a temporary name beside it, then renamed into place.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?
        .to_owned();
    name.push(".tmp");
    let temporary = parent.join(name);

    let _ = tokio::fs::remove_file(&temporary).await;
    let mut file = create_private_file(&temporary).await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&temporary, path).await?;
    sync_dir(parent).await
}

/// Moves a finished file into place, private and safely on disk, whoever wrote it.
/// Both paths must be on the same disk, which they are, since both live under the
/// data directory.
pub async fn install(from: &Path, to: &Path) -> io::Result<()> {
    let file = tokio::fs::File::open(from).await?;
    file.set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
        .await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(from, to).await?;
    match to.parent() {
        Some(parent) => sync_dir(parent).await,
        None => Ok(()),
    }
}

/// Bytes free for this process on the disk holding `path`.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let stats = rustix::fs::statvfs(path)?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

/// Tightens a directory's mode, for a data directory that already existed.
pub async fn make_private(path: &Path) -> io::Result<()> {
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(DIR_MODE)).await
}
