//! The book that's open: its reader, its file, and the rest of it being
//! laid out on another thread.

use std::{
    io::{Read, Seek},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use book_format::{BookReader, FormatError, Position};
use reader_core::{PageMap, Reader, Settings, paginate_book};

use crate::{
    AppError, Books, Device, Entry, load_json,
    place::{START, SavedPlace},
    save_json,
};

/// Opens the book afresh, for the reader and for background pagination.
type Opener<R> = Arc<dyn Fn() -> Result<BookReader<R>, FormatError> + Send + Sync>;

/// The file a book's place is saved in, by [`Entry::place_key`].
pub(crate) fn position_file(place_key: u64) -> String {
    format!("position-{place_key:016x}.json")
}

pub(crate) fn pages_file(key: u64) -> String {
    format!("pages-{key:016x}.json")
}

/// The rest of the book being paginated on another thread.
struct Background {
    receive: mpsc::Receiver<PageMap>,
    stop: Arc<AtomicBool>,
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct OpenBook<R> {
    pub file: String,
    pub key: u64,
    /// Where its place is saved: the same across files for a synced book.
    place_key: u64,
    /// The server's revision of the file, for a synced book.
    revision: Option<i64>,
    open: Opener<R>,
    pub reader: Reader<R>,
    background: Option<Background>,
    /// Where the page started when the layout was first changed since the
    /// last turn. Each change puts the line holding this at the top of the
    /// page. That line can start a few words earlier, so starting from the
    /// new page each time would creep back a line per step.
    anchor: Option<Position>,
}

impl<R> std::fmt::Debug for OpenBook<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenBook")
            .field("file", &self.file)
            .field("reader", &self.reader)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek + Send + 'static> OpenBook<R> {
    /// Opens the book in `entry` where it was left.
    pub(crate) fn open(
        books: &Arc<dyn Books<R>>,
        entry: &Entry,
        settings: &Settings,
        device: &mut dyn Device,
    ) -> Result<Self, AppError> {
        let books = Arc::clone(books);
        let name = entry.file.clone();
        let open: Opener<R> = Arc::new(move || books.open(&name));
        let key = entry.key;
        let at = match load_json::<SavedPlace>(device, &position_file(entry.place_key())) {
            Some(saved) => saved.resolve(&mut open()?, key, entry.revision),
            None => START,
        };
        let reader = open_reader(&open, key, settings, at, false, device)?;
        Ok(Self {
            file: entry.file.clone(),
            key,
            place_key: entry.place_key(),
            revision: entry.revision,
            open,
            reader,
            background: None,
            anchor: None,
        })
    }

    /// Lays the book out again with new settings, keeping the reader's
    /// first line at the top of the page.
    pub(crate) fn relayout(
        &mut self,
        settings: &Settings,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let at = self
            .anchor
            .or_else(|| self.reader.position())
            .unwrap_or(START);
        self.anchor = Some(at);
        self.background = None;
        self.reader = open_reader(&self.open, self.key, settings, at, true, device)?;
        Ok(())
    }

    /// The reader moved on by itself, so a later layout change starts from
    /// the new page.
    pub(crate) fn moved(&mut self) {
        self.anchor = None;
    }

    /// Saves the place, with the time it was reached, unless it's where
    /// it was: a book opened and left alone mustn't look freshly read, or
    /// its old place would win over a newer one from another reader.
    pub(crate) fn save(&self, device: &mut dyn Device) {
        let Some(position) = self.reader.position() else {
            return;
        };
        let name = position_file(self.place_key);
        let saved: Option<SavedPlace> = load_json(device, &name);
        if saved
            .is_some_and(|saved| saved.position == position && saved.fits(self.key, self.revision))
        {
            return;
        }
        let place = SavedPlace {
            position,
            file: self.key,
            revision: self.revision,
            progress: self.progress().unwrap_or(0.0),
            at: device.clock(),
        };
        save_json(device, &name, &place);
    }

    /// How far through the book the current page ends, from 0 to 1: the
    /// chapters before it by length, and its share of its own chapter.
    pub(crate) fn progress(&self) -> Option<f32> {
        let page = self.reader.current()?;
        let chapters = &self.reader.manifest().chapters;
        let total: u64 = chapters.iter().map(|c| c.chars).sum();
        if total == 0 {
            return None;
        }
        let index = page.chapter as usize;
        let before: u64 = chapters[..index].iter().map(|c| c.chars).sum();
        let within = chapters[index].chars as f32 * self.reader.through_chapter()?;
        Some(((before as f32 + within) / total as f32).clamp(0.0, 1.0))
    }

    pub(crate) fn paginating(&self) -> bool {
        self.background.is_some()
    }

    /// Starts laying out the rest of the book on another thread, if it isn't
    /// all laid out and that isn't already under way. Only once the screen
    /// is drawn: the Clara BW has one core, and the thread would take half
    /// of it from drawing the page.
    pub(crate) fn paginate_in_background(&mut self) {
        if self.background.is_some() || self.reader.page_map().is_complete() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut map = self.reader.page_map().clone();
        let open = Arc::clone(&self.open);
        let thread_stop = Arc::clone(&stop);
        let settings = self.reader.settings().clone();
        let keep = self.reader.keeping();
        std::thread::spawn(move || {
            let Ok(mut book) = open() else {
                return;
            };
            let result = paginate_book(&mut book, settings, &mut map, keep, || {
                thread_stop.load(Ordering::Relaxed)
            });
            if result.is_ok() && map.is_complete() {
                let _ = send.send(map);
            }
        });
        self.background = Some(Background { receive, stop });
    }

    /// Takes the whole book's page map once the thread has finished, and
    /// saves it for next time.
    /// Takes the page map from the background, if it's done. True if it
    /// was, so the page number can be shown.
    pub(crate) fn collect(&mut self, device: &mut dyn Device) -> bool {
        if let Some(background) = &self.background
            && let Ok(map) = background.receive.try_recv()
        {
            self.reader.adopt(&map);
            let key = PageMap::key(self.key, self.reader.settings());
            save_json(device, &pages_file(key), &map);
            self.background = None;
            return true;
        }
        false
    }
}

/// Opens the reader with its saved page map. With `relayout`, the settings
/// have just changed, and the line at `at` stays at the top of its page;
/// see [`Reader::relayout`].
fn open_reader<R: Read + Seek + Send + 'static>(
    open: &Opener<R>,
    book_key: u64,
    settings: &Settings,
    at: Position,
    relayout: bool,
    device: &mut dyn Device,
) -> Result<Reader<R>, AppError> {
    let key = PageMap::key(book_key, settings);
    let cached: Option<PageMap> = load_json(device, &pages_file(key));
    let reader = if relayout {
        Reader::relayout(open()?, settings.clone(), book_key, at, cached)?
    } else {
        Reader::open(open()?, settings.clone(), book_key, at, cached)?
    };
    if relayout && reader.page_map().is_complete() {
        // There's no background pagination to save it, and the page at `at`
        // has moved from where a saved map has it.
        save_json(device, &pages_file(key), reader.page_map());
    }
    Ok(reader)
}
