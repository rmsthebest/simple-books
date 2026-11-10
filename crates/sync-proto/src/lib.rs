//! What the simple-books reader and server say to each other, as JSON under
//! [`PREFIX`]. The Kobo's own store protocol is separate, and unchanged.
//!
//! A reader pairs once: it asks for a code ([`PairStart`]), shows it, and
//! someone signed in to the website types it on the device page. From then on
//! the reader sends its secret as `Authorization: Bearer <secret>` on every
//! request. The secret never goes in a path, so it stays out of logs.
//!
//! A sync sends the places that changed (`PUT books/{id}/place`), then reads
//! [`Changes`] and downloads the files it lists (`GET books/{id}`).

use serde::{Deserialize, Serialize};

/// Where the protocol lives on the server.
pub const PREFIX: &str = "/device/v1";

/// The longest device name kept. Longer ones are cut.
pub const MAX_NAME: usize = 64;

/// `POST pair`: a reader asking to be paired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    /// What the website calls it, such as "Kobo Clara BW".
    pub name: String,
}

/// The answer to [`PairRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStart {
    /// The reader's secret from now on, once the code is entered. Shown to
    /// no one.
    pub secret: String,
    /// What the person types, such as `ABCD-EFGH`.
    pub code: String,
    /// The page to type it on.
    pub url: String,
    /// Seconds until the code stops working.
    pub expires_in: u32,
    /// Seconds to wait between asking whether it's done.
    pub interval: u32,
}

/// `GET pair`, with the secret: whether the code has been entered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum PairStatus {
    /// Not yet.
    Pending,
    /// Done: the secret now works, for this user's library.
    Paired { user: String },
    /// The code ran out, or the device was removed. Start again.
    Expired,
}

/// `GET changes?since=N`: what's changed in the library since sequence `N`,
/// a page at a time. Start from 0. Ask again from [`Changes::next`] while
/// [`Changes::more`] is set; the last page's `next` is where to start the
/// next sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    /// Books added or changed, with their files' details.
    pub books: Vec<Book>,
    /// Books no longer in the library, by id. Ones the reader never had may
    /// be among them.
    pub removed: Vec<String>,
    /// Where readers are in the books, newest per book.
    pub places: Vec<Place>,
    pub next: i64,
    pub more: bool,
    /// This run started from nothing, because it was asked to or the server
    /// wanted everything sent again. Once its last page is in, any synced
    /// book it didn't list is gone.
    pub full: bool,
}

/// A book as the library has it. The title and the rest can be edited on the
/// website, so they can differ from what's in the book's file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Series>,
    /// The book in the reader's format, once it's been made. A book that
    /// couldn't be converted never has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<BookFile>,
    /// Changes when the cover does; `GET books/{id}/cover` has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    /// When it was added to the library, in seconds since 1970.
    pub added: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<f64>,
}

/// `GET books/{id}` sends this file, with its revision in [`REVISION_HEADER`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFile {
    pub size: u64,
    /// Changes whenever the file is made again. A place taken in another
    /// revision may not point at the same words.
    pub revision: i64,
    /// The file's SHA-256 in lower-case hex. A download that doesn't match
    /// isn't used.
    pub sha256: String,
}

/// The header a book download gives its file's revision in.
pub const REVISION_HEADER: &str = "x-book-revision";

/// Where a reader is in a book.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub chapter: u32,
    pub block: u32,
    /// Characters into the block.
    #[serde(default)]
    pub offset: u32,
    /// The [`BookFile::revision`] it was taken in.
    pub revision: i64,
    /// How far through, from 0 to 100.
    pub percent: f32,
    pub status: Status,
    /// The reader's clock when it was there, in seconds since 1970. The
    /// newest place wins.
    pub at: i64,
}

/// A [`Spot`] in a given book, as [`Changes`] lists them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub book: String,
    #[serde(flatten)]
    pub spot: Spot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Unread,
    Reading,
    Finished,
}

/// The answer to `PUT books/{id}/place`, whose body is a [`Spot`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlaceAnswer {
    /// Whether it was kept: `false` if a newer one was already there.
    pub kept: bool,
    /// The place the server has now.
    pub place: Spot,
}

/// Any request that fails says why, with a status code to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_are_flat() {
        let place = Place {
            book: "b".into(),
            spot: Spot {
                chapter: 1,
                block: 2,
                offset: 3,
                revision: 4,
                percent: 50.0,
                status: Status::Reading,
                at: 5,
            },
        };
        let json = serde_json::to_value(&place).unwrap();
        assert_eq!(json["book"], "b");
        assert_eq!(json["status"], "reading");
        assert_eq!(serde_json::from_value::<Place>(json).unwrap(), place);
    }

    #[test]
    fn statuses_read_as_tagged_objects() {
        let paired = PairStatus::Paired { user: "sam".into() };
        let json = serde_json::to_string(&paired).unwrap();
        assert_eq!(json, r#"{"state":"paired","user":"sam"}"#);
        assert_eq!(serde_json::from_str::<PairStatus>(&json).unwrap(), paired);
        assert_eq!(
            serde_json::from_str::<PairStatus>(r#"{"state":"pending"}"#).unwrap(),
            PairStatus::Pending
        );
    }
}
