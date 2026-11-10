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

use book_format::{Block, BookReader, Chapter, FormatError, ImageInfo, Manifest, Position};

use crate::{
    Canvas, PageMap, PageRef, PageStart, Settings, Typesetter,
    paginate::{draw_page, paginate, paginate_from, paginate_keeping, paginate_unless},
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
/// Pages laid out straight away after the settings change: the one shown,
/// and the next for [`Reader::prepare`].
const WINDOW_PAGES: usize = 2;

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
    /// After [`Reader::relayout`], the pages around the reader's place,
    /// until their chapter is laid out in full.
    window: Option<Window>,
}

/// A few pages of a chapter, from the line the reader was on, laid out
/// without the rest of it. Page 0 is the one at `top`, whatever its number
/// in the whole chapter turns out to be. The chapter stays `None` in the
/// map meanwhile.
struct Window {
    chapter: u32,
    top: Position,
    /// Where each page starts, and then where the page after the last
    /// starts, unless `to_end`.
    starts: Vec<PageStart>,
    /// The chapter ends with the last page.
    to_end: bool,
    /// How far through the chapter each page ends, from 0 to 1, by
    /// characters.
    through: Vec<f32>,
}

impl Window {
    fn pages(&self) -> u32 {
        let pages = if self.to_end {
            self.starts.len()
        } else {
            self.starts.len().saturating_sub(1)
        };
        pages as u32
    }

    /// Whether page 0 is the chapter's first.
    fn starts_chapter(&self) -> bool {
        self.starts
            .first()
            .is_some_and(|start| start.block == 0 && start.line == 0)
    }
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
        let mut reader = Self::new(book, settings, book_key, cached);
        reader.go_to(at)?;
        Ok(reader)
    }

    /// Opens a book at `at` after the settings changed, with the line that
    /// holds `at` at the top of the page, so the text the reader was on
    /// stays where it was. Only a couple of pages are laid out from there:
    /// the rest of `at`'s chapter waits for [`paginate_book`], given
    /// [`Reader::keeping`], then [`Reader::adopt`], or for a turn past them.
    /// The page map then differs from one laid out from the start, so save
    /// it ([`Reader::page_map`]) for opening here again.
    pub fn relayout(
        book: BookReader<R>,
        settings: Settings,
        book_key: u64,
        at: Position,
        cached: Option<PageMap>,
    ) -> Result<Self, ReaderError> {
        let mut reader = Self::new(book, settings, book_key, cached);
        let chapters = reader.map.chapters.len() as u32;
        if at.chapter >= chapters {
            reader.go_to(at)?;
            return Ok(reader);
        }
        let index = reader.load_chapter(at.chapter)?;
        let (starts, to_end) = paginate_from(
            &mut reader.typesetter,
            &reader.chapters[index].1,
            &reader.images,
            at,
            WINDOW_PAGES,
        );
        // Laid out again from `at`, whatever a saved map says.
        reader.map.chapters[at.chapter as usize] = None;
        let blocks = &reader.chapters[index].1.blocks;
        let total = blocks.iter().map(Block::chars).sum::<u64>().max(1) as f32;
        let through = (1..=starts.len())
            .map(|end| match starts.get(end) {
                Some(end) => {
                    let before: u64 = blocks[..end.block as usize].iter().map(Block::chars).sum();
                    ((before + u64::from(end.offset)) as f32 / total).min(1.0)
                }
                None => 1.0,
            })
            .collect();
        let window = Window {
            chapter: at.chapter,
            top: at,
            starts,
            to_end,
            through,
        };
        if window.pages() == 0 {
            // Nothing from `at` on: go by the whole chapter.
            reader.window = Some(window);
            reader.finish_window()?;
            reader.go_to(at)?;
        } else {
            reader.window = Some(window);
            reader.current = Some(PageRef {
                chapter: at.chapter,
                page: 0,
            });
        }
        Ok(reader)
    }

    fn new(
        book: BookReader<R>,
        settings: Settings,
        book_key: u64,
        cached: Option<PageMap>,
    ) -> Self {
        let manifest = book.manifest();
        let key = PageMap::key(book_key, &settings);
        let chapters = manifest.chapters.len();
        let map = cached
            .filter(|map| map.key == key && map.chapters.len() == chapters)
            .unwrap_or_else(|| PageMap::new(key, chapters));
        let typesetter = Typesetter::new(settings, manifest.language.as_deref());
        let images = manifest.images.clone();
        Self {
            book,
            images,
            typesetter,
            map,
            chapters: Vec::new(),
            pages: Vec::new(),
            current: None,
            history: Vec::new(),
            window: None,
        }
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
        if let Some(window) = &self.window
            && let Some(Some(pages)) = map.chapters.get(window.chapter as usize)
        {
            let pages = pages.clone();
            self.finish_with(pages);
        }
        for (mine, theirs) in self.map.chapters.iter_mut().zip(&map.chapters) {
            if mine.is_none() {
                mine.clone_from(theirs);
            }
        }
    }

    /// Where [`paginate_book`] must keep a page starting, for
    /// [`Reader::adopt`] to take its map: the reader's place when the
    /// settings changed, until its chapter is laid out in full.
    pub fn keeping(&self) -> Option<Position> {
        self.window.as_ref().map(|window| window.top)
    }

    /// How far through its chapter the current page ends, from 0 to 1: by
    /// pages once the chapter is laid out, and by characters before.
    pub fn through_chapter(&self) -> Option<f32> {
        let page = self.current?;
        match &self.window {
            Some(window) if window.chapter == page.chapter => {
                window.through.get(page.page as usize).copied()
            }
            _ => {
                let pages = self.map.pages(page.chapter)?.len().max(1);
                Some((page.page as f32 + 1.0) / pages as f32)
            }
        }
    }

    pub fn current(&self) -> Option<PageRef> {
        self.current
    }

    /// Where the current page starts. This is what to save and sync.
    pub fn position(&self) -> Option<Position> {
        let current = self.current?;
        match &self.window {
            Some(window) if window.chapter == current.chapter => {
                let start = window.starts.get(current.page as usize)?;
                Some(Position {
                    chapter: current.chapter,
                    block: start.block,
                    offset: start.offset,
                })
            }
            _ => self.map.position(current),
        }
    }

    /// Moves to the page that shows `position`. Past the end of a chapter, or
    /// in a chapter with nothing on it, that's the next page there is.
    pub fn go_to(&mut self, position: Position) -> Result<(), ReaderError> {
        self.finish_window()?;
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
        match self.step(current, turn, false)? {
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
            // Not past a window's pages: that would lay out the whole
            // chapter here, which the background does instead.
            if let Some(page) = self.step(current, turn, true)? {
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
    /// With `lazy`, a turn past a window's pages is `None` instead.
    fn step(
        &mut self,
        mut from: PageRef,
        turn: Turn,
        lazy: bool,
    ) -> Result<Option<PageRef>, ReaderError> {
        if let Some(window) = &self.window
            && window.chapter == from.chapter
        {
            let past = match turn {
                Turn::Next => from.page + 1 >= window.pages() && !window.to_end,
                Turn::Previous => from.page == 0 && !window.starts_chapter(),
            };
            if past {
                if lazy {
                    return Ok(None);
                }
                if let Some(shift) = self.finish_window()? {
                    from.page += shift;
                }
            }
        }
        let pages = self.paginated(from.chapter)?;
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

    /// Lays out the window's chapter in full, now. Returns how far its
    /// pages moved: the window's page 0 is that page in the chapter.
    fn finish_window(&mut self) -> Result<Option<u32>, ReaderError> {
        let Some(window) = &self.window else {
            return Ok(None);
        };
        let (chapter, top) = (window.chapter, window.top);
        let index = self.load_chapter(chapter)?;
        let pages = paginate_keeping(
            &mut self.typesetter,
            &self.chapters[index].1,
            &self.images,
            top,
        );
        Ok(self.finish_with(pages))
    }

    /// Puts the window's chapter, laid out in full, in the map, and moves
    /// the current page and the drawn ones to their numbers in it.
    fn finish_with(&mut self, pages: Vec<PageStart>) -> Option<u32> {
        let window = self.window.take()?;
        let chapter = window.chapter;
        let shown = window.starts.len().min(window.pages() as usize + 1);
        let shift = pages
            .iter()
            .position(|start| *start == window.starts[0])
            .filter(|&at| pages.get(at..at + shown) == Some(&window.starts[..shown]));
        self.map.chapters[chapter as usize] = Some(pages);
        let Some(shift) = shift else {
            // Laid out some other way: go by where the page starts.
            self.pages.retain(|(page, _)| page.chapter != chapter);
            if self.current.is_some_and(|page| page.chapter == chapter) {
                self.current = self.map.locate(window.top);
            }
            return None;
        };
        let shift = shift as u32;
        for (page, _) in &mut self.pages {
            if page.chapter == chapter {
                page.page += shift;
            }
        }
        if let Some(current) = &mut self.current
            && current.chapter == chapter
        {
            current.page += shift;
        }
        Some(shift)
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
            let pages = self.paginated(chapter)?;
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

    /// How many pages the chapter has, laying it out if it isn't yet. A
    /// window counts only its own pages.
    fn paginated(&mut self, chapter: u32) -> Result<u32, ReaderError> {
        if let Some(window) = &self.window
            && window.chapter == chapter
        {
            return Ok(window.pages());
        }
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
        Ok(self
            .map
            .pages(chapter)
            .map_or(0, |pages| pages.len() as u32))
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
            window,
            ..
        } = self;
        let at = ChapterRef {
            index: page.chapter,
            chapter: &chapters[index].1,
            images,
        };
        let pages = match window {
            Some(window) if window.chapter == page.chapter => &window.starts,
            _ => map.pages(page.chapter).unwrap_or_default(),
        };
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
/// background thread with its own copy of the book. With `keep`, from
/// [`Reader::keeping`], a page starts there, and its chapter goes first.
/// `stop` is asked every few paragraphs, so the thread can give up soon
/// after the book is closed or the settings change.
pub fn paginate_book<R: Read + Seek>(
    book: &mut BookReader<R>,
    settings: Settings,
    map: &mut PageMap,
    keep: Option<Position>,
    mut stop: impl FnMut() -> bool,
) -> Result<(), ReaderError> {
    let manifest = book.manifest().clone();
    let mut typesetter = Typesetter::new(settings, manifest.language.as_deref());
    let chapters = map.chapters.len();
    let first = keep
        .map(|keep| keep.chapter as usize)
        .filter(|&c| c < chapters);
    let order = first
        .into_iter()
        .chain((0..chapters).filter(|&c| Some(c) != first));
    for index in order {
        if map.chapters[index].is_some() {
            continue;
        }
        if stop() {
            break;
        }
        let chapter = book.chapter(index)?;
        let top = keep.filter(|top| top.chapter as usize == index);
        let Some(pages) = paginate_unless(
            &mut typesetter,
            index as u32,
            &chapter,
            &manifest.images,
            top,
            &mut stop,
        ) else {
            break;
        };
        map.chapters[index] = Some(pages);
    }
    Ok(())
}
