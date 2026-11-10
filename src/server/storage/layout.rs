//! Where files live. Paths are built only from validated UUIDs, never from
//! user input, so path traversal isn't expressible here.

use std::path::{Path, PathBuf};

use uuid::Uuid;

pub const READER_FILE: &str = "reader.book";

#[derive(Clone, Debug)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> PathBuf {
        self.root.join("db.sqlite")
    }

    pub fn books_dir(&self) -> PathBuf {
        self.root.join("books")
    }

    /// Scratch space, wiped at every start.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn book_dir(&self, book: Uuid) -> PathBuf {
        self.books_dir().join(book.to_string())
    }

    pub fn original(&self, book: Uuid) -> PathBuf {
        self.book_dir(book).join("original.epub")
    }

    pub fn kepub(&self, book: Uuid) -> PathBuf {
        self.book_dir(book).join("kobo.kepub.epub")
    }

    /// The book in the simple-books reader's own format.
    pub fn reader(&self, book: Uuid) -> PathBuf {
        self.book_dir(book).join(READER_FILE)
    }

    pub fn cover(&self, book: Uuid, cover: Uuid) -> PathBuf {
        self.book_dir(book).join(format!("cover-{cover}.jpg"))
    }

    pub fn thumb(&self, book: Uuid, cover: Uuid) -> PathBuf {
        self.book_dir(book).join(format!("thumb-{cover}.jpg"))
    }
}
