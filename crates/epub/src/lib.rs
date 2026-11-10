//! Pure EPUB handling. This crate deliberately has no filesystem, database or
//! async access: it takes readers and returns data, which keeps it cheap to
//! fuzz and makes it a capability boundary for untrusted input.
//!
//! Every limit applies to bytes actually produced, not to what a file's headers
//! claim. Nothing is ever extracted to disk.
#![forbid(unsafe_code)]

mod archive;
pub mod isbn;
mod opf;
mod text;
mod xml;

use std::io::{Read, Seek};

pub use opf::Metadata;

/// What a file may cost to read.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_entries: u64,
    /// Decompressed bytes in any one entry.
    pub max_entry_bytes: u64,
    /// Decompressed bytes in the whole archive.
    pub max_total_bytes: u64,
    /// Decompressed bytes in an XML document this crate parses.
    pub max_xml_bytes: u64,
    /// Decompressed bytes in the cover image.
    pub max_cover_bytes: u64,
    /// Largest allowed expansion of an entry. Book content compresses perhaps 10
    /// times over; only a crafted file gets near deflate's limit of about 1000.
    pub max_ratio: u64,
    /// Entries smaller than this are exempt from the ratio check, because a tiny
    /// file of repeated bytes can legitimately exceed it.
    pub ratio_floor_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_entries: 20_000,
            max_entry_bytes: 256 * MIB,
            max_total_bytes: 2048 * MIB,
            max_xml_bytes: 4 * MIB,
            max_cover_bytes: 32 * MIB,
            max_ratio: 200,
            ratio_floor_bytes: MIB,
        }
    }
}

/// Why a file was refused. The messages are shown to the person uploading it.
#[derive(Debug, thiserror::Error)]
pub enum EpubError {
    #[error("this isn't an EPUB file")]
    NotAnArchive,
    #[error("this EPUB is damaged")]
    Corrupt,
    #[error("this EPUB has DRM, so it can't be read here")]
    Drm,
    #[error("this EPUB has no book inside it")]
    NoContent,
    #[error("this EPUB is too large or too complex to read safely")]
    TooLarge,
    #[error("this EPUB contains something unsafe: {0}")]
    Unsafe(&'static str),
    #[error("couldn't read the file: {0}")]
    Io(#[source] std::io::Error),
}

/// A cover image's bytes, not yet checked to be an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverBytes(pub Vec<u8>);

/// What was learned from a file that passed every check.
#[derive(Debug, Clone)]
pub struct Book {
    pub metadata: Metadata,
    pub cover: Option<CoverBytes>,
}

/// Validates a whole EPUB and reads its metadata and cover.
///
/// # Errors
///
/// Any [`EpubError`], if the file breaks a limit, isn't safe, or can't be read.
pub fn parse<R: Read + Seek>(reader: R, limits: &Limits) -> Result<Book, EpubError> {
    let mut archive = archive::Archive::open(reader, limits)?;
    archive.verify_contents()?;
    opf::read_book(&mut archive)
}

/// A book opened to read what's inside, after the same checks as [`parse`].
/// Every name it gives is an entry in the archive.
pub struct Contents<'l, R> {
    archive: archive::Archive<'l, R>,
    package: opf::Opened,
}

impl<R> std::fmt::Debug for Contents<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contents")
            .field("spine", &self.package.spine)
            .finish_non_exhaustive()
    }
}

impl<'l, R: Read + Seek> Contents<'l, R> {
    /// Opens and checks a whole EPUB.
    ///
    /// # Errors
    ///
    /// Any [`EpubError`], as for [`parse`].
    pub fn open(reader: R, limits: &'l Limits) -> Result<Self, EpubError> {
        let mut archive = archive::Archive::open(reader, limits)?;
        archive.verify_contents()?;
        let package = opf::open_package(&mut archive)?;
        Ok(Self { archive, package })
    }

    pub fn metadata(&self) -> &Metadata {
        &self.package.metadata
    }

    /// The documents to read, in order. Never empty.
    pub fn spine(&self) -> &[String] {
        &self.package.spine
    }

    /// The EPUB 3 navigation document.
    pub fn nav(&self) -> Option<&str> {
        self.package.nav.as_deref()
    }

    /// The EPUB 2 table of contents.
    pub fn ncx(&self) -> Option<&str> {
        self.package.ncx.as_deref()
    }

    /// The cover image, if the book names one.
    pub fn cover(&self) -> Option<&str> {
        self.package.cover.as_deref()
    }

    /// The media type the manifest gives an entry.
    pub fn media_type(&self, name: &str) -> Option<&str> {
        self.package.media_types.get(name).map(String::as_str)
    }

    /// Reads one entry, up to `limit` bytes. `None` if there's no such entry.
    ///
    /// # Errors
    ///
    /// [`EpubError::TooLarge`] past the limit, or [`EpubError::Corrupt`].
    pub fn read(&mut self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, EpubError> {
        self.archive.read(name, limit)
    }
}

/// Where a link in the document `from` points, as an entry name, without any
/// `#fragment`. A link to a fragment alone points at `from` itself. `None` for
/// a link that leaves the book.
pub fn resolve(from: &str, href: &str) -> Option<String> {
    let path = href.split(['#', '?']).next().unwrap_or_default();
    if path.is_empty() {
        return Some(from.to_owned());
    }
    let base = from.rsplit_once('/').map_or("", |(directory, _)| directory);
    opf::resolve_href(base, path)
}

/// Turns a document's bytes into text, following its byte order mark.
pub fn decode(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    xml::decode(bytes)
}
