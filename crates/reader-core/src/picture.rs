//! A gray picture outside a book's pages, such as a cover in the library.
//!
//! Covers are decoded and shrunk once, when a book is added, and kept as a
//! small raw file, so a screen of covers is copies rather than JPEG decodes.

use crate::{Canvas, scale::scale};

/// The raw file's first bytes, before the width and the height.
const MAGIC: &[u8; 4] = b"SBP1";
/// A side longer than this in a stored picture is taken as damage.
const MAX_SIDE: u32 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    width: u32,
    height: u32,
    /// One byte a pixel, row by row.
    pixels: Vec<u8>,
}

impl Picture {
    /// Decodes a JPEG or PNG, as stored in a book, into gray.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let gray = image::load_from_memory(bytes).ok()?.into_luma8();
        let (width, height) = gray.dimensions();
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels: gray.into_raw(),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The largest size that fits in `width` by `height` with the picture's
    /// shape, never larger than it is.
    pub fn fitted_size(&self, width: u32, height: u32) -> (u32, u32) {
        let scale = (width as f32 / self.width as f32)
            .min(height as f32 / self.height as f32)
            .min(1.0);
        (
            ((self.width as f32 * scale).floor() as u32).max(1),
            ((self.height as f32 * scale).floor() as u32).max(1),
        )
    }

    /// The picture shrunk to fit in `width` by `height`, keeping its shape.
    #[must_use]
    pub fn fitted(&self, width: u32, height: u32) -> Self {
        let (to_width, to_height) = self.fitted_size(width, height);
        Self {
            width: to_width,
            height: to_height,
            pixels: self.pixels_at(to_width, to_height),
        }
    }

    /// Draws the picture at `width` by `height`, its top left at (`left`,
    /// `top`).
    pub fn draw(&self, canvas: &mut Canvas, left: i32, top: i32, width: u32, height: u32) {
        canvas.copy(left, top, width, height, &self.pixels_at(width, height));
    }

    fn pixels_at(&self, width: u32, height: u32) -> Vec<u8> {
        if (width, height) == (self.width, self.height) {
            return self.pixels.clone();
        }
        scale(
            &self.pixels,
            (self.width as usize, self.height as usize),
            (width as usize, height as usize),
        )
    }

    /// The picture as a file: a short header, then the pixels.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(12 + self.pixels.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&self.pixels);
        bytes
    }

    /// Reads [`Picture::to_bytes`]' output. `None` if it isn't that.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (header, pixels) = bytes.split_at_checked(12)?;
        let side =
            |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap_or_default());
        let (width, height) = (side(4), side(8));
        let sane = (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height);
        if &header[..4] != MAGIC || !sane || pixels.len() != width as usize * height as usize {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels: pixels.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Picture {
        Picture {
            width,
            height,
            pixels: (0..width * height).map(|i| (i % 251) as u8).collect(),
        }
    }

    #[test]
    fn fitting_keeps_the_shape_and_never_enlarges() {
        let cover = gradient(965, 1448);
        assert_eq!(cover.fitted_size(400, 600), (399, 600));
        assert_eq!(cover.fitted_size(200, 1000), (200, 300));
        assert_eq!(cover.fitted_size(2000, 2000), (965, 1448));
        let small = cover.fitted(400, 600);
        assert_eq!((small.width(), small.height()), (399, 600));
    }

    #[test]
    fn a_stored_picture_reads_back_and_damage_is_refused() {
        let picture = gradient(30, 20);
        let bytes = picture.to_bytes();
        assert_eq!(Picture::from_bytes(&bytes), Some(picture));
        assert_eq!(Picture::from_bytes(&bytes[..bytes.len() - 1]), None);
        assert_eq!(Picture::from_bytes(b"SBP1"), None);
        let mut wrong = bytes.clone();
        wrong[0] = b'X';
        assert_eq!(Picture::from_bytes(&wrong), None);
    }

    #[test]
    fn it_draws_at_the_size_asked() {
        let picture = Picture {
            width: 2,
            height: 2,
            pixels: vec![0, 0, 0, 0],
        };
        let mut canvas = Canvas::new(10, 10);
        canvas.clear();
        picture.draw(&mut canvas, 2, 3, 4, 4);
        let rows: Vec<&[u8]> = canvas.pixels().chunks(10).collect();
        for (y, row) in rows.iter().enumerate() {
            let inked: Vec<usize> = (0..10).filter(|x| row[*x] == 0).collect();
            let expected: Vec<usize> = if (3..7).contains(&y) {
                (2..6).collect()
            } else {
                Vec::new()
            };
            assert_eq!(inked, expected, "row {y}");
        }
        assert_eq!(canvas.pixels()[3 * 10 + 2], 0);
    }
}
