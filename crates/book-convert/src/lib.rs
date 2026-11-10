//! Turns an EPUB into the simple-books book format, which the reader on the
//! e-reader draws. The hard parts happen here, once per book: reading the
//! book's HTML and CSS, choosing hyphenation points, and making images gray
//! and screen-sized.
//!
//! Like `epub`, this crate has no filesystem, database or async access, and
//! every input is treated as hostile: each limit holds whatever the book does.
#![forbid(unsafe_code)]

mod build;
mod css;
mod html;
mod hyphenate;
mod images;
mod toc;

use std::{
    collections::HashMap,
    io::{Read, Seek, Write},
    rc::Rc,
};

use book_format::{Block, BlockKind, Chapter, FormatError, NewImage, Position, Span, TocEntry};
use epub::{Contents, EpubError};

use crate::{
    build::{BuiltChapter, ImageUse, RawBlock, Target},
    css::Styles,
    html::{Document, HtmlLimits},
    hyphenate::Hyphenator,
    images::{ImageLimits, Prepared},
    toc::RawEntry,
};

/// The Kobo Clara BW's screen, in pixels.
pub const CLARA_BW_SCREEN: (u32, u32) = (1072, 1448);

/// What converting a book may cost.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Bytes in any one chapter's HTML.
    pub max_document_bytes: u64,
    /// Bytes of HTML in the whole book.
    pub max_total_document_bytes: u64,
    pub max_stylesheet_bytes: u64,
    pub max_stylesheets_per_chapter: usize,
    pub max_rules: usize,
    /// Units of style matching for the whole book, roughly one per element
    /// and ancestor. Past it, the rest of the book is converted without styles.
    pub max_style_work: u64,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_image_bytes: u64,
    pub max_image_side: u32,
    pub max_image_alloc: u64,
    pub max_images: usize,
    /// Bytes of prepared images in the whole book.
    pub max_total_image_bytes: u64,
    pub max_toc_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_document_bytes: 8 * MIB,
            max_total_document_bytes: 256 * MIB,
            max_stylesheet_bytes: MIB,
            max_stylesheets_per_chapter: 16,
            max_rules: 20_000,
            max_style_work: 200_000_000,
            max_nodes: 1_000_000,
            max_depth: 128,
            max_image_bytes: 32 * MIB,
            max_image_side: 12_000,
            max_image_alloc: 512 * MIB,
            max_images: 2000,
            max_total_image_bytes: 256 * MIB,
            max_toc_entries: 10_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Images are scaled down to fit this, as (width, height).
    pub screen: (u32, u32),
    pub hyphenate: bool,
    pub limits: Limits,
    pub epub_limits: epub::Limits,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            screen: CLARA_BW_SCREEN,
            hyphenate: true,
            limits: Limits::default(),
            epub_limits: epub::Limits::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error(transparent)]
    Epub(#[from] EpubError),
    #[error("this book has no text to show")]
    NoText,
    #[error("this book is too large to convert")]
    TooLarge,
    #[error("couldn't write the converted book: {0}")]
    Write(FormatError),
}

impl From<FormatError> for ConvertError {
    fn from(error: FormatError) -> Self {
        match error {
            FormatError::TooLarge => Self::TooLarge,
            other => Self::Write(other),
        }
    }
}

/// What a conversion made, for the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub chapters: usize,
    pub images: usize,
    /// Images that couldn't be read, or were too small to keep.
    pub images_dropped: usize,
    pub toc_entries: usize,
    /// Whether soft hyphens were added for the book's language.
    pub hyphenated: bool,
    /// Whether the book ran out of style work and lost some styles.
    pub styles_cut_short: bool,
}

/// Converts the EPUB in `epub` and writes the result to `out`.
///
/// # Errors
///
/// [`ConvertError::Epub`] if the EPUB fails a check, [`ConvertError::NoText`]
/// if there's nothing in it to read, [`ConvertError::TooLarge`] if a limit is
/// reached, or [`ConvertError::Write`].
pub fn convert<R: Read + Seek, W: Write + Seek>(
    epub: R,
    out: W,
    options: &Options,
) -> Result<Report, ConvertError> {
    let mut contents = Contents::open(epub, &options.epub_limits)?;
    let limits = &options.limits;
    let html_limits = HtmlLimits {
        max_nodes: limits.max_nodes,
        max_depth: limits.max_depth,
    };
    let metadata = contents.metadata().clone();
    let mut state = State {
        contents: &mut contents,
        options,
        document: String::new(),
        images: Vec::new(),
        image_uses: HashMap::new(),
        image_bytes: 0,
        targets: Vec::new(),
        style_work: limits.max_style_work,
    };
    let mut language = metadata.language.clone().filter(|tag| is_language(tag));
    let built = read_chapters(&mut state, html_limits, &mut language)?;
    // After the chapters, so a cover shown on a page is stored once.
    let cover = match state.contents.cover().map(str::to_owned) {
        Some(entry) => match build::Book::image(&mut state, &entry) {
            ImageUse::Block(index) => Some(index),
            ImageUse::Small | ImageUse::Missing => None,
        },
        None => None,
    };

    // Empty documents, such as a page that only held a cover too small to
    // keep, are left out; a link to one goes to the next chapter instead.
    let mut numbers: HashMap<String, u32> = HashMap::new();
    let mut kept: Vec<BuiltChapter> = Vec::new();
    let mut waiting: Vec<String> = Vec::new();
    for (entry, chapter) in built {
        if chapter.blocks.is_empty() {
            waiting.push(entry);
            continue;
        }
        let number = kept.len() as u32;
        for empty in waiting.drain(..) {
            numbers.insert(empty, number);
        }
        numbers.insert(entry, number);
        kept.push(chapter);
    }
    if kept.is_empty() {
        return Err(ConvertError::NoText);
    }
    if kept.len() > book_format::Limits::default().max_chapters {
        return Err(ConvertError::TooLarge);
    }
    let last = kept.len() as u32 - 1;
    for empty in waiting {
        numbers.insert(empty, last);
    }

    let positions = Positions {
        numbers: &numbers,
        chapters: &kept,
    };
    let links: Vec<Option<Position>> = state
        .targets
        .iter()
        .map(|target| positions.of(target))
        .collect();
    let toc = table_of_contents(&mut state, &positions, html_limits)?;

    let hyphenator = language
        .as_deref()
        .filter(|_| options.hyphenate)
        .and_then(Hyphenator::for_language);
    let chapters: Vec<Chapter> = kept
        .iter()
        .map(|chapter| finish_chapter(chapter, &links, hyphenator))
        .collect();

    let report = Report {
        chapters: chapters.len(),
        images: state.images.len(),
        images_dropped: state
            .image_uses
            .values()
            .filter(|use_| !matches!(use_, ImageUse::Block(_)))
            .count(),
        toc_entries: toc.len(),
        hyphenated: hyphenator.is_some(),
        styles_cut_short: state.style_work == 0,
    };
    let series = metadata.series.map(|name| book_format::Series {
        name,
        index: metadata.series_index.filter(|index| index.is_finite()),
    });
    book_format::write(
        out,
        book_format::About {
            title: metadata.title.unwrap_or_default(),
            authors: metadata.authors,
            language,
            series,
            cover,
        },
        toc,
        &chapters,
        &state.images,
    )?;
    Ok(report)
}

/// Builds every document in the spine, in order. Picks up a language from
/// the documents if the package didn't give one.
fn read_chapters<R: Read + Seek>(
    state: &mut State<'_, '_, R>,
    html_limits: HtmlLimits,
    language: &mut Option<String>,
) -> Result<Vec<(String, BuiltChapter)>, ConvertError> {
    let limits = &state.options.limits;
    let spine = state.contents.spine().to_vec();
    let mut sheets = Sheets::default();
    let mut document_bytes = 0u64;
    let mut built = Vec::with_capacity(spine.len());
    for entry in spine {
        let Some(bytes) = state.contents.read(&entry, limits.max_document_bytes)? else {
            continue;
        };
        document_bytes += bytes.len() as u64;
        if document_bytes > limits.max_total_document_bytes {
            return Err(ConvertError::TooLarge);
        }
        let document = Document::parse(&epub::decode(&bytes), html_limits);
        drop(bytes);
        if language.is_none() {
            *language = document_language(&document);
        }
        let styles = sheets.for_document(state, &entry, &document)?;
        state.document.clone_from(&entry);
        let chapter = build::chapter(&document, &styles, state);
        built.push((entry, chapter));
    }
    Ok(built)
}

/// What the chapter builder needs while it works.
struct State<'c, 'l, R> {
    contents: &'c mut Contents<'l, R>,
    options: &'c Options,
    /// The entry being converted, which links are relative to.
    document: String,
    images: Vec<NewImage>,
    image_uses: HashMap<String, ImageUse>,
    image_bytes: u64,
    targets: Vec<Target>,
    style_work: u64,
}

impl<R: Read + Seek> build::Book for State<'_, '_, R> {
    fn resolve(&self, href: &str) -> Option<Target> {
        let document = epub::resolve(&self.document, href)?;
        let fragment = href
            .split_once('#')
            .map(|(_, fragment)| fragment.to_owned())
            .filter(|fragment| !fragment.is_empty());
        Some(Target { document, fragment })
    }

    fn image(&mut self, entry: &str) -> ImageUse {
        if let Some(found) = self.image_uses.get(entry) {
            return *found;
        }
        let limits = &self.options.limits;
        let found = if self.images.len() >= limits.max_images {
            ImageUse::Missing
        } else {
            match self.contents.read(entry, limits.max_image_bytes) {
                Ok(Some(bytes)) => {
                    let image_limits = ImageLimits {
                        max_side: limits.max_image_side,
                        max_alloc: limits.max_image_alloc,
                    };
                    match images::prepare(&bytes, self.options.screen, image_limits) {
                        Prepared::Image(image)
                            if self.image_bytes + image.bytes.len() as u64
                                <= limits.max_total_image_bytes =>
                        {
                            self.image_bytes += image.bytes.len() as u64;
                            self.images.push(image);
                            ImageUse::Block(self.images.len() as u32 - 1)
                        }
                        Prepared::Small => ImageUse::Small,
                        Prepared::Image(_) | Prepared::Unusable => ImageUse::Missing,
                    }
                }
                _ => ImageUse::Missing,
            }
        };
        self.image_uses.insert(entry.to_owned(), found);
        found
    }

    fn link(&mut self, target: Target) -> usize {
        self.targets.push(target);
        self.targets.len() - 1
    }

    fn charge(&mut self, units: u64) -> bool {
        if let Some(left) = self.style_work.checked_sub(units) {
            self.style_work = left;
            true
        } else {
            self.style_work = 0;
            false
        }
    }
}

/// Stylesheets, each read once, and the combined styles for each set of them
/// a chapter uses.
#[derive(Default)]
struct Sheets {
    text: HashMap<String, Rc<str>>,
    combined: HashMap<String, Rc<Styles>>,
}

impl Sheets {
    fn for_document<R: Read + Seek>(
        &mut self,
        state: &mut State<'_, '_, R>,
        entry: &str,
        document: &Document,
    ) -> Result<Rc<Styles>, ConvertError> {
        let limits = &state.options.limits;
        let mut parts: Vec<Rc<str>> = Vec::new();
        let mut key = String::new();
        for (index, element) in document.elements() {
            if parts.len() >= limits.max_stylesheets_per_chapter {
                break;
            }
            match element.name.as_str() {
                "link"
                    if element
                        .attribute("rel")
                        .is_some_and(|rel| rel.to_ascii_lowercase().contains("stylesheet")) =>
                {
                    let Some(path) = element
                        .attribute("href")
                        .and_then(|href| epub::resolve(entry, href))
                    else {
                        continue;
                    };
                    let text = if let Some(text) = self.text.get(&path) {
                        text.clone()
                    } else {
                        // A sheet too large to read is left out, not fatal.
                        let text: Rc<str> =
                            match state.contents.read(&path, limits.max_stylesheet_bytes) {
                                Ok(Some(bytes)) => epub::decode(&bytes).into(),
                                Ok(None) | Err(EpubError::TooLarge) => "".into(),
                                Err(error) => return Err(error.into()),
                            };
                        self.text.insert(path.clone(), text.clone());
                        text
                    };
                    key.push_str(&path);
                    key.push('\0');
                    parts.push(text);
                }
                "style" => {
                    let text = document.text_of(index);
                    key.push_str(&text);
                    key.push('\0');
                    parts.push(text.into());
                }
                _ => {}
            }
        }
        if let Some(styles) = self.combined.get(&key) {
            return Ok(styles.clone());
        }
        let mut styles = Styles::new(limits.max_rules);
        for part in &parts {
            styles.add_sheet(part);
        }
        let styles = Rc::new(styles);
        self.combined.insert(key, styles.clone());
        Ok(styles)
    }
}

/// The `lang` on a document's root element.
fn document_language(document: &Document) -> Option<String> {
    let (_, html) = document.elements().find(|(_, e)| e.name == "html")?;
    let tag = html
        .attribute("xml:lang")
        .or_else(|| html.attribute("lang"))?
        .trim();
    (is_language(tag) && tag.len() <= 35).then(|| tag.to_owned())
}

/// Leaves out the tags that name no language: undetermined, several, none.
fn is_language(tag: &str) -> bool {
    let primary = tag.split(['-', '_']).next().unwrap_or_default();
    !primary.is_empty()
        && !["und", "mul", "zxx", "mis"]
            .iter()
            .any(|code| primary.eq_ignore_ascii_case(code))
}

/// Turns a link into a [`Position`] once chapters are numbered.
struct Positions<'p> {
    numbers: &'p HashMap<String, u32>,
    chapters: &'p [BuiltChapter],
}

impl Positions<'_> {
    /// `None` for a link to something that isn't a chapter, such as an image.
    fn of(&self, target: &Target) -> Option<Position> {
        let chapter = *self.numbers.get(&target.document)?;
        let built = &self.chapters[chapter as usize];
        let (block, offset) = target
            .fragment
            .as_ref()
            .and_then(|fragment| built.anchors.get(fragment))
            .copied()
            .unwrap_or((0, 0));
        let length = built.blocks.get(block as usize).map_or(0, raw_chars);
        Some(Position {
            chapter,
            block,
            offset: offset.min(length),
        })
    }
}

fn raw_chars(block: &RawBlock) -> u32 {
    block
        .spans
        .iter()
        .map(|span| {
            span.text
                .chars()
                .filter(|c| *c != book_format::SOFT_HYPHEN)
                .count() as u32
        })
        .sum()
}

fn table_of_contents<R: Read + Seek>(
    state: &mut State<'_, '_, R>,
    positions: &Positions<'_>,
    html_limits: HtmlLimits,
) -> Result<Vec<TocEntry>, ConvertError> {
    let limits = &state.options.limits;
    let sources = [
        (state.contents.nav().map(str::to_owned), true),
        (state.contents.ncx().map(str::to_owned), false),
    ];
    // Some books fill in only one of the two, so the fuller one is used.
    let mut best: Vec<TocEntry> = Vec::new();
    for (entry, is_nav) in sources {
        let Some(entry) = entry else {
            continue;
        };
        let bytes = match state.contents.read(&entry, limits.max_document_bytes) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(EpubError::TooLarge) => continue,
            Err(error) => return Err(error.into()),
        };
        let document = Document::parse(&epub::decode(&bytes), html_limits);
        let raw = if is_nav {
            toc::from_nav(&document, limits.max_toc_entries)
        } else {
            toc::from_ncx(&document, limits.max_toc_entries)
        };
        state.document.clone_from(&entry);
        let entries: Vec<TocEntry> = raw
            .into_iter()
            .filter_map(|RawEntry { title, depth, href }| {
                let target = build::Book::resolve(&*state, &href)?;
                Some(TocEntry {
                    title,
                    depth,
                    target: positions.of(&target)?,
                })
            })
            .collect();
        if entries.len() > best.len() {
            best = entries;
        }
    }
    if best.is_empty() {
        best = from_headings(positions.chapters);
    }
    Ok(best)
}

/// A book with no usable table of contents gets one from its top two levels
/// of headings.
fn from_headings(chapters: &[BuiltChapter]) -> Vec<TocEntry> {
    let mut found: Vec<(u8, String, Position)> = Vec::new();
    for (chapter, built) in chapters.iter().enumerate() {
        for (block, raw) in built.blocks.iter().enumerate() {
            if let BlockKind::Heading { level } = raw.kind {
                let text: String = raw.spans.iter().map(|s| s.text.as_str()).collect();
                let title = toc::title(&text.replace(book_format::SOFT_HYPHEN, ""));
                if !title.is_empty() {
                    let target = Position {
                        chapter: chapter as u32,
                        block: block as u32,
                        offset: 0,
                    };
                    found.push((level, title, target));
                }
            }
        }
    }
    let Some(top) = found.iter().map(|(level, _, _)| *level).min() else {
        return Vec::new();
    };
    found
        .into_iter()
        .filter(|(level, _, _)| *level <= top + 1)
        .map(|(level, title, target)| TocEntry {
            title,
            depth: level - top,
            target,
        })
        .collect()
}

fn finish_chapter(
    built: &BuiltChapter,
    links: &[Option<Position>],
    hyphenator: Option<Hyphenator>,
) -> Chapter {
    let blocks = built
        .blocks
        .iter()
        .map(|raw| {
            let hyphenate = !matches!(
                raw.kind,
                BlockKind::Heading { .. } | BlockKind::Preformatted
            );
            let spans = raw
                .spans
                .iter()
                .map(|span| {
                    let text = match hyphenator {
                        Some(hyphenator) if hyphenate && !span.style.code => {
                            hyphenator.apply(&span.text)
                        }
                        _ => span.text.clone(),
                    };
                    Span {
                        text,
                        style: span.style,
                        link: span
                            .link
                            .and_then(|link| links.get(link).copied().flatten()),
                    }
                })
                .collect();
            Block {
                kind: raw.kind.clone(),
                indent: raw.indent,
                align: raw.align,
                spans,
            }
        })
        .collect();
    Chapter { blocks }
}
