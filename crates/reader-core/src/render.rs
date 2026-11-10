//! Glyphs are drawn from Skrifa's outlines with zeno and cached as alpha
//! masks, so a page is mostly copies. Images are scaled and copied in.

use std::collections::HashMap;

use parley::FontData;
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{LocationRef, Size},
    outline::{DrawSettings, OutlinePen},
};
use zeno::{Command, Mask, Scratch, Vector};

use crate::Canvas;

/// Glyphs land on one of this many horizontal positions within a pixel.
/// Vertical positions are whole pixels: baselines are rounded.
const SUBPIXELS: f32 = 4.0;

/// The cache is emptied when it grows past this many masks. A page uses a
/// few hundred; a book's whole set at one size is rarely over two thousand.
const MAX_GLYPHS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    font: u64,
    index: u32,
    glyph: u32,
    size: u32,
    coords: u64,
    subpixel: u8,
}

#[derive(Debug)]
struct Bitmap {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

pub(crate) struct Glyphs {
    /// Coverage to ink, from [`crate::Settings::gamma`].
    curve: [u8; 256],
    masks: HashMap<Key, Bitmap>,
    chars: HashMap<(u64, u32, char), Option<u32>>,
    scratch: Scratch,
    path: Vec<Command>,
}

impl std::fmt::Debug for Glyphs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Glyphs({} cached)", self.masks.len())
    }
}

impl Glyphs {
    pub(crate) fn new(gamma: f32) -> Self {
        let gamma = if gamma.is_finite() && gamma > 0.0 {
            gamma
        } else {
            1.0
        };
        let mut curve = [0u8; 256];
        for (i, out) in curve.iter_mut().enumerate() {
            let coverage = i as f32 / 255.0;
            *out = (coverage.powf(1.0 / gamma) * 255.0).round() as u8;
        }
        Self {
            curve,
            masks: HashMap::new(),
            chars: HashMap::new(),
            scratch: Scratch::new(),
            path: Vec::new(),
        }
    }

    /// Draws a glyph with its origin at (`x`, `y`) on the page. `coords` are
    /// the font's normalized variation coordinates, as raw `F2Dot14` bits.
    /// Returns whether the glyph has any ink: false for a space.
    #[allow(clippy::too_many_arguments, reason = "a glyph has this many parts")]
    pub(crate) fn draw(
        &mut self,
        canvas: &mut Canvas,
        font: &FontData,
        size: f32,
        coords: &[i16],
        glyph: u32,
        x: f32,
        y: f32,
    ) -> bool {
        let mut whole = x.floor();
        let mut subpixel = ((x - whole) * SUBPIXELS).round();
        if subpixel >= SUBPIXELS {
            whole += 1.0;
            subpixel = 0.0;
        }
        let key = Key {
            font: font.data.id(),
            index: font.index,
            glyph,
            size: size.to_bits(),
            coords: hash_coords(coords),
            subpixel: subpixel as u8,
        };
        if !self.masks.contains_key(&key) {
            if self.masks.len() >= MAX_GLYPHS {
                self.masks.clear();
            }
            let bitmap = self.rasterize(font, size, coords, glyph, subpixel / SUBPIXELS);
            self.masks.insert(key, bitmap);
        }
        let bitmap = &self.masks[&key];
        if bitmap.width > 0 {
            canvas.ink(
                whole as i32 + bitmap.left,
                y.round() as i32 + bitmap.top,
                bitmap.width,
                bitmap.height,
                &bitmap.alpha,
            );
        }
        bitmap.width > 0
    }

    /// Draws a character that Parley didn't shape, such as the hyphen at a
    /// soft-hyphen break, in the font of the text before it.
    #[allow(clippy::too_many_arguments, reason = "a glyph has this many parts")]
    pub(crate) fn draw_char(
        &mut self,
        canvas: &mut Canvas,
        font: &FontData,
        size: f32,
        coords: &[i16],
        c: char,
        x: f32,
        y: f32,
    ) {
        let glyph = *self
            .chars
            .entry((font.data.id(), font.index, c))
            .or_insert_with(|| {
                let font = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
                font.charmap().map(c).map(GlyphId::to_u32)
            });
        if let Some(glyph) = glyph {
            self.draw(canvas, font, size, coords, glyph, x, y);
        }
    }

    fn rasterize(
        &mut self,
        font: &FontData,
        size: f32,
        coords: &[i16],
        glyph: u32,
        offset: f32,
    ) -> Bitmap {
        let empty = Bitmap {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            alpha: Vec::new(),
        };
        let Ok(font_ref) = FontRef::from_index(font.data.as_ref(), font.index) else {
            return empty;
        };
        let Some(outline) = font_ref.outline_glyphs().get(GlyphId::new(glyph)) else {
            return empty;
        };
        let coords: Vec<skrifa::instance::NormalizedCoord> = coords
            .iter()
            .map(|bits| skrifa::instance::NormalizedCoord::from_bits(*bits))
            .collect();
        self.path.clear();
        let settings = DrawSettings::unhinted(Size::new(size), LocationRef::new(&coords));
        if outline.draw(settings, &mut Pen(&mut self.path)).is_err() || self.path.is_empty() {
            return empty;
        }
        let (mut alpha, placement) = Mask::with_scratch(&self.path[..], &mut self.scratch)
            .offset(Vector::new(offset, 0.0))
            .render();
        for a in &mut alpha {
            *a = self.curve[usize::from(*a)];
        }
        Bitmap {
            left: placement.left,
            top: placement.top,
            width: placement.width,
            height: placement.height,
            alpha,
        }
    }
}

fn hash_coords(coords: &[i16]) -> u64 {
    let mut hash = crate::Fnv::default();
    for coord in coords {
        hash.write(&coord.to_le_bytes());
    }
    hash.0
}

/// Turns Skrifa's outline, with y up, into zeno's path, with y down.
struct Pen<'a>(&'a mut Vec<Command>);

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::MoveTo(Vector::new(x, -y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::LineTo(Vector::new(x, -y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0
            .push(Command::QuadTo(Vector::new(cx0, -cy0), Vector::new(x, -y)));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(Command::CurveTo(
            Vector::new(cx0, -cy0),
            Vector::new(cx1, -cy1),
            Vector::new(x, -y),
        ));
    }

    fn close(&mut self) {
        self.0.push(Command::Close);
    }
}

/// Copies an image onto the page at the given size, scaling it if needed.
pub(crate) fn image(
    canvas: &mut Canvas,
    gray: &image::GrayImage,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) {
    let (width, height) = (width.max(1.0) as u32, height.max(1.0) as u32);
    let left = x.round() as i32;
    let top = y.round() as i32;
    if gray.width() == width && gray.height() == height {
        canvas.copy(left, top, width, height, gray.as_raw());
    } else {
        let scaled = crate::scale::scale(
            gray.as_raw(),
            (gray.width() as usize, gray.height() as usize),
            (width as usize, height as usize),
        );
        canvas.copy(left, top, width, height, &scaled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_darkens_edges_but_not_solid_ink_or_paper() {
        let plain = Glyphs::new(1.0);
        let dark = Glyphs::new(1.8);
        assert!((0..=255).all(|i| plain.curve[i] as usize == i));
        assert_eq!((dark.curve[0], dark.curve[255]), (0, 255));
        assert!(dark.curve[64] > 100, "{}", dark.curve[64]);
        assert!(dark.curve.windows(2).all(|w| w[0] <= w[1]));
        // Nonsense falls back to no change.
        assert_eq!(Glyphs::new(f32::NAN).curve, plain.curve);
    }
}
