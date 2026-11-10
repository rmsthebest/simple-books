//! Converts books to the format the simple-books reader draws. It runs after
//! the KEPUB, and a book that fails here still syncs to Kobos as before.
//!
//! Converting means reading hostile HTML, CSS and images, so it happens in a
//! child: this same program's hidden `convert-book` command. The child opens
//! its two files, then gives up all file and network access and caps its own
//! memory and CPU before reading a byte of the book.

use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    process::Stdio,
};

use book_format::{BookReader, FORMAT_VERSION, Limits};
use tokio::{process::Command, time::Instant};
use uuid::Uuid;

use super::{AttemptError, Tools, limited, wait_for};
use crate::server::{
    AppState,
    db::jobs::MAX_CONVERSION_ATTEMPTS,
    storage::files::{create_private_dir, install},
};

/// Bump the low part whenever the converter's output gets better, and every
/// book is converted again in the background. The format version is in the
/// high part, so a new format always does the same.
pub const REVISION: i64 = ((FORMAT_VERSION as i64) << 16) | 2;

/// The child's address space. A book's text and images are held in memory
/// while it's written.
const CHILD_MEMORY: u64 = 1536 * 1024 * 1024;

/// Handles one attempt on one book, if any is waiting. Returns whether one was.
pub async fn convert_next(state: &AppState, tools: &Tools) -> Result<bool, sqlx::Error> {
    let Some(book) = state.db.next_reader_conversion(REVISION).await? else {
        return Ok(false);
    };
    let started = Instant::now();
    let attempts = state.db.count_reader_attempt(book).await?;

    match attempt(state, tools, book).await {
        Ok(size) => {
            if state
                .db
                .finish_reader_conversion(book, REVISION, Some(size))
                .await?
            {
                tracing::info!(
                    book = %book,
                    ms = started.elapsed().as_millis(),
                    "converted for the reader"
                );
            } else {
                // Deleted while converting.
                let _ = tokio::fs::remove_file(state.layout.reader(book)).await;
            }
        }
        Err(error) => {
            tracing::warn!(book = %book, attempts, %error, "reader conversion failed");
            if attempts >= MAX_CONVERSION_ATTEMPTS {
                state
                    .db
                    .finish_reader_conversion(book, REVISION, None)
                    .await?;
                // A file from an earlier revision goes too, after the commit
                // that stops referring to it.
                let _ = tokio::fs::remove_file(state.layout.reader(book)).await;
            }
        }
    }
    Ok(true)
}

/// Runs the child in a private scratch directory, checks what it made, and
/// moves that into place. Returns its size.
async fn attempt(state: &AppState, tools: &Tools, book: Uuid) -> Result<i64, AttemptError> {
    let scratch = state.layout.tmp_dir().join(format!("reader-{book}"));
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    create_private_dir(&scratch).await?;

    let result = async {
        let output = scratch.join("out.book");
        let mut command = Command::new(&tools.helper);
        command
            .arg("convert-book")
            .arg("--input")
            .arg(state.layout.original(book))
            .arg("--output")
            .arg(&output)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        wait_for(command, tools.timeout).await?;

        let found = tokio::fs::symlink_metadata(&output).await?;
        if !found.is_file() {
            return Err(AttemptError::BadOutput("not a regular file".to_owned()));
        }
        let size = i64::try_from(found.len())
            .map_err(|_| AttemptError::BadOutput("too large".to_owned()))?;
        // The child's word isn't taken for it: the reader must be able to open
        // every part.
        let checked = output.clone();
        tokio::task::spawn_blocking(move || check(&checked))
            .await
            .map_err(|error| AttemptError::BadOutput(error.to_string()))?
            .map_err(AttemptError::BadOutput)?;

        install(&output, &state.layout.reader(book)).await?;
        Ok(size)
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&scratch).await;
    result
}

fn check(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut book =
        BookReader::open(BufReader::new(file), Limits::default()).map_err(|e| e.to_string())?;
    for index in 0..book.manifest().chapters.len() {
        book.chapter(index).map_err(|error| error.to_string())?;
    }
    for index in 0..book.manifest().images.len() {
        book.image(index).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The arguments of the hidden `convert-book` command.
#[derive(Debug, Clone)]
pub struct ConvertBook {
    pub input: PathBuf,
    /// Created, so it must not exist yet.
    pub output: PathBuf,
}

/// The hidden `convert-book` command, run in a child. The message on failure
/// is for the server's log.
///
/// # Errors
///
/// If a file can't be opened, the limits can't be applied, or the book can't
/// be converted.
pub fn convert_book(job: &ConvertBook) -> Result<(), String> {
    let input = File::open(&job.input).map_err(|error| format!("opening the book: {error}"))?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&job.output)
        .map_err(|error| format!("creating the output: {error}"))?;
    limited::confine(&[], &[], Some(CHILD_MEMORY))
        .map_err(|error| format!("applying limits: {error}"))?;

    let mut output = BufWriter::new(output);
    book_convert::convert(
        BufReader::new(input),
        &mut output,
        &book_convert::Options::default(),
    )
    .map_err(|error| error.to_string())?;
    output
        .into_inner()
        .map_err(|error| error.to_string())?
        .sync_all()
        .map_err(|error| error.to_string())
}
