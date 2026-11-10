//! An open book: where the reader is, the pages around it, and the chapters
//! paginated so far.
//!
//! The last three pages drawn are kept, so after [`Reader::prepare`] the next
//! and previous pages are both ready. Turning back never lays out again.
//!
//! Each drawn page keeps where its links are. [`Reader::link_at`] finds the
//! one a tap meant, and [`Reader::follow`] remembers where the reader was, so
//! [`Reader::back`] can return from a footnote.

use std::io::{Read, Seek};

use book_format::{BookReader, Chapter, FormatError, ImageInfo, Manifest, Position};

use crate::{
    Canvas, PageMap, PageRef, PageStart, Settings, Typesetter,
    paginate::{draw_page, paginate},
    typeset::{ChapterRef, Page},
};

/// Drawn pages kept: the current one and one each side.
const PAGES_KEPT: usize = 3;
/// Chapters kept parsed, for turning across a chapter's end and back.
const CHAPTERS_KEPT: usize = 2;
/// A tap this close to a link follows it, in millimetres: a fingertip's
/// accuracy, and a footnote's superscript number is smaller than that.
const REACH_MM: f32 = 3.0;
/// Followed links remembered for [`Reader::back`].
const HISTORY_KEPT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    Next,
    Previous,
}

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error(transparent)]
    Book(#[from] FormatError),
}

pub struct Reader<R> {
    book: BookReader<R>,
    images: Vec<ImageInfo>,
    typesetter: Typesetter,
    map: PageMap,
    /// Most recently used last.
    chapters: Vec<(u32, Chapter)>,
    /// Most recently used last.
    pages: Vec<(PageRef, Page)>,
    /// `None` only when the book has no pages at all.
    current: Option<PageRef>,
    /// Where each followed link was followed from. Most recent last.
    history: Vec<Position>,
}

impl<R> std::fmt::Debug for Reader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("current", &self.current)
            .field("pages_drawn", &self.pages.len())
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> Reader<R> {
    /// Opens a book at `at`. `book_key` identifies this version of the book
    /// for [`PageMap::key`]; `cached` is a page map saved earlier, used if its
    /// key still matches.
    pub fn open(
        book: BookReader<R>,
        settings: Settings,
        book_key: u64,
        at: Position,
        cached: Option<PageMap>,
    ) -> Result<Self, ReaderError> {
        let manifest = book.manifest();
        let key = PageMap::key(book_key, &settings);
        let chapters = manifest.chapters.len();
        let map = cached
            .filter(|map| map.key == key && map.chapters.len() == chapters)
            .unwrap_or_else(|| PageMap::new(key, chapters));
        let typesetter = Typesetter::new(settings, manifest.language.as_deref());
        let images = manifest.images.clone();
        let mut reader = Self {
            book,
            images,
            typesetter,
            map,
            chapters: Vec::new(),
            pages: Vec::new(),
            current: None,
            history: Vec::new(),
        };
        reader.go_to(at)?;
        Ok(reader)
    }

    pub fn manifest(&self) -> &Manifest {
        self.book.manifest()
    }

    pub fn settings(&self) -> &Settings {
        self.typesetter.settings()
    }

    /// The pages found so far, to save for next time.
    pub fn page_map(&self) -> &PageMap {
        &self.map
    }

    /// Takes in chapters paginated elsewhere, such as by [`paginate_book`] on
    /// another thread. A map for other settings is ignored.
    pub fn adopt(&mut self, map: &PageMap) {
        if map.key != self.map.key {
            return;
        }
        for (mine, theirs) in self.map.chapters.iter_mut().zip(&map.chapters) {
            if mine.is_none() {
                mine.clone_from(theirs);
            }
        }
    }

    pub fn current(&self) -> Option<PageRef> {
        self.current
    }

    /// Where the current page starts. This is what to save and sync.
    pub fn position(&self) -> Option<Position> {
        self.map.position(self.current?)
    }

    /// Moves to the page that shows `position`. Past the end of a chapter, or
    /// in a chapter with nothing on it, that's the next page there is.
    pub fn go_to(&mut self, position: Position) -> Result<(), ReaderError> {
        let chapters = self.map.chapters.len() as u32;
        if chapters == 0 {
            self.current = None;
            return Ok(());
        }
        let chapter = position.chapter.min(chapters - 1);
        let position = Position {
            chapter,
            ..position
        };
        self.paginated(chapter)?;
        self.current = match self.map.locate(position) {
            Some(page) => Some(page),
            None => match self.nearest(chapter, Turn::Next, true)? {
                Some(page) => Some(page),
                None => self.nearest(chapter, Turn::Previous, true)?,
            },
        };
        Ok(())
    }

    /// The current page, drawn. `None` if the book has no pages.
    pub fn page(&mut self) -> Result<Option<&Canvas>, ReaderError> {
        let Some(current) = self.current else {
            return Ok(None);
        };
        self.draw(current)?;
        Ok(self.cached(current))
    }

    /// Turns a page. Returns false at either end of the book.
    pub fn turn(&mut self, turn: Turn) -> Result<bool, ReaderError> {
        let Some(current) = self.current else {
            return Ok(false);
        };
        match self.step(current, turn)? {
            Some(page) => {
                self.current = Some(page);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Draws the pages either side of the current one, so the next turn in
    /// either direction is a copy. Call it when there's nothing else to do.
    pub fn prepare(&mut self) -> Result<(), ReaderError> {
        let Some(current) = self.current else {
            return Ok(());
        };
        self.draw(current)?;
        for turn in [Turn::Next, Turn::Previous] {
            if let Some(page) = self.step(current, turn)? {
                self.draw(page)?;
            }
        }
        // Keep the current page the most recently used, so it's never the
        // one pushed out.
        self.cached(current);
        Ok(())
    }

    /// Where the link nearest (`x`, `y`) on the current page goes, if one is
    /// within reach. The page must have been drawn with [`Self::page`].
    pub fn link_at(&self, x: f32, y: f32) -> Option<Position> {
        let current = self.current?;
        let (_, page) = self.pages.iter().find(|(p, _)| *p == current)?;
        let reach = REACH_MM / 25.4 * self.typesetter.settings().dpi;
        page.links
            .iter()
            .map(|link| (link.distance(x, y), link.target))
            .filter(|(distance, _)| *distance <= reach)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, target)| target)
    }

    /// Goes to a link's target, remembering where the reader was.
    pub fn follow(&mut self, target: Position) -> Result<(), ReaderError> {
        if let Some(here) = self.position() {
            if self.history.len() >= HISTORY_KEPT {
                self.history.remove(0);
            }
            self.history.push(here);
        }
        self.go_to(target)
    }

    /// Returns to where the last followed link was followed from. False if
    /// there's nowhere to return to.
    pub fn back(&mut self) -> Result<bool, ReaderError> {
        let Some(from) = self.history.pop() else {
            return Ok(false);
        };
        self.go_to(from)?;
        Ok(true)
    }

    /// Whether [`Self::back`] has somewhere to go.
    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    /// Whether a page is drawn already. For tests and timing.
    pub fn is_drawn(&self, page: PageRef) -> bool {
        self.pages.iter().any(|(p, _)| *p == page)
    }

    /// The page after or before `from`, paginating chapters as needed.
    fn step(&mut self, from: PageRef, turn: Turn) -> Result<Option<PageRef>, ReaderError> {
        let pages = self.paginated(from.chapter)?.len() as u32;
        match turn {
            Turn::Next if from.page + 1 < pages => Ok(Some(PageRef {
                page: from.page + 1,
                ..from
            })),
            Turn::Previous if from.page > 0 && pages > 0 => Ok(Some(PageRef {
                page: (from.page - 1).min(pages - 1),
                ..from
            })),
            _ => self.nearest(from.chapter, turn, false),
        }
    }

    /// Going forward, the first page of the nearest chapter after `chapter`
    /// that has pages; going back, the last page of the nearest one before.
    /// `inclusive` lets `chapter` itself be the one.
    fn nearest(
        &mut self,
        chapter: u32,
        turn: Turn,
        inclusive: bool,
    ) -> Result<Option<PageRef>, ReaderError> {
        let chapters = self.map.chapters.len() as u32;
        let mut next = if inclusive {
            Some(chapter)
        } else {
            match turn {
                Turn::Next => chapter.checked_add(1),
                Turn::Previous => chapter.checked_sub(1),
            }
        };
        while let Some(chapter) = next.filter(|c| *c < chapters) {
            let pages = self.paginated(chapter)?.len() as u32;
            if pages > 0 {
                let page = match turn {
                    Turn::Next => 0,
                    Turn::Previous => pages - 1,
                };
                return Ok(Some(PageRef { chapter, page }));
            }
            next = match turn {
                Turn::Next => chapter.checked_add(1),
                Turn::Previous => chapter.checked_sub(1),
            };
        }
        Ok(None)
    }

    fn paginated(&mut self, chapter: u32) -> Result<&[PageStart], ReaderError> {
        if self.map.pages(chapter).is_none() {
            let index = self.load_chapter(chapter)?;
            let pages = paginate(
                &mut self.typesetter,
                chapter,
                &self.chapters[index].1,
                &self.images,
            );
            self.map.chapters[chapter as usize] = Some(pages);
        }
        Ok(self.map.pages(chapter).unwrap_or_default())
    }

    /// Makes sure a chapter is parsed and returns where it is in the cache.
    fn load_chapter(&mut self, chapter: u32) -> Result<usize, ReaderError> {
        if let Some(at) = self.chapters.iter().position(|(c, _)| *c == chapter) {
            let entry = self.chapters.remove(at);
            self.chapters.push(entry);
        } else {
            let parsed = self.book.chapter(chapter as usize)?;
            if self.chapters.len() >= CHAPTERS_KEPT {
                self.chapters.remove(0);
            }
            self.chapters.push((chapter, parsed));
        }
        Ok(self.chapters.len() - 1)
    }

    fn cached(&mut self, page: PageRef) -> Option<&Canvas> {
        let at = self.pages.iter().position(|(p, _)| *p == page)?;
        let entry = self.pages.remove(at);
        self.pages.push(entry);
        self.pages.last().map(|(_, page)| &page.canvas)
    }

    fn draw(&mut self, page: PageRef) -> Result<(), ReaderError> {
        if self.cached(page).is_some() {
            return Ok(());
        }
        self.paginated(page.chapter)?;
        let index = self.load_chapter(page.chapter)?;
        let settings = self.typesetter.settings();
        let mut drawn = if self.pages.len() >= PAGES_KEPT {
            self.pages.remove(0).1
        } else {
            Page::new(settings.width, settings.height)
        };
        let Self {
            book,
            images,
            typesetter,
            map,
            chapters,
            ..
        } = self;
        let at = ChapterRef {
            index: page.chapter,
            chapter: &chapters[index].1,
            images,
        };
        let pages = map.pages(page.chapter).unwrap_or_default();
        draw_page(
            typesetter,
            &mut drawn,
            at,
            pages,
            page.page as usize,
            &mut |id| decode(book, id),
        );
        self.pages.push((page, drawn));
        Ok(())
    }
}

fn decode<R: Read + Seek>(book: &mut BookReader<R>, id: u32) -> Option<image::GrayImage> {
    let bytes = book.image(id as usize).ok()?;
    Some(image::load_from_memory(&bytes).ok()?.into_luma8())
}

/// Paginates every chapter of a book that `map` doesn't have yet, for a
/// background thread with its own copy of the book. `stop` is asked between
/// chapters, so the thread can give up when the book is closed or the
/// settings change.
pub fn paginate_book<R: Read + Seek>(
    book: &mut BookReader<R>,
    settings: Settings,
    map: &mut PageMap,
    mut stop: impl FnMut() -> bool,
) -> Result<(), ReaderError> {
    let manifest = book.manifest().clone();
    let mut typesetter = Typesetter::new(settings, manifest.language.as_deref());
    for index in 0..map.chapters.len() {
        if map.chapters[index].is_some() {
            continue;
        }
        if stop() {
            break;
        }
        let chapter = book.chapter(index)?;
        map.chapters[index] = Some(paginate(
            &mut typesetter,
            index as u32,
            &chapter,
            &manifest.images,
        ));
    }
    Ok(())
}
