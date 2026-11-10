//! Books kept as `.book` files in a folder, as on the Kobo and in the
//! emulator.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use book_format::{BookReader, FormatError, Limits};

use crate::{BookFile, Books};

#[derive(Debug, Clone)]
pub struct Folder {
    dir: PathBuf,
}

impl Folder {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The file for `name`, if it's a plain name in this folder: a name
    /// with a path in it is refused, so nothing outside can be opened.
    fn path(&self, name: &str) -> io::Result<PathBuf> {
        let plain = Path::new(name).file_name().is_some_and(|n| n == name);
        if !plain {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a book in the folder: {name}"),
            ));
        }
        Ok(self.dir.join(name))
    }
}

impl Books<File> for Folder {
    /// The `.book` files, by name. A folder that can't be read has none.
    fn list(&self) -> Vec<BookFile> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut books: Vec<BookFile> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let metadata = entry.metadata().ok()?;
                let is_book = Path::new(&name).extension().is_some_and(|e| e == "book");
                if !is_book || !metadata.is_file() {
                    return None;
                }
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_secs());
                Some(BookFile {
                    name,
                    size: metadata.len(),
                    modified,
                })
            })
            .collect();
        books.sort_by(|a, b| a.name.cmp(&b.name));
        books
    }

    fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path(name)?)
    }

    fn open(&self, name: &str) -> Result<BookReader<File>, FormatError> {
        BookReader::open(File::open(self.path(name)?)?, Limits::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_in_the_folder() {
        let folder = Folder::new("/books");
        assert_eq!(folder.path("a.book").unwrap(), Path::new("/books/a.book"));
        for name in ["../a.book", "sub/a.book", "/etc/passwd", "", ".."] {
            assert!(folder.path(name).is_err(), "{name}");
        }
    }
}
