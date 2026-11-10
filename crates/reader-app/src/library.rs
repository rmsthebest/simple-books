//! The books on the device, as the home screen and library show them.
//!
//! The platform lists its book files ([`Books`]); this keeps what's needed
//! to show each one in `library.json`, keyed by file name, size and time, so
//! a book is only opened again when its file changes. Opening a book once
//! here also draws its cover small, into a picture file beside it.

use std::io::Cursor;

use book_format::{BlockKind, BookReader, Limits, Series};
use reader_core::Picture;
use serde::{Deserialize, Serialize};

use crate::{Books, Device, book_key, load_json, save_json};

pub(crate) const LIBRARY: &str = "library.json";
/// Covers are kept at most this size: the largest the home screen shows.
pub(crate) const COVER_SIZE: (u32, u32) = (400, 600);

pub(crate) fn cover_file(key: u64) -> String {
    format!("cover-{key:016x}.pic")
}

/// One book file, as last seen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub file: String,
    pub size: u64,
    /// The file's time, in seconds since 1970. Taken as when it was added.
    pub modified: u64,
    /// The book's key, for its saved place and page maps.
    pub key: u64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Series>,
    /// Whether a cover picture was saved.
    #[serde(default)]
    pub cover: bool,
    /// When it was last opened, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<u64>,
    /// How far through it the reader is, from 0 to 1, as of when it was
    /// last left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// The server's id, for a synced book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The server's revision of this file, for a synced book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    /// When the server's library got it, for a synced book, in seconds
    /// since 1970. A file downloaded again doesn't count as newly added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
}

impl Entry {
    /// Where its place is saved. A synced book's stays the same when its
    /// file is made again; any other book's goes with its file.
    pub fn place_key(&self) -> u64 {
        self.id
            .as_ref()
            .map_or(self.key, |id| book_key(format!("server:{id}").as_bytes()))
    }

    /// When it was added, for "added lately".
    fn added_at(&self) -> u64 {
        self.added.unwrap_or(self.modified)
    }

    /// The series and the book's place in it, such as "Earthsea, book 2".
    pub fn series_text(&self) -> Option<String> {
        let series = self.series.as_ref()?;
        Some(match series.index {
            Some(index) => format!("{}, book {index}", series.name),
            None => series.name.clone(),
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Library {
    books: Vec<Entry>,
}

impl Library {
    pub(crate) fn load(device: &mut dyn Device) -> Self {
        load_json(device, LIBRARY).unwrap_or_default()
    }

    pub(crate) fn save(&self, device: &mut dyn Device) {
        save_json(device, LIBRARY, self);
    }

    /// Brings the index up to date with the files there are now: new and
    /// changed files are opened, and books whose files are gone are dropped.
    /// Returns whether anything changed. A file that can't be read as a book
    /// is left out, and tried again next time.
    pub(crate) fn refresh<R>(&mut self, books: &dyn Books<R>, device: &mut dyn Device) -> bool {
        let files = books.list();
        // A file that changed is opened again, keeping what it's been
        // through: a synced book downloaded again is still the same book.
        let (kept, replaced): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut self.books)
            .into_iter()
            .partition(|entry| {
                files.iter().any(|f| {
                    f.name == entry.file && f.size == entry.size && f.modified == entry.modified
                })
            });
        self.books = kept;
        let mut changed = !replaced.is_empty();
        for file in files {
            if self.books.iter().any(|entry| entry.file == file.name) {
                continue;
            }
            let Ok(bytes) = books.read(&file.name) else {
                continue;
            };
            let key = book_key(&bytes);
            let Ok(mut book) = BookReader::open(Cursor::new(bytes), Limits::default()) else {
                continue;
            };
            let cover = cover_picture(&mut book).map(|picture| {
                let small = picture.fitted(COVER_SIZE.0, COVER_SIZE.1);
                device.save(&cover_file(key), &small.to_bytes());
            });
            let manifest = book.manifest();
            let before = replaced.iter().find(|entry| entry.file == file.name);
            self.books.push(Entry {
                key,
                title: manifest.title.clone(),
                authors: manifest.authors.clone(),
                series: manifest.series.clone(),
                cover: cover.is_some(),
                opened: before.and_then(|entry| entry.opened),
                progress: before.and_then(|entry| entry.progress),
                id: before.and_then(|entry| entry.id.clone()),
                revision: None,
                added: before.and_then(|entry| entry.added),
                file: file.name,
                size: file.size,
                modified: file.modified,
            });
            changed = true;
        }
        changed
    }

    pub fn books(&self) -> &[Entry] {
        &self.books
    }

    /// Puts what the server says about each synced book over what its file
    /// says: titles and the rest can be edited on the website. Returns
    /// whether anything changed.
    pub(crate) fn apply_sync(&mut self, state: &reader_sync::State) -> bool {
        let mut changed = false;
        for entry in &mut self.books {
            let Some(synced) = state.book_for_file(&entry.file) else {
                continue;
            };
            let book = &synced.book;
            let revision = book
                .file
                .as_ref()
                .filter(|file| synced.have.as_ref() == Some(&file.sha256))
                .map(|file| file.revision);
            let updated = Entry {
                title: book.title.clone(),
                authors: book.authors.clone(),
                series: book.series.as_ref().map(|series| Series {
                    name: series.name.clone(),
                    index: series.index,
                }),
                id: Some(book.id.clone()),
                // Kept if the file on hand isn't the one the server has now.
                revision: revision.or(entry.revision),
                added: u64::try_from(book.added).ok(),
                ..entry.clone()
            };
            if updated != *entry {
                *entry = updated;
                changed = true;
            }
        }
        changed
    }

    pub fn by_id(&self, id: &str) -> Option<&Entry> {
        self.books
            .iter()
            .find(|entry| entry.id.as_deref() == Some(id))
    }

    pub fn get(&self, file: &str) -> Option<&Entry> {
        self.books.iter().find(|entry| entry.file == file)
    }

    /// Notes that a book was opened at `when`.
    pub(crate) fn opened(&mut self, file: &str, when: u64) {
        if let Some(entry) = self.books.iter_mut().find(|entry| entry.file == file) {
            entry.opened = Some(when);
        }
    }

    pub(crate) fn set_progress(&mut self, file: &str, progress: f32) {
        if let Some(entry) = self.books.iter_mut().find(|entry| entry.file == file) {
            entry.progress = Some(progress.clamp(0.0, 1.0));
        }
    }

    /// The book opened most recently: the one being read.
    pub fn current(&self) -> Option<&Entry> {
        self.books
            .iter()
            .filter(|entry| entry.opened.is_some())
            .max_by_key(|entry| entry.opened)
    }

    /// Opened books, most recent first.
    pub fn recently_opened(&self) -> Vec<&Entry> {
        let mut opened: Vec<&Entry> = self.books.iter().filter(|e| e.opened.is_some()).collect();
        opened.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
        opened
    }

    /// Every book, newest file first.
    pub fn recently_added(&self) -> Vec<&Entry> {
        let mut added: Vec<&Entry> = self.books.iter().collect();
        added.sort_by(|a, b| {
            b.added_at()
                .cmp(&a.added_at())
                .then_with(|| a.title.cmp(&b.title))
        });
        added
    }
}

/// The book's cover: the image the manifest names, or else, for books
/// converted before covers were kept, an image alone at the start.
fn cover_picture(book: &mut BookReader<Cursor<Vec<u8>>>) -> Option<Picture> {
    let index = if let Some(index) = book.manifest().cover {
        index
    } else {
        let first = book.chapter(0).ok()?;
        let [only] = first.blocks.as_slice() else {
            return None;
        };
        let BlockKind::Image { image, .. } = only.kind else {
            return None;
        };
        image
    };
    Picture::decode(&book.image(index as usize).ok()?)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, io, sync::Mutex};

    use book_format::{
        About, Align, Block, Chapter, FormatError, ImageInfo, ImageKind, NewImage, Span,
    };

    use super::*;
    use crate::{Battery, BookFile, Light, Refresh};

    /// A book whose one chapter is a paragraph saying `title`, with a cover
    /// if `cover` is set.
    fn book(title: &str, series: Option<(&str, f64)>, cover: bool) -> Vec<u8> {
        let paragraph = Block {
            kind: BlockKind::Paragraph {
                indent_first: false,
            },
            indent: 0,
            align: Align::Default,
            spans: vec![Span::plain(title.to_owned())],
        };
        let images = if cover {
            let mut png = Vec::new();
            image::GrayImage::from_pixel(60, 90, image::Luma([90]))
                .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            vec![NewImage {
                info: ImageInfo {
                    width: 60,
                    height: 90,
                    kind: ImageKind::Png,
                },
                bytes: png,
            }]
        } else {
            Vec::new()
        };
        let mut out = Cursor::new(Vec::new());
        book_format::write(
            &mut out,
            About {
                title: title.to_owned(),
                authors: vec!["Ann Author".to_owned()],
                series: series.map(|(name, index)| Series {
                    name: name.to_owned(),
                    index: Some(index),
                }),
                cover: cover.then_some(0),
                ..About::default()
            },
            Vec::new(),
            &[Chapter {
                blocks: vec![paragraph],
            }],
            &images,
        )
        .unwrap();
        out.into_inner()
    }

    #[derive(Default)]
    struct Shelf {
        files: Mutex<HashMap<String, (Vec<u8>, u64)>>,
        reads: Mutex<u32>,
    }

    impl Shelf {
        fn put(&self, name: &str, bytes: Vec<u8>, modified: u64) {
            self.files
                .lock()
                .unwrap()
                .insert(name.to_owned(), (bytes, modified));
        }
    }

    impl Books<Cursor<Vec<u8>>> for Shelf {
        fn list(&self) -> Vec<BookFile> {
            let files = self.files.lock().unwrap();
            let mut list: Vec<BookFile> = files
                .iter()
                .map(|(name, (bytes, modified))| BookFile {
                    name: name.clone(),
                    size: bytes.len() as u64,
                    modified: *modified,
                })
                .collect();
            list.sort_by(|a, b| a.name.cmp(&b.name));
            list
        }
        fn read(&self, name: &str) -> io::Result<Vec<u8>> {
            *self.reads.lock().unwrap() += 1;
            let files = self.files.lock().unwrap();
            files
                .get(name)
                .map(|(bytes, _)| bytes.clone())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }
        fn open(&self, name: &str) -> Result<BookReader<Cursor<Vec<u8>>>, FormatError> {
            BookReader::open(Cursor::new(self.read(name)?), Limits::default())
        }
    }

    #[derive(Default)]
    struct Files(HashMap<String, Vec<u8>>);

    impl Device for Files {
        fn show(&mut self, _: &reader_core::Canvas, _: Refresh) {}
        fn battery(&mut self) -> Option<Battery> {
            None
        }
        fn light(&mut self) -> Light {
            Light {
                brightness: 0,
                warmth: 0,
            }
        }
        fn set_light(&mut self, _: Light) {}
        fn suspend(&mut self) -> bool {
            false
        }
        fn load(&mut self, name: &str) -> Option<Vec<u8>> {
            self.0.get(name).cloned()
        }
        fn save(&mut self, name: &str, bytes: &[u8]) {
            self.0.insert(name.to_owned(), bytes.to_vec());
        }
        fn clock(&mut self) -> u64 {
            0
        }
    }

    #[test]
    fn new_books_are_indexed_with_their_details_and_cover() {
        let shelf = Shelf::default();
        shelf.put("road.book", book("The Road", None, true), 100);
        shelf.put("dune.book", book("Dune", Some(("Dune", 1.0)), false), 200);
        let mut device = Files::default();
        let mut library = Library::default();
        assert!(library.refresh(&shelf, &mut device));

        let road = library.get("road.book").unwrap();
        assert_eq!(road.title, "The Road");
        assert_eq!(road.authors, ["Ann Author"]);
        assert_eq!(road.key, book_key(&book("The Road", None, true)));
        assert!(road.cover);
        let picture = Picture::from_bytes(&device.0[&cover_file(road.key)]).unwrap();
        assert_eq!((picture.width(), picture.height()), (60, 90));

        let dune = library.get("dune.book").unwrap();
        assert_eq!(dune.series.as_ref().map(|s| s.name.as_str()), Some("Dune"));
        assert!(!dune.cover);
        let added: Vec<&str> = library
            .recently_added()
            .iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(added, ["Dune", "The Road"]);
    }

    #[test]
    fn only_new_or_changed_files_are_read_again() {
        let shelf = Shelf::default();
        shelf.put("a.book", book("A", None, false), 1);
        shelf.put("b.book", book("B", None, false), 1);
        let mut device = Files::default();
        let mut library = Library::default();
        library.refresh(&shelf, &mut device);
        library.opened("a.book", 50);
        library.save(&mut device);
        assert_eq!(*shelf.reads.lock().unwrap(), 2);

        // Loaded again, as after a restart: nothing to read.
        let mut library = Library::load(&mut device);
        assert!(!library.refresh(&shelf, &mut device));
        assert_eq!(*shelf.reads.lock().unwrap(), 2);
        assert_eq!(library.current().map(|e| e.file.as_str()), Some("a.book"));

        // B replaced, C added, A removed.
        shelf.put("b.book", book("B, second edition", None, false), 2);
        shelf.put("c.book", book("C", None, false), 3);
        shelf.files.lock().unwrap().remove("a.book");
        assert!(library.refresh(&shelf, &mut device));
        assert_eq!(*shelf.reads.lock().unwrap(), 4);
        assert_eq!(library.get("b.book").unwrap().title, "B, second edition");
        assert!(library.get("a.book").is_none());
        assert_eq!(library.current(), None);
    }

    #[test]
    fn synced_books_show_the_servers_details_and_keep_their_history() {
        let id = "0b9e6c1e-6e0e-4c1a-9d43-1b1f3a7e2a10";
        let file = format!("{id}.book");
        let shelf = Shelf::default();
        let first = book("Title in the file", None, false);
        shelf.put(&file, first.clone(), 1);
        shelf.put("own.book", book("Mine", None, false), 1);
        let mut device = Files::default();
        let mut library = Library::default();
        library.refresh(&shelf, &mut device);

        let synced = |sha256: &str, revision: i64| reader_sync::State {
            books: [(
                id.to_owned(),
                reader_sync::Synced {
                    book: reader_sync::Book {
                        id: id.to_owned(),
                        title: "Title on the website".to_owned(),
                        authors: vec!["Someone".to_owned()],
                        series: Some(sync_proto::Series {
                            name: "Series".to_owned(),
                            index: Some(2.0),
                        }),
                        file: Some(sync_proto::BookFile {
                            size: 1,
                            revision,
                            sha256: sha256.to_owned(),
                        }),
                        cover: None,
                        added: 500,
                    },
                    have: Some("a".to_owned()),
                },
            )]
            .into(),
            ..reader_sync::State::default()
        };
        assert!(library.apply_sync(&synced("a", 7)));
        assert!(!library.apply_sync(&synced("a", 7)), "once is enough");
        let entry = library.get(&file).unwrap().clone();
        assert_eq!(entry.title, "Title on the website");
        assert_eq!(entry.authors, ["Someone"]);
        assert_eq!(entry.series_text().as_deref(), Some("Series, book 2"));
        assert_eq!(entry.id.as_deref(), Some(id));
        assert_eq!(entry.revision, Some(7));
        assert_eq!(entry.added, Some(500));
        assert_eq!(
            library.by_id(id).map(|e| e.file.as_str()),
            Some(file.as_str())
        );
        assert_eq!(library.get("own.book").unwrap().id, None, "not synced");
        assert_ne!(entry.place_key(), entry.key);

        // Made again on the server and downloaded: a new file, the same book.
        library.opened(&file, 40);
        library.set_progress(&file, 0.5);
        shelf.put(
            &file,
            book("Title in the file, made better", None, false),
            2,
        );
        assert!(library.refresh(&shelf, &mut device));
        let again = library.get(&file).unwrap();
        assert_ne!(again.key, entry.key, "its own page maps");
        assert_eq!(again.place_key(), entry.place_key(), "the same place");
        assert_eq!(again.opened, Some(40));
        assert_eq!(again.progress, Some(0.5));
        assert_eq!(again.revision, None, "not known until the sync says");
        library.apply_sync(&synced("a", 8));
        assert_eq!(library.get(&file).unwrap().revision, Some(8));

        // Not yet the server's newest file: the revision on hand stays.
        library.apply_sync(&synced("b", 9));
        assert_eq!(library.get(&file).unwrap().revision, Some(8));
    }

    #[test]
    fn a_file_that_isnt_a_book_is_left_out_and_tried_again() {
        let shelf = Shelf::default();
        shelf.put("broken.book", b"not a book".to_vec(), 1);
        let mut device = Files::default();
        let mut library = Library::default();
        library.refresh(&shelf, &mut device);
        assert_eq!(library.books(), []);
        library.refresh(&shelf, &mut device);
        assert_eq!(*shelf.reads.lock().unwrap(), 2);
    }

    #[test]
    fn recently_opened_comes_most_recent_first() {
        let shelf = Shelf::default();
        for name in ["a", "b", "c"] {
            shelf.put(&format!("{name}.book"), book(name, None, false), 1);
        }
        let mut device = Files::default();
        let mut library = Library::default();
        library.refresh(&shelf, &mut device);
        library.opened("b.book", 10);
        library.opened("c.book", 30);
        library.opened("a.book", 20);
        library.set_progress("c.book", 1.5);
        let opened: Vec<&str> = library
            .recently_opened()
            .iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(opened, ["c", "a", "b"]);
        assert_eq!(library.current().unwrap().progress, Some(1.0));
    }
}
