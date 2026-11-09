//! Row types and small domain enums.

use uuid::Uuid;

/// Who a book is available to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Only the uploader.
    Private,
    /// Anyone can see it and add it to their own library.
    Shared,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookRow {
    pub id: String,
    pub owner_id: i64,
    pub visibility: String,
    pub title: String,
    pub authors_json: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    pub published_on: Option<String>,
    pub isbn: Option<String>,
    pub cover_id: Option<String>,
    pub original_size: i64,
    pub kepub_state: String,
    pub kepub_size: Option<i64>,
    /// From the reader's library entry, if they have one.
    pub first_seq: Option<i64>,
    pub change_seq: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl BookRow {
    /// The KEPUB's size, if a converted copy is ready to serve.
    pub fn ready_kepub_size(&self) -> Option<i64> {
        if self.kepub_state == "ready" {
            self.kepub_size
        } else {
            None
        }
    }

    pub fn authors(&self) -> Vec<String> {
        serde_json::from_str(&self.authors_json).unwrap_or_default()
    }

    /// Whether to offer this user the editing controls. The rule that counts is the
    /// one each change enforces; a test keeps the two in step.
    pub fn editable_by(&self, user_id: i64, is_admin: bool) -> bool {
        self.owner_id == user_id || (self.visibility == "shared" && is_admin)
    }

    /// Stored IDs are always UUIDs; anything else means a corrupt row.
    pub fn uuid(&self) -> Result<Uuid, uuid::Error> {
        Uuid::parse_str(&self.id)
    }
}

#[derive(Debug, Clone)]
pub struct ReadingStateRow {
    pub state_json: String,
    pub last_modified: i64,
}

/// One entry of a sync page. `seq` values are unique per user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncRowKind {
    Book,
    Tombstone,
    ReadingState,
}

#[derive(Debug, Clone)]
pub struct SyncRow {
    pub kind: SyncRowKind,
    pub id: String,
    pub seq: i64,
}

/// Values needed to create a book row.
#[derive(Debug, Clone)]
pub struct NewBook {
    pub id: Uuid,
    pub title: String,
    pub authors: Vec<String>,
    pub cover_id: Option<Uuid>,
    pub original_sha256: String,
    pub original_size: i64,
    pub visibility: Visibility,
    pub kepub_size: Option<i64>,
    pub details: BookDetails,
}

/// What the book page's form changes.
#[derive(Debug, Clone)]
pub struct MetadataEdit {
    pub title: String,
    pub authors: Vec<String>,
    pub details: BookDetails,
}

/// A book in a grid. `shared_by` is whoever uploaded it, shown only for shared books.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookCard {
    pub id: String,
    pub owner_id: i64,
    pub visibility: String,
    pub title: String,
    pub authors_json: String,
    pub cover_id: Option<String>,
    pub kepub_state: String,
    pub shared_by: String,
    pub in_library: bool,
}

impl BookCard {
    pub fn authors(&self) -> Vec<String> {
        serde_json::from_str(&self.authors_json).unwrap_or_default()
    }
}

/// The metadata beyond title and authors. All of it is optional.
#[derive(Debug, Clone, Default)]
pub struct BookDetails {
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    pub published_on: Option<String>,
    /// As an ISBN-13.
    pub isbn: Option<String>,
}
