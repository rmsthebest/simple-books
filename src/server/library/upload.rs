//! Getting books in. The file is streamed to a private temp file and hashed on the
//! way, so memory use stays flat however large it is, and nothing is believed until
//! the parser has been through all of it.

use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use axum::{
    Json,
    extract::{Multipart, Path, State, multipart::Field},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::work;
use crate::server::{
    AppState,
    auth::session::CurrentUser,
    db::{
        models::{BookDetails, NewBook, Visibility},
        scope::{BookError, Duplicate, UserScope},
    },
    error::AppError,
    storage::{
        covers::ProcessedCover,
        files::{
            create_private_dir, create_private_file, free_bytes, install, sync_dir, write_atomic,
        },
    },
};

const MAX_UPLOADS_PER_USER: usize = 2;
const MAX_TITLE_CHARS: usize = 500;
const MAX_COVER_BYTES: usize = 32 * 1024 * 1024;
const FALLBACK_TITLE: &str = "Untitled";

/// What to do next, in words anyone can follow. A Kobo only hears about a book when
/// it syncs, and only downloads it when tapped.
pub const SYNC_HINT: &str = "On your Kobo: tap sync, and it'll be in My Books.";

/// Why an upload didn't work. The message is for the person who tried.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("{0}")]
    NotABook(#[from] epub::EpubError),
    #[error("that isn't a JPEG, PNG or WebP image")]
    NotAnImage,
    #[error("send a file")]
    NoFile,
    #[error("this form has something unexpected in it")]
    BadForm,
    #[error("that's larger than this server accepts")]
    TooLarge,
    #[error("you've reached your storage limit")]
    QuotaExceeded,
    #[error("you're already uploading two books; try again when one finishes")]
    TooManyAtOnce,
    #[error("the server is nearly out of disk space")]
    DiskFull,
    #[error("the upload was interrupted")]
    Interrupted,
    #[error(transparent)]
    Server(#[from] AppError),
}

impl From<std::io::Error> for UploadError {
    fn from(error: std::io::Error) -> Self {
        Self::Server(AppError::Io(error))
    }
}

impl From<sqlx::Error> for UploadError {
    fn from(error: sqlx::Error) -> Self {
        Self::Server(AppError::Db(error))
    }
}

impl From<BookError> for UploadError {
    fn from(error: BookError) -> Self {
        match error {
            BookError::QuotaExceeded => Self::QuotaExceeded,
            other => Self::Server(other.into()),
        }
    }
}

impl IntoResponse for UploadError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotABook(epub::EpubError::Io(_)) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::NotABook(_) | Self::NotAnImage => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NoFile | Self::BadForm | Self::Interrupted => StatusCode::BAD_REQUEST,
            Self::TooLarge | Self::QuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TooManyAtOnce => StatusCode::TOO_MANY_REQUESTS,
            Self::DiskFull => StatusCode::INSUFFICIENT_STORAGE,
            Self::Server(error) => return AppErrorRef(error).into_response(),
        };
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}

impl UploadError {
    /// The code for a page's `?error=`, for a browser posting a plain form.
    fn code(&self) -> &'static str {
        match self {
            Self::NotABook(epub::EpubError::Io(_)) | Self::Server(_) => "server",
            Self::NotABook(_) => "not_a_book",
            Self::NotAnImage => "not_image",
            Self::NoFile => "no_file",
            Self::BadForm => "bad_form",
            Self::TooLarge => "too_large",
            Self::QuotaExceeded => "storage",
            Self::TooManyAtOnce => "busy",
            Self::DiskFull => "disk",
            Self::Interrupted => "interrupted",
        }
    }

    fn log(&self) {
        if let Self::Server(error) = self {
            error.log();
        }
    }
}

/// Scripts ask for JSON. A plain form gets a redirect back to a page that says what
/// happened, and never a page of JSON.
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/json"))
}

/// Lets a server error log itself and hide its details, as it does everywhere.
struct AppErrorRef<'a>(&'a AppError);

impl IntoResponse for AppErrorRef<'_> {
    fn into_response(self) -> Response {
        self.0.log();
        self.0.status().into_response()
    }
}

/// Counts uploads in progress per user.
#[derive(Debug, Default)]
pub struct UploadGate {
    counts: Mutex<HashMap<i64, usize>>,
}

struct UploadSlot {
    gate: Arc<UploadGate>,
    user: i64,
}

impl UploadGate {
    fn enter(self: &Arc<Self>, user: i64) -> Option<UploadSlot> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(user).or_insert(0);
        if *count >= MAX_UPLOADS_PER_USER {
            return None;
        }
        *count += 1;
        Some(UploadSlot {
            gate: Arc::clone(self),
            user,
        })
    }
}

impl Drop for UploadSlot {
    fn drop(&mut self) {
        let mut counts = self
            .gate
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.user) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.user);
            }
        }
    }
}

/// Removes a file when dropped, so a dropped connection leaves nothing behind.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The book's directory, removed if the upload fails before it's committed.
struct NewBookDir {
    path: PathBuf,
    committed: bool,
}

impl Drop for NewBookDir {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// What became of an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Stored as a new book.
    Added,
    /// The user had this file already.
    AlreadyYours,
    /// Someone shares this file, so it went into the user's library from there.
    AddedFromShared,
}

#[derive(Debug, Serialize)]
pub struct UploadReply {
    pub outcome: Outcome,
    pub id: Uuid,
    pub title: String,
}

struct Received {
    temp: TempFile,
    size: i64,
    sha256: String,
    filename: Option<String>,
}

/// `POST /books/upload`: a form with a `file`, and optionally `visibility`.
pub async fn upload(
    user: CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Multipart,
) -> Result<Response, UploadError> {
    let result = receive_upload(user, state, form).await;
    if wants_json(&headers) {
        return result;
    }
    Ok(match result {
        Ok(_) => Redirect::to("/?done=uploaded").into_response(),
        Err(error) => {
            error.log();
            Redirect::to(&format!("/?error={}", error.code())).into_response()
        }
    })
}

async fn receive_upload(
    user: CurrentUser,
    state: AppState,
    mut form: Multipart,
) -> Result<Response, UploadError> {
    let _slot = state
        .uploads
        .enter(user.id)
        .ok_or(UploadError::TooManyAtOnce)?;
    check_disk(&state)?;

    let mut received = None;
    let mut visibility = Visibility::Private;
    while let Some(mut field) = form.next_field().await.map_err(form_error)? {
        match field.name() {
            Some("visibility") => visibility = read_visibility(field).await?,
            Some("file") if received.is_none() => {
                received = Some(receive_file(&state, &mut field).await?);
            }
            _ => return Err(UploadError::BadForm),
        }
    }
    let received = received.ok_or(UploadError::NoFile)?;

    let reply = ingest(&state, &user.scope, received, visibility).await?;
    let status = match reply.outcome {
        Outcome::Added => StatusCode::CREATED,
        Outcome::AlreadyYours | Outcome::AddedFromShared => StatusCode::OK,
    };
    Ok((status, Json(reply)).into_response())
}

fn check_disk(state: &AppState) -> Result<(), UploadError> {
    if free_bytes(state.layout.root())? < state.config.min_free_disk_bytes {
        return Err(UploadError::DiskFull);
    }
    Ok(())
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "it goes straight into map_err"
)]
fn form_error(error: axum::extract::multipart::MultipartError) -> UploadError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        UploadError::TooLarge
    } else {
        UploadError::Interrupted
    }
}

async fn read_visibility(mut field: Field<'_>) -> Result<Visibility, UploadError> {
    let mut value = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(form_error)? {
        value.extend_from_slice(&chunk);
        if value.len() > 16 {
            return Err(UploadError::BadForm);
        }
    }
    match value.as_slice() {
        b"private" => Ok(Visibility::Private),
        b"shared" => Ok(Visibility::Shared),
        _ => Err(UploadError::BadForm),
    }
}

async fn receive_file(state: &AppState, field: &mut Field<'_>) -> Result<Received, UploadError> {
    let filename = field.file_name().map(str::to_owned);
    let path = state
        .layout
        .tmp_dir()
        .join(format!("{}.upload", Uuid::new_v4()));
    let mut file = create_private_file(&path).await?;
    let temp = TempFile(path);

    let mut sha = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = field.chunk().await.map_err(form_error)? {
        size += chunk.len() as u64;
        if size > state.config.max_upload_bytes {
            return Err(UploadError::TooLarge);
        }
        sha.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;

    if size == 0 {
        return Err(UploadError::NoFile);
    }
    Ok(Received {
        temp,
        size: i64::try_from(size).map_err(|_| UploadError::TooLarge)?,
        sha256: hex::encode(sha.finalize()),
        filename,
    })
}

async fn ingest(
    state: &AppState,
    scope: &UserScope,
    received: Received,
    visibility: Visibility,
) -> Result<UploadReply, UploadError> {
    // The stored copy passed the parser when it arrived, and the hash says this
    // is the same file, so there's no need to parse it again.
    if let Some(reply) = reuse(scope, &received.sha256, visibility).await? {
        return Ok(reply);
    }

    let book = work::parse_epub(received.temp.0.clone()).await?;
    let id = Uuid::new_v4();
    let new_dir = NewBookDir {
        path: state.layout.book_dir(id),
        committed: false,
    };
    create_private_dir(&new_dir.path).await?;

    let cover = match book.cover {
        Some(bytes) => {
            // A book with a bad cover is still a good book.
            work::process_cover(bytes.0).await.ok()
        }
        None => None,
    };
    let cover_id = match &cover {
        Some(cover) => Some(store_cover(state, id, cover).await?),
        None => None,
    };

    install(&received.temp.0, &state.layout.original(id)).await?;
    sync_dir(&state.layout.books_dir()).await?;

    let metadata = book.metadata;
    let title = metadata
        .title
        .or_else(|| title_from_filename(received.filename.as_deref()))
        .unwrap_or_else(|| FALLBACK_TITLE.to_owned());
    let new = NewBook {
        id,
        title: title.clone(),
        authors: metadata.authors,
        cover_id,
        original_sha256: received.sha256.clone(),
        original_size: received.size,
        visibility,
        kepub_size: None,
        details: BookDetails {
            series: metadata.series,
            series_index: metadata.series_index,
            description: metadata.description,
            publisher: metadata.publisher,
            language: metadata.language,
            published_on: metadata.published_on,
            isbn: metadata.isbn,
        },
    };

    match scope.create_book(&new, false).await {
        Ok(()) => {}
        // The same file arrived another way while this was being processed: this
        // user dropped it twice, or someone else shared it.
        Err(error @ (BookError::AlreadyShared | BookError::AlreadyYours)) => {
            return match reuse(scope, &received.sha256, visibility).await? {
                Some(reply) => Ok(reply),
                None => Err(error.into()),
            };
        }
        Err(error) => return Err(error.into()),
    }
    let mut new_dir = new_dir;
    new_dir.committed = true;

    state.conversions.notify_one();
    Ok(UploadReply {
        outcome: Outcome::Added,
        id,
        title,
    })
}

/// The existing book this upload can use instead of storing the file, if any.
///
/// "Just for me" doesn't use someone else's shared book, because they could then
/// take it off this user's Kobo by un-sharing it. Unless this user already added
/// that book, in which case another copy would only show up twice.
async fn reuse(
    scope: &UserScope,
    sha256: &str,
    visibility: Visibility,
) -> Result<Option<UploadReply>, UploadError> {
    let Some(duplicate) = scope.find_duplicate(sha256).await? else {
        return Ok(None);
    };
    let (id, outcome) = match duplicate {
        Duplicate::Own(id) => (id, Outcome::AlreadyYours),
        Duplicate::Shared(id) if scope.library_book(id).await?.is_some() => {
            (id, Outcome::AlreadyYours)
        }
        Duplicate::Shared(id) if visibility == Visibility::Shared => {
            scope.add_to_library(id).await?;
            (id, Outcome::AddedFromShared)
        }
        Duplicate::Shared(_) => return Ok(None),
    };
    let book = scope
        .readable_book(id)
        .await?
        .ok_or(UploadError::Server(AppError::NotFound))?;
    Ok(Some(UploadReply {
        outcome,
        id,
        title: book.title,
    }))
}

/// A sensible title from `My Book - Author.epub`, for a book with none inside.
fn title_from_filename(filename: Option<&str>) -> Option<String> {
    let name = filename?.rsplit(['/', '\\']).next()?;
    let stem = name
        .strip_suffix(".epub")
        .or_else(|| name.strip_suffix(".EPUB"))
        .unwrap_or(name);
    let title: String = stem
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

async fn store_cover(
    state: &AppState,
    book: Uuid,
    cover: &ProcessedCover,
) -> Result<Uuid, std::io::Error> {
    let id = Uuid::new_v4();
    // Thumbnail first, so a cover file on disk implies the thumbnail is there too.
    write_atomic(&state.layout.thumb(book, id), &cover.thumb).await?;
    write_atomic(&state.layout.cover(book, id), &cover.full).await?;
    Ok(id)
}

async fn remove_cover_files(
    layout: &crate::server::storage::layout::Layout,
    book: Uuid,
    cover: Uuid,
) {
    for path in [layout.cover(book, cover), layout.thumb(book, cover)] {
        remove_quietly(&path).await;
    }
}

async fn remove_quietly(path: &FsPath) {
    if let Err(error) = tokio::fs::remove_file(path).await {
        tracing::warn!(%error, "couldn't remove a file; the next start will");
    }
}

/// `POST /books/{id}/cover`: a form with a `file`.
pub async fn replace_cover(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(book): Path<Uuid>,
    headers: HeaderMap,
    form: Multipart,
) -> Result<Response, UploadError> {
    let result = receive_cover(&user, &state, book, form).await;
    if wants_json(&headers) {
        return result;
    }
    match result {
        Ok(_) => Ok(Redirect::to(&format!("/books/{book}?done=cover")).into_response()),
        // Someone with no business here gets the status, not a page about the book.
        Err(UploadError::Server(error)) => Err(error.into()),
        Err(error) => {
            error.log();
            Ok(Redirect::to(&format!("/books/{book}?error={}", error.code())).into_response())
        }
    }
}

async fn receive_cover(
    user: &CurrentUser,
    state: &AppState,
    book: Uuid,
    mut form: Multipart,
) -> Result<Response, UploadError> {
    // First, so that someone who can't change this book can't make the server do
    // any work on an image for it.
    let row = user.scope.editable_book(book).await?;
    check_disk(state)?;

    let mut bytes = None;
    while let Some(mut field) = form.next_field().await.map_err(form_error)? {
        if field.name() != Some("file") || bytes.is_some() {
            return Err(UploadError::BadForm);
        }
        let mut body = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(form_error)? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_COVER_BYTES {
                return Err(UploadError::TooLarge);
            }
        }
        bytes = Some(body);
    }
    let bytes = bytes
        .filter(|body| !body.is_empty())
        .ok_or(UploadError::NoFile)?;

    let cover = work::process_cover(bytes).await.map_err(|error| {
        tracing::debug!(%error, "refused a cover");
        UploadError::NotAnImage
    })?;
    let id = store_cover(state, book, &cover).await?;
    if let Err(error) = user.scope.set_cover(book, id).await {
        remove_cover_files(&state.layout, book, id).await;
        return Err(error.into());
    }
    if let Some(old) = row
        .cover_id
        .as_deref()
        .and_then(|old| Uuid::parse_str(old).ok())
    {
        remove_cover_files(&state.layout, book, old).await;
    }
    Ok(Json(json!({ "cover_id": id })).into_response())
}

/// `POST /books/{id}/delete`: removes a book for everyone holding it. The
/// database goes first, so a crash leaves only files for the next start to clear.
pub async fn delete(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(book): Path<Uuid>,
) -> Result<Redirect, AppError> {
    user.scope.delete_book(book).await?;
    if let Err(error) = tokio::fs::remove_dir_all(state.layout.book_dir(book)).await {
        tracing::warn!(%error, "couldn't remove a deleted book's files; the next start will");
    }
    Ok(Redirect::to("/?done=deleted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_from_filenames() {
        let cases = [
            (Some("My Book.epub"), Some("My Book")),
            (Some("My Book.EPUB"), Some("My Book")),
            (Some("no extension"), Some("no extension")),
            (Some("../../etc/passwd"), Some("passwd")),
            (Some("C:\\Users\\me\\Book.epub"), Some("Book")),
            (Some(".epub"), None),
            (Some("   "), None),
            (None, None),
        ];
        for (input, expected) in cases {
            assert_eq!(title_from_filename(input).as_deref(), expected, "{input:?}");
        }
    }

    #[test]
    fn the_gate_counts_per_user_and_frees_on_drop() {
        let gate = Arc::new(UploadGate::default());
        let first = gate.enter(1).unwrap();
        let _second = gate.enter(1).unwrap();
        assert!(gate.enter(1).is_none());
        let _other_user = gate.enter(2).unwrap();
        drop(first);
        assert!(gate.enter(1).is_some());
    }
}
