//! Converts books to KEPUB, one at a time, in the background. A book stays out of
//! everyone's sync until this finishes, because a Kobo needs the final size.
//! When no KEPUB is waiting, books are converted to the reader's own format.

pub mod limited;
pub mod reader;

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Instant, timeout},
};
use uuid::Uuid;

use super::{
    AppState,
    db::{
        jobs::{Conversion, MAX_CONVERSION_ATTEMPTS},
        scope::BookError,
    },
    library::work,
    storage::files::{create_private_dir, install},
};

const IDLE_RECHECK: Duration = Duration::from_secs(60);
const MAX_LOGGED_STDERR: u64 = 2048;
/// In Go's own terms: where it starts collecting harder, not a hard stop.
const GO_MEMORY_LIMIT: &str = "400MiB";

/// How to run kepubify.
#[derive(Debug, Clone)]
pub struct Tools {
    pub kepubify: PathBuf,
    /// This program, for its `exec-limited` command.
    pub helper: PathBuf,
    pub timeout: Duration,
    /// More read-only paths for it. A static kepubify needs none.
    pub extra_read: Vec<PathBuf>,
}

impl Tools {
    pub fn new(kepubify: PathBuf, helper: PathBuf) -> Self {
        Self {
            kepubify,
            helper,
            timeout: Duration::from_secs(600),
            extra_read: Vec::new(),
        }
    }

    /// The version, to record with each conversion. `None` means it can't be run,
    /// in which case books will still work, served as EPUB.
    pub async fn kepubify_version(&self) -> Option<String> {
        let output = Command::new(&self.kepubify)
            .arg("--version")
            .env_clear()
            .kill_on_drop(true)
            .output();
        let output = timeout(Duration::from_secs(10), output).await.ok()?.ok()?;
        if !output.status.success() {
            return None;
        }
        let version = String::from_utf8_lossy(&output.stdout);
        let version = version.split_whitespace().last()?;
        (version.len() <= 32).then(|| version.to_owned())
    }
}

/// Works through the queue until told to stop, which is never.
pub async fn run(state: AppState, tools: Tools) {
    let version = tools.kepubify_version().await;
    if version.is_none() {
        tracing::warn!(
            path = %tools.kepubify.display(),
            "kepubify can't be run, so books will be served as EPUB"
        );
    }
    loop {
        let worked = match convert_next(&state, &tools, version.as_deref()).await {
            Ok(false) => reader::convert_next(&state, &tools).await,
            other => other,
        };
        match worked {
            Ok(true) => {}
            Ok(false) => {
                let _ = timeout(IDLE_RECHECK, state.conversions.notified()).await;
            }
            Err(error) => {
                tracing::error!(%error, "conversion queue failed");
                tokio::time::sleep(IDLE_RECHECK).await;
            }
        }
    }
}

/// Handles one attempt on one book, if any is waiting. Returns whether one was.
pub async fn convert_next(
    state: &AppState,
    tools: &Tools,
    version: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let Some(job) = state.db.next_conversion().await? else {
        return Ok(false);
    };
    let started = Instant::now();
    let attempts = state.db.count_conversion_attempt(job.book).await?;

    let scope = state.db.scope(job.owner_id);
    match attempt(state, tools, job.book).await {
        Ok(size) => match scope.mark_kepub(job.book, Some(size)).await {
            Ok(()) => {
                if let Some(version) = version {
                    state.db.record_kepubify_version(job.book, version).await?;
                }
                tracing::info!(
                    book = %job.book,
                    ms = started.elapsed().as_millis(),
                    "converted"
                );
            }
            Err(error) => note_mark_failure(state, job, &error).await,
        },
        Err(error) => {
            tracing::warn!(book = %job.book, attempts, %error, "conversion failed");
            if attempts >= MAX_CONVERSION_ATTEMPTS {
                // The book still reaches people, as the EPUB they uploaded.
                if let Err(error) = scope.mark_kepub(job.book, None).await {
                    note_mark_failure(state, job, &error).await;
                }
            }
        }
    }
    Ok(true)
}

/// The usual cause is that the book was deleted while converting.
async fn note_mark_failure(state: &AppState, job: Conversion, error: &BookError) {
    tracing::debug!(book = %job.book, %error, "couldn't record a conversion");
    let _ = tokio::fs::remove_file(state.layout.kepub(job.book)).await;
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AttemptError {
    #[error("couldn't run it: {0}")]
    Io(#[from] std::io::Error),
    #[error("it took longer than {0:?}")]
    TimedOut(Duration),
    #[error("it failed: {0}")]
    Failed(String),
    #[error("its output didn't pass checks: {0}")]
    BadOutput(String),
}

/// Runs kepubify in a private scratch directory, checks what it made, and moves
/// that into place. Returns its size.
async fn attempt(state: &AppState, tools: &Tools, book: Uuid) -> Result<i64, AttemptError> {
    let scratch = state.layout.tmp_dir().join(format!("convert-{book}"));
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    create_private_dir(&scratch).await?;

    let result = async {
        let output = scratch.join("out.kepub.epub");
        let input = state.layout.original(book);
        run_sandboxed(tools, &input, &scratch, &output).await?;

        // Not followed: from here on, this server reads it with its own access.
        let found = tokio::fs::symlink_metadata(&output).await?;
        if !found.is_file() {
            return Err(AttemptError::BadOutput("not a regular file".to_owned()));
        }
        let size = i64::try_from(found.len())
            .map_err(|_| AttemptError::BadOutput("too large".to_owned()))?;
        // What goes to a Kobo has to be readable, however it was produced.
        work::parse_epub(output.clone())
            .await
            .map_err(|error| AttemptError::BadOutput(error.to_string()))?;

        install(&output, &state.layout.kepub(book)).await?;
        Ok(size)
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&scratch).await;
    result
}

async fn run_sandboxed(
    tools: &Tools,
    input: &Path,
    scratch: &Path,
    output: &Path,
) -> Result<(), AttemptError> {
    let mut command = Command::new(&tools.helper);
    command
        .arg("exec-limited")
        .arg("--program")
        .arg(&tools.kepubify)
        .arg("--work")
        .arg(scratch)
        .arg("--read")
        .arg(input);
    for path in &tools.extra_read {
        command.arg("--read").arg(path);
    }
    command
        .arg("--")
        .arg("-o")
        .arg(output)
        .arg(input)
        .env_clear()
        .env("GOMAXPROCS", "1")
        .env("GOMEMLIMIT", GO_MEMORY_LIMIT)
        .env("TMPDIR", scratch)
        .env("HOME", scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    wait_for(command, tools.timeout).await
}

/// Runs a child to completion, or kills it at `limit`. Its first stderr is kept
/// for the error, and the rest is drained so it can't block on a full pipe.
pub(crate) async fn wait_for(mut command: Command, limit: Duration) -> Result<(), AttemptError> {
    let mut child = command.spawn()?;
    let mut stderr = String::new();
    let captured = child.stderr.take();
    let wait = async {
        if let Some(pipe) = captured {
            let mut limited = pipe.take(MAX_LOGGED_STDERR);
            let _ = limited.read_to_string(&mut stderr).await;
            // Keep draining, or a chatty child would block on a full pipe.
            let mut rest = limited.into_inner();
            let _ = tokio::io::copy(&mut rest, &mut tokio::io::sink()).await;
        }
        child.wait().await
    };
    let status = timeout(limit, wait)
        .await
        .map_err(|_| AttemptError::TimedOut(limit))??;

    if status.success() {
        Ok(())
    } else {
        Err(AttemptError::Failed(format!("{status}: {}", stderr.trim())))
    }
}
