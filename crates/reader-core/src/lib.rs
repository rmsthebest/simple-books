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
mod picture;
mod reader;
mod render;
mod scale;
mod typeset;

use serde::{Deserialize, Serialize};

pub use canvas::Canvas;
pub use fonts::{find_fonts, font_families, varies_in_weight};
pub use guard::TurnGuard;
pub use paginate::{PageMap, PageRef, PageStart, paginate};
pub use picture::Picture;
pub use reader::{Reader, ReaderError, Turn, paginate_book};
pub use typeset::{Typesetter, WordArea};

/// Bumped whenever a change here would put page breaks somewhere else, so
/// page maps cached on disk are made again.
pub const LAYOUT_VERSION: u32 = 4;

/// Literata's weight axis.
const MIN_WEIGHT: f32 = 200.0;
const MAX_WEIGHT: f32 = 900.0;

/// How pages look. Everything that changes where lines break is in here, and
/// [`Settings::cache_key`] covers all of that; `gamma` only changes drawing.
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
    /// The body text's weight, 400 being regular. Literata is variable, so
    /// text can be made darker for e-ink by thickening its strokes. Headings
    /// and bold text keep the same steps above it.
    pub weight: f32,
    /// The body text's family, from [`font_families`]; `None` for Literata.
    /// One that's no longer there is set in Literata too.
    #[serde(default)]
    pub font: Option<String>,
    /// Applied to glyphs' coverage: above 1, the grey edges of letters are
    /// drawn darker, so text looks heavier without changing its shapes.
    pub gamma: f32,
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
            weight: 400.0,
            font: None,
            gamma: 1.0,
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
        for value in [self.dpi, self.font_size, self.line_height, self.weight] {
            hash.write(&value.to_bits().to_le_bytes());
        }
        hash.write(&[
            u8::from(self.justify),
            u8::from(self.hyphenate),
            u8::from(self.optimal_breaks),
        ]);
        if let Some(font) = &self.font {
            hash.write(font.as_bytes());
            hash.write(&fonts::fingerprint(Some(font)).to_le_bytes());
        }
        hash.0
    }

    pub(crate) fn content_width(&self) -> f32 {
        self.width.saturating_sub(2 * self.margin_side) as f32
    }

    pub(crate) fn content_height(&self) -> f32 {
        self.height
            .saturating_sub(self.margin_top + self.margin_bottom) as f32
    }

    /// A weight `steps` above the body text's, within the fonts' range.
    #[cfg(test)]
    pub(crate) fn weight_above(&self, steps: f32) -> f32 {
        weight_above(self.weight, steps)
    }

    /// The height of one line of body text.
    pub(crate) fn line(&self) -> f32 {
        self.font_size * self.line_height
    }
}

/// A weight `steps` above `weight`, within the fonts' range.
pub(crate) fn weight_above(weight: f32, steps: f32) -> f32 {
    (weight + steps).clamp(MIN_WEIGHT, MAX_WEIGHT)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_changes_the_layout_and_gamma_does_not() {
        let plain = Settings::default();
        let heavier = Settings {
            weight: 450.0,
            ..plain.clone()
        };
        let darker = Settings {
            gamma: 1.5,
            ..plain.clone()
        };
        assert_ne!(plain.cache_key(), heavier.cache_key());
        assert_eq!(plain.cache_key(), darker.cache_key());
    }

    #[test]
    fn weights_stay_in_the_fonts_range() {
        let heavy = Settings {
            weight: 800.0,
            ..Settings::default()
        };
        assert_eq!(heavy.weight_above(300.0), 900.0);
        assert_eq!(Settings::default().weight_above(200.0), 600.0);
    }
}
