//! The CPU- and memory-heavy steps. They run off the async runtime, and only a
//! couple at a time, so a burst of uploads queues instead of exhausting memory.

use std::{fs::File, io::BufReader, path::PathBuf};

use epub::{Book, EpubError, Limits};
use tokio::sync::Semaphore;

use crate::server::storage::covers::{self, CoverError, ProcessedCover};

const MAX_CONCURRENT: usize = 2;
static PERMITS: Semaphore = Semaphore::const_new(MAX_CONCURRENT);

fn gone() -> std::io::Error {
    std::io::Error::other("the worker stopped")
}

/// Reads and validates an EPUB on disk.
pub async fn parse_epub(path: PathBuf) -> Result<Book, EpubError> {
    let _permit = PERMITS.acquire().await.map_err(|_| EpubError::Io(gone()))?;
    tokio::task::spawn_blocking(move || {
        let file = File::open(path).map_err(EpubError::Io)?;
        epub::parse(BufReader::new(file), &Limits::default())
    })
    .await
    .map_err(|_| EpubError::Io(gone()))?
}

/// Re-encodes an untrusted image.
pub async fn process_cover(bytes: Vec<u8>) -> Result<ProcessedCover, CoverError> {
    let failed = |_| CoverError::Encode("the worker stopped".to_owned());
    let _permit = PERMITS.acquire().await.map_err(|_| CoverError::Invalid)?;
    tokio::task::spawn_blocking(move || covers::process(&bytes))
        .await
        .map_err(failed)?
}
