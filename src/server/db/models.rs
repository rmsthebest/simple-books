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
    /// On the user's readers, as far as the library says; false when it's
    /// not in their library at all.
    pub on_device: bool,
    /// `unread`, `reading` or `finished`: see [`Reading`].
    pub read_status: String,
    pub read_percent: Option<f64>,
}

impl BookCard {
    pub fn authors(&self) -> Vec<String> {
        serde_json::from_str(&self.authors_json).unwrap_or_default()
    }

    pub fn reading(&self) -> Reading {
        Reading::from_row(&self.read_status, self.read_percent)
    }
}

/// How far a user is through a book, from the reader or Nickel, whichever
/// was there last.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    Unread,
    /// The percent read, from 0 to 100.
    Reading(f64),
    Finished,
}

impl Reading {
    /// From what the library queries say: a status of `unread`, `reading` or
    /// `finished`, and a percent.
    pub fn from_row(status: &str, percent: Option<f64>) -> Self {
        match status {
            "finished" => Self::Finished,
            "reading" => Self::Reading(percent.unwrap_or(0.0).clamp(0.0, 100.0)),
            _ => Self::Unread,
        }
    }

    /// As the website puts it: "Finished", "Reading · 45%", or nothing.
    pub fn shown(self) -> Option<String> {
        match self {
            Self::Unread => None,
            Self::Reading(percent) => Some(format!("Reading · {}%", percent.round() as i64)),
            Self::Finished => Some("Finished".to_owned()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_reads_as_the_website_says_it() {
        assert_eq!(Reading::from_row("unread", Some(3.0)).shown(), None);
        assert_eq!(
            Reading::from_row("reading", Some(44.6)).shown().as_deref(),
            Some("Reading · 45%")
        );
        assert_eq!(
            Reading::from_row("reading", None),
            Reading::Reading(0.0),
            "Nickel may not say how far"
        );
        assert_eq!(
            Reading::from_row("finished", Some(99.0)).shown().as_deref(),
            Some("Finished")
        );
    }
}
