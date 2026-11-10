//! Stacks a chapter's lines into pages.
//!
//! Lines fill the page from the top, and a page ends when the next line
//! doesn't fit, so the bottom edge is a little ragged, as on the stock reader.
//! Before breaking, it backs up so that:
//!
//! - a heading isn't left at the bottom of a page;
//! - a paragraph doesn't leave its first line alone at the bottom (an orphan)
//!   or its last line alone at the top of the next page (a widow).

use book_format::{Chapter, ImageInfo, Position};
use serde::{Deserialize, Serialize};

use crate::{
    Settings, Typesetter,
    typeset::{ChapterRef, Page},
};

/// Where a page starts: a line of a block, and that line's place in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageStart {
    pub block: u32,
    /// The line within the block.
    pub line: u32,
    /// Characters into the block, as in [`Position::offset`].
    pub offset: u32,
}

/// A page of a book: a chapter and a page within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageRef {
    pub chapter: u32,
    pub page: u32,
}

/// Where every page of a book starts, for one set of [`Settings`]. Chapters
/// are filled in as they're paginated, so a book opens before it's all done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageMap {
    /// From [`PageMap::key`]. A cached map with another key is out of date.
    pub key: u64,
    pub chapters: Vec<Option<Vec<PageStart>>>,
}

impl PageMap {
    pub fn new(key: u64, chapters: usize) -> Self {
        Self {
            key,
            chapters: vec![None; chapters],
        }
    }

    /// The key for a book, identified by the caller (such as the server's
    /// revision of the file), laid out with `settings`.
    pub fn key(book: u64, settings: &Settings) -> u64 {
        let mut hash = crate::Fnv::default();
        hash.write(&book.to_le_bytes());
        hash.write(&settings.cache_key().to_le_bytes());
        hash.0
    }

    pub fn is_complete(&self) -> bool {
        self.chapters.iter().all(Option::is_some)
    }

    pub fn pages(&self, chapter: u32) -> Option<&[PageStart]> {
        self.chapters.get(chapter as usize)?.as_deref()
    }

    /// The number of pages in the book, once every chapter is paginated.
    pub fn total(&self) -> Option<usize> {
        self.chapters
            .iter()
            .map(|pages| pages.as_ref().map(Vec::len))
            .sum()
    }

    /// The page's number in the whole book, counting from 0, once the
    /// chapters before it are paginated.
    pub fn number(&self, page: PageRef) -> Option<usize> {
        let before: Option<usize> = self.chapters[..page.chapter as usize]
            .iter()
            .map(|pages| pages.as_ref().map(Vec::len))
            .sum();
        Some(before? + page.page as usize)
    }

    /// The page that shows `position`, if its chapter is paginated. A
    /// position past a chapter's end is on its last page.
    pub fn locate(&self, position: Position) -> Option<PageRef> {
        let pages = self.pages(position.chapter)?;
        if pages.is_empty() {
            return None;
        }
        let after = pages.partition_point(|start| {
            (start.block, start.offset) <= (position.block, position.offset)
        });
        Some(PageRef {
            chapter: position.chapter,
            page: after.saturating_sub(1) as u32,
        })
    }

    /// Where a page starts, as a [`Position`].
    pub fn position(&self, page: PageRef) -> Option<Position> {
        let start = self.pages(page.chapter)?.get(page.page as usize)?;
        Some(Position {
            chapter: page.chapter,
            block: start.block,
            offset: start.offset,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Item {
    block: u32,
    line: u32,
    lines: u32,
    offset: u32,
    height: f32,
    /// Space above, left out at the top of a page.
    space: f32,
    keep_with_next: bool,
}

/// Lays out a chapter and returns where each of its pages starts. A chapter
/// with nothing in it has no pages.
pub fn paginate(
    typesetter: &mut Typesetter,
    index: u32,
    chapter: &Chapter,
    images: &[ImageInfo],
) -> Vec<PageStart> {
    paginate_with_top(typesetter, index, chapter, images, None, &mut || false).unwrap_or_default()
}

/// Like [`paginate`], but a page starts with the line that holds `top`, a
/// place in this chapter. For laying the chapter out again with other
/// settings, so the reader's first line stays at the top of the page. The
/// pages before it are laid out as usual, so the one just before may be
/// short.
pub fn paginate_keeping(
    typesetter: &mut Typesetter,
    chapter: &Chapter,
    images: &[ImageInfo],
    top: Position,
) -> Vec<PageStart> {
    paginate_with_top(
        typesetter,
        top.chapter,
        chapter,
        images,
        Some(top),
        &mut || false,
    )
    .unwrap_or_default()
}

/// Blocks laid out between asking whether to stop.
const BLOCKS_BETWEEN_STOPS: usize = 16;

/// [`paginate`], or with `top` [`paginate_keeping`], giving up with `None`
/// as soon as `stop` says so: a long chapter takes a second on the Kobo.
pub(crate) fn paginate_unless(
    typesetter: &mut Typesetter,
    index: u32,
    chapter: &Chapter,
    images: &[ImageInfo],
    top: Option<Position>,
    stop: &mut dyn FnMut() -> bool,
) -> Option<Vec<PageStart>> {
    paginate_with_top(typesetter, index, chapter, images, top, stop)
}

fn paginate_with_top(
    typesetter: &mut Typesetter,
    index: u32,
    chapter: &Chapter,
    images: &[ImageInfo],
    top: Option<Position>,
    stop: &mut dyn FnMut() -> bool,
) -> Option<Vec<PageStart>> {
    let at = ChapterRef {
        index,
        chapter,
        images,
    };
    let mut items = Vec::new();
    for block in 0..chapter.blocks.len() {
        if block % BLOCKS_BETWEEN_STOPS == BLOCKS_BETWEEN_STOPS - 1 && stop() {
            return None;
        }
        push_lines(typesetter, at, block, &mut items);
    }
    let available = typesetter.settings().content_height();
    // The last line starting at or before `top`.
    let forced = top.and_then(|top| last_line_at(&items, top));
    let starts = break_pages(&items, available, forced);
    Some(starts.into_iter().map(|i| items[i].start()).collect())
}

/// A few pages from the line that holds `top`, as [`paginate_keeping`]
/// would find them, without laying out the rest of the chapter: enough to
/// show a page straight after the settings change.
///
/// Returns where up to `wanted` pages start, then where the page after
/// them starts, so the last one's end is known; or, when the chapter ends
/// sooner, where each page up to its end starts, and true.
pub fn paginate_from(
    typesetter: &mut Typesetter,
    chapter: &Chapter,
    images: &[ImageInfo],
    top: Position,
    wanted: usize,
) -> (Vec<PageStart>, bool) {
    let at = ChapterRef {
        index: top.chapter,
        chapter,
        images,
    };
    let available = typesetter.settings().content_height();
    let first = (top.block as usize).min(chapter.blocks.len());
    let mut items = Vec::new();
    // Where `top`'s line is in `items`, once its block is laid out.
    let mut from = None;
    let mut height = 0.0;
    for block in first..chapter.blocks.len() {
        let before = items.len();
        push_lines(typesetter, at, block, &mut items);
        if from.is_none() {
            from = Some(last_line_at(&items, top).unwrap_or(0));
        }
        height += items[before..]
            .iter()
            .map(|item| item.space + item.height)
            .sum::<f32>();
        // Breaks are only settled once a line has overflowed onto the page
        // after: lines on the last page could still be pulled back.
        if height > available * (wanted + 1) as f32 {
            let from = from.unwrap_or(0);
            let starts = break_pages(&items[from..], available, None);
            if starts.len() > wanted {
                let starts = starts[..=wanted].iter().map(|&i| items[from + i].start());
                return (starts.collect(), false);
            }
        }
    }
    let from = from.unwrap_or(0).min(items.len());
    let starts = break_pages(&items[from..], available, None);
    (
        starts.iter().map(|&i| items[from + i].start()).collect(),
        true,
    )
}

/// Adds block `block`'s lines to `items`.
fn push_lines(
    typesetter: &mut Typesetter,
    at: ChapterRef<'_>,
    block: usize,
    items: &mut Vec<Item>,
) {
    let placed = typesetter.block(at, block);
    let lines = placed.lines.len() as u32;
    for (line, info) in placed.lines.iter().enumerate() {
        items.push(Item {
            block: block as u32,
            line: line as u32,
            lines,
            offset: info.offset,
            height: info.height,
            space: if line == 0 { placed.space_before } else { 0.0 },
            keep_with_next: placed.keep_with_next,
        });
    }
}

/// The last line in `items` starting at or before `top`.
fn last_line_at(items: &[Item], top: Position) -> Option<usize> {
    items
        .partition_point(|item| (item.block, item.offset) <= (top.block, top.offset))
        .checked_sub(1)
}

impl Item {
    fn start(&self) -> PageStart {
        PageStart {
            block: self.block,
            line: self.line,
            offset: self.offset,
        }
    }
}

/// Returns the index of the item that starts each page. If `forced` is
/// given, a page starts with that item.
fn break_pages(items: &[Item], available: f32, forced: Option<usize>) -> Vec<usize> {
    if items.is_empty() {
        return Vec::new();
    }
    // Rounding in line heights mustn't push a line that fits onto the next page.
    let available = available + 0.01;
    let mut starts = vec![0];
    let mut start = 0;
    let mut used = 0.0;
    let mut i = 0;
    while i < items.len() {
        if forced == Some(i) && i > start {
            starts.push(i);
            start = i;
            used = 0.0;
        }
        let item = &items[i];
        let height = if i == start {
            item.height
        } else {
            item.space + item.height
        };
        if i > start && used + height > available {
            let at = choose_break(items, start, i);
            starts.push(at);
            start = at;
            used = 0.0;
            i = at;
            continue;
        }
        used += height;
        i += 1;
    }
    starts
}

/// Where to break when item `overflow` doesn't fit on the page that starts
/// at `start`: at `overflow` itself, or a little earlier.
fn choose_break(items: &[Item], start: usize, overflow: usize) -> usize {
    let mut at = overflow;
    loop {
        let before = at;
        // A heading goes over with what follows it.
        while at - 1 > start && items[at - 1].keep_with_next {
            at -= 1;
        }
        let next = &items[at];
        if next.line == 1 && next.lines > 1 && at - 1 > start {
            // The paragraph's first line would be alone at the bottom.
            at -= 1;
        } else if next.line + 1 == next.lines && next.line >= 1 {
            // Its last line would be alone at the top of the next page: take
            // one more line over, or the whole paragraph if that would leave
            // its first line alone instead.
            let back = if next.line >= 3 {
                1
            } else {
                next.line as usize
            };
            if at - back > start {
                at -= back;
            }
        }
        if at == before {
            return at;
        }
    }
}

/// Draws page `page` of a chapter paginated into `pages`, and lists where
/// its links are.
pub(crate) fn draw_page(
    typesetter: &mut Typesetter,
    drawn: &mut Page,
    at: ChapterRef<'_>,
    pages: &[PageStart],
    page: usize,
    image: &mut dyn FnMut(u32) -> Option<image::GrayImage>,
) {
    drawn.canvas.clear();
    drawn.links.clear();
    let Some(start) = pages.get(page) else {
        return;
    };
    let end = pages.get(page + 1);
    let mut top = typesetter.settings().margin_top as f32;
    let mut first = true;
    let last_block = end.map_or(at.chapter.blocks.len().saturating_sub(1), |end| {
        end.block as usize
    });
    for block in start.block as usize..=last_block.min(at.chapter.blocks.len().saturating_sub(1)) {
        let placed = typesetter.block(at, block);
        let from = if block == start.block as usize {
            start.line as usize
        } else {
            0
        };
        let to = match end {
            Some(end) if end.block as usize == block => end.line as usize,
            _ => placed.lines.len(),
        };
        if from >= to {
            continue;
        }
        if !first && from == 0 {
            top += placed.space_before;
        }
        first = false;
        typesetter.draw_lines(drawn, at, block, from..to, top, image);
        top += placed.lines[from..to].iter().map(|l| l.height).sum::<f32>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(block: u32, lines: u32) -> Vec<Item> {
        (0..lines)
            .map(|line| Item {
                block,
                line,
                lines,
                offset: line * 50,
                height: 10.0,
                space: 0.0,
                keep_with_next: false,
            })
            .collect()
    }

    fn heading(block: u32) -> Item {
        Item {
            keep_with_next: true,
            space: 10.0,
            ..paragraph(block, 1)[0]
        }
    }

    #[test]
    fn fills_pages() {
        let items = paragraph(0, 25);
        assert_eq!(break_pages(&items, 100.0, None), [0, 10, 20]);
    }

    #[test]
    fn no_orphans() {
        // 9 lines, then a paragraph whose first line would be alone at the bottom.
        let mut items = paragraph(0, 9);
        items.extend(paragraph(1, 5));
        assert_eq!(break_pages(&items, 100.0, None), [0, 9]);
    }

    #[test]
    fn no_widows() {
        // 11 lines: the last would be alone on the next page, so two go over.
        let items = paragraph(0, 11);
        assert_eq!(break_pages(&items, 100.0, None), [0, 9]);
        // 3-line paragraph after 8 lines: moving one line over would orphan
        // its first, so the whole paragraph goes.
        let mut items = paragraph(0, 8);
        items.extend(paragraph(1, 3));
        assert_eq!(break_pages(&items, 100.0, None), [0, 8]);
    }

    #[test]
    fn headings_stay_with_their_text() {
        let mut items = paragraph(0, 8);
        items.push(heading(1));
        items.extend(paragraph(2, 4));
        // The heading (with its space) fits as line 9, but its paragraph
        // wouldn't start before the page ends.
        assert_eq!(break_pages(&items, 100.0, None), [0, 8]);
    }

    #[test]
    fn space_is_dropped_at_the_top_of_a_page() {
        let mut items = paragraph(0, 10);
        items.push(heading(1));
        items.extend(paragraph(2, 9));
        // The heading starts page two without its space, so all nine lines fit.
        assert_eq!(break_pages(&items, 100.0, None), [0, 10]);
    }

    #[test]
    fn a_page_always_moves_forward() {
        // Nothing but headings: the keep rule can't push the break back to the
        // start of the page.
        let items: Vec<Item> = (0..30).map(heading).collect();
        let starts = break_pages(&items, 100.0, None);
        assert!(starts.windows(2).all(|w| w[0] < w[1]));
        // A single line taller than the page still gets a page.
        let mut tall = paragraph(0, 1);
        tall[0].height = 500.0;
        tall.extend(paragraph(1, 3));
        assert_eq!(break_pages(&tall, 100.0, None), [0, 1]);
    }

    #[test]
    fn a_forced_start_begins_a_page_and_the_rest_follow_from_it() {
        // Ten lines a page, in paragraphs of five. Forcing line 14 to the
        // top leaves a short page before it, even though that leaves the
        // paragraph's last line alone. After it the usual rules apply: 24
        // would be a widow, so that page ends a line early.
        let items: Vec<Item> = (0..6).flat_map(|b| paragraph(b, 5)).collect();
        assert_eq!(break_pages(&items, 100.0, Some(14)), [0, 10, 14, 23]);
        // Forcing a line that starts a page anyway changes nothing.
        assert_eq!(
            break_pages(&items, 100.0, Some(10)),
            break_pages(&items, 100.0, None)
        );
        // Nor does forcing the first line.
        assert_eq!(break_pages(&items, 100.0, Some(0)), [0, 10, 20]);
    }

    #[test]
    fn locate_finds_the_page_with_a_position() {
        let mut map = PageMap::new(1, 2);
        map.chapters[1] = Some(vec![
            PageStart {
                block: 0,
                line: 0,
                offset: 0,
            },
            PageStart {
                block: 3,
                line: 4,
                offset: 200,
            },
        ]);
        let at = |block, offset| Position {
            chapter: 1,
            block,
            offset,
        };
        let page = |page| Some(PageRef { chapter: 1, page });
        assert_eq!(map.locate(at(0, 0)), page(0));
        assert_eq!(map.locate(at(3, 199)), page(0));
        assert_eq!(map.locate(at(3, 200)), page(1));
        assert_eq!(map.locate(at(9, 0)), page(1));
        assert_eq!(
            map.locate(Position {
                chapter: 0,
                block: 0,
                offset: 0
            }),
            None
        );
        assert_eq!(map.total(), None);
        map.chapters[0] = Some(Vec::new());
        assert_eq!(map.total(), Some(2));
        assert_eq!(
            map.number(PageRef {
                chapter: 1,
                page: 1
            }),
            Some(1)
        );
    }
}
