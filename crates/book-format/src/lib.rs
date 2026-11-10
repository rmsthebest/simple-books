//! The book format the server makes from each EPUB, and the reader draws.
//!
//! An EPUB can use any HTML and CSS. This format has a short, fixed list of
//! blocks and inline styles instead, so the reader on a slow e-reader only lays
//! out things it knows. The server does the hard part once, when a book arrives.
//!
//! Blocks are a flat list per chapter, with nesting kept as an indent level.
//! That keeps [`Position`]s simple: a chapter, a block, and a character offset.
#![forbid(unsafe_code)]

mod container;

use serde::{Deserialize, Serialize};

pub use container::{About, BookReader, FormatError, Limits, NewImage, write};

/// Bumped whenever a reader of the old format couldn't read the new one.
pub const FORMAT_VERSION: u32 = 1;

/// The soft hyphen. It marks where a word may be broken, and isn't counted in
/// [`Position`] offsets.
pub const SOFT_HYPHEN: char = '\u{AD}';

/// The deepest indent a block may have. Deeper nesting in an EPUB is flattened.
pub const MAX_INDENT: u8 = 8;

/// Everything about a book except its chapters and images, which are read on
/// demand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// A BCP 47 tag, such as `en` or `de-CH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub chapters: Vec<ChapterInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toc: Vec<TocEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageInfo>,
    /// Added after format 1 was settled, so older books don't have it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Series>,
    /// The image to show for the book in a library: an index into
    /// [`Manifest::images`]. Added after format 1 was settled, like
    /// [`Manifest::series`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<u32>,
}

/// The series a book belongs to, and where in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    /// Usually a whole number; 1.5 for a novella between books one and two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<f64>,
}

/// What the reader needs to know about a chapter before opening it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterInfo {
    /// The chapter's length in [`Position`] units, for showing progress
    /// through the book without opening every chapter.
    pub chars: u64,
}

/// A table-of-contents entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocEntry {
    pub title: String,
    /// 0 for the top level.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub depth: u8,
    pub target: Position,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub kind: ImageKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageKind {
    Png,
    Jpeg,
}

/// A place in a book. It stays the same whatever the font, margins or screen,
/// so it can move between devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    pub chapter: u32,
    pub block: u32,
    /// Characters into the block's text, not counting soft hyphens.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    /// Nesting from blockquotes and lists, up to [`MAX_INDENT`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub indent: u8,
    #[serde(default, skip_serializing_if = "Align::is_default")]
    pub align: Align,
    /// Empty for images and separators. A `\n` in the text is a line break.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
}

impl Block {
    /// The block's length in [`Position`] units.
    pub fn chars(&self) -> u64 {
        self.spans
            .iter()
            .map(|span| span.text.chars().filter(|c| *c != SOFT_HYPHEN).count() as u64)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BlockKind {
    Paragraph {
        /// Whether the first line is indented, as in most novels. Off for a
        /// paragraph that follows a heading or separator.
        #[serde(default, skip_serializing_if = "is_false")]
        indent_first: bool,
    },
    Heading {
        /// 1 to 6.
        level: u8,
    },
    ListItem {
        /// What goes before the text: "•", "3.", "iv." and so on.
        marker: String,
    },
    /// Text whose spaces and line breaks are kept, such as code or poetry.
    Preformatted,
    Image {
        /// An index into [`Manifest::images`].
        image: u32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        alt: String,
    },
    /// A scene break.
    Separator,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    /// Justified for paragraphs, left for everything else.
    #[default]
    Default,
    Left,
    Center,
    Right,
}

impl Align {
    #[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's signature")]
    fn is_default(&self) -> bool {
        *self == Self::Default
    }
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "Style::is_plain")]
    pub style: Style,
    /// Where tapping it goes, such as a footnote. Links out of the book aren't
    /// kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Position>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
            link: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags, any of which can combine"
)]
pub struct Style {
    #[serde(default, skip_serializing_if = "is_false")]
    pub emphasis: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub strong: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub small_caps: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub code: bool,
    #[serde(default, skip_serializing_if = "Vertical::is_baseline")]
    pub vertical: Vertical,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vertical {
    #[default]
    Baseline,
    Super,
    Sub,
}

impl Vertical {
    #[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's signature")]
    fn is_baseline(&self) -> bool {
        *self == Self::Baseline
    }
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's signature")]
fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}
