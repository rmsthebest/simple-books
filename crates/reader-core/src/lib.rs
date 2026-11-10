//! The reading engine: lays out a book's blocks with Parley, stacks the lines
//! into pages, and draws a page into an 8-bit gray buffer.
//!
//! Nothing here knows about a device. The emulator and the Kobo both hand a
//! [`Canvas`] to their screen, and both feed taps through a [`TurnGuard`].
//!
//! - [`Typesetter`] lays out one block at a time and draws laid-out lines.
//! - [`paginate`] turns a chapter into [`PageStart`]s, which stay valid for as
//!   long as the book, [`Settings`] and screen stay the same.
//! - [`Reader`] holds an open book, its [`PageMap`] and the last few drawn
//!   pages, so turning back is a cache hit.
#![forbid(unsafe_code)]
#![allow(
    clippy::cast_precision_loss,
    reason = "pixel sizes and line counts are far below 2^24"
)]

mod canvas;
mod fonts;
mod guard;
mod knuth_plass;
mod paginate;
mod reader;
mod render;
mod typeset;

use serde::{Deserialize, Serialize};

pub use canvas::Canvas;
pub use guard::TurnGuard;
pub use paginate::{PageMap, PageRef, PageStart, paginate};
pub use reader::{Reader, ReaderError, Turn, paginate_book};
pub use typeset::Typesetter;

/// Bumped whenever a change here would put page breaks somewhere else, so
/// page maps cached on disk are made again.
pub const LAYOUT_VERSION: u32 = 3;

/// How pages look. Everything that changes where lines break is in here, and
/// [`Settings::cache_key`] covers all of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The page in pixels, as drawn: upright portrait on the Kobo.
    pub width: u32,
    pub height: u32,
    /// The screen's pixel density, which sets fonts' optical size.
    pub dpi: f32,
    pub margin_top: u32,
    pub margin_bottom: u32,
    pub margin_side: u32,
    /// The body text's size in pixels.
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Justify paragraphs. Otherwise they're ragged right.
    pub justify: bool,
    /// Break words at the soft hyphens the server put in.
    pub hyphenate: bool,
    /// Choose a justified paragraph's line breaks together, with Knuth and
    /// Plass's method, rather than filling each line in turn.
    pub optimal_breaks: bool,
}

impl Default for Settings {
    /// The Kobo Clara BW, with body text near 10 points.
    fn default() -> Self {
        Self {
            width: 1072,
            height: 1448,
            dpi: 300.0,
            margin_top: 72,
            margin_bottom: 72,
            margin_side: 64,
            font_size: 40.0,
            line_height: 1.4,
            justify: true,
            hyphenate: true,
            optimal_breaks: true,
        }
    }
}

impl Settings {
    /// Identifies these settings and [`LAYOUT_VERSION`] in a page map's cache
    /// key. Stable across runs and builds, unlike `std`'s hasher.
    pub fn cache_key(&self) -> u64 {
        let mut hash = Fnv::default();
        hash.write(&LAYOUT_VERSION.to_le_bytes());
        for value in [
            self.width,
            self.height,
            self.margin_top,
            self.margin_bottom,
            self.margin_side,
        ] {
            hash.write(&value.to_le_bytes());
        }
        for value in [self.dpi, self.font_size, self.line_height] {
            hash.write(&value.to_bits().to_le_bytes());
        }
        hash.write(&[
            u8::from(self.justify),
            u8::from(self.hyphenate),
            u8::from(self.optimal_breaks),
        ]);
        hash.0
    }

    pub(crate) fn content_width(&self) -> f32 {
        self.width.saturating_sub(2 * self.margin_side) as f32
    }

    pub(crate) fn content_height(&self) -> f32 {
        self.height
            .saturating_sub(self.margin_top + self.margin_bottom) as f32
    }

    /// The height of one line of body text.
    pub(crate) fn line(&self) -> f32 {
        self.font_size * self.line_height
    }
}

/// 64-bit FNV-1a.
pub(crate) struct Fnv(pub(crate) u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}
