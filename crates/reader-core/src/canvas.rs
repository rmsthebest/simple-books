/// An 8-bit gray page, one byte per pixel, rows top to bottom. 255 is white.
/// It's the Kobo framebuffer's own layout, so it can be copied straight in.
#[derive(Clone, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl std::fmt::Debug for Canvas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Canvas({}×{})", self.width, self.height)
    }
}

pub(crate) const WHITE: u8 = 255;
/// A pixel darker than this has ink on it, for underlines to avoid.
const INKED: u8 = 224;

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![WHITE; width as usize * height as usize],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub(crate) fn clear(&mut self) {
        self.pixels.fill(WHITE);
    }

    /// Darkens the page through an alpha mask, as black ink would.
    pub(crate) fn ink(&mut self, left: i32, top: i32, width: u32, height: u32, alpha: &[u8]) {
        self.each_row(left, top, width, height, |row, src_x, src_y, len| {
            let src = &alpha[src_y * width as usize + src_x..][..len];
            for (dst, a) in row.iter_mut().zip(src) {
                // dst × (1 − a), rounded.
                let keep = u16::from(255 - a);
                *dst = ((u16::from(*dst) * keep + 127) / 255) as u8;
            }
        });
    }

    /// Draws an underline, leaving a `gap` around ink already in its way,
    /// such as a descender, so the line doesn't run through letters.
    pub(crate) fn underline(&mut self, left: i32, top: i32, width: u32, height: u32, gap: u32) {
        let page_width = self.width as i32;
        let x0 = left.max(0);
        let x1 = (left + width as i32).min(page_width);
        if x0 >= x1 {
            return;
        }
        let gap = gap as i32;
        let rows = (top - gap).max(0)..(top + height as i32 + gap).min(self.height as i32);
        // Columns with ink near the line, widened by the gap on each side.
        let span = (x1 - x0) as usize;
        let mut blocked = vec![false; span];
        for x in (x0 - gap).max(0)..(x1 + gap).min(page_width) {
            let inked = rows
                .clone()
                .any(|y| self.pixels[(y * page_width + x) as usize] < INKED);
            if inked {
                let from = (x - gap - x0).max(0);
                let to = (x + gap + 1 - x0).min(span as i32);
                for b in from..to {
                    blocked[b as usize] = true;
                }
            }
        }
        // Rows start at x0, as `blocked` does.
        self.each_row(left, top, width, height, |row, _, _, _| {
            for (px, blocked) in row.iter_mut().zip(&blocked) {
                if !blocked {
                    *px = 0;
                }
            }
        });
    }

    /// Copies a gray image onto the page.
    pub(crate) fn copy(&mut self, left: i32, top: i32, width: u32, height: u32, gray: &[u8]) {
        self.each_row(left, top, width, height, |row, src_x, src_y, len| {
            row.copy_from_slice(&gray[src_y * width as usize + src_x..][..len]);
        });
    }

    /// Calls `f` with each visible row of a rectangle, clipped to the page:
    /// the page's pixels, the source column and row they start at, and how
    /// many there are.
    fn each_row(
        &mut self,
        left: i32,
        top: i32,
        width: u32,
        height: u32,
        mut f: impl FnMut(&mut [u8], usize, usize, usize),
    ) {
        let x0 = left.max(0);
        let y0 = top.max(0);
        let x1 = (i64::from(left) + i64::from(width)).min(i64::from(self.width)) as i32;
        let y1 = (i64::from(top) + i64::from(height)).min(i64::from(self.height)) as i32;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let len = (x1 - x0) as usize;
        let src_x = (x0 - left) as usize;
        for y in y0..y1 {
            let at = y as usize * self.width as usize + x0 as usize;
            f(
                &mut self.pixels[at..at + len],
                src_x,
                (y - top) as usize,
                len,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_is_clipped_to_the_page() {
        let mut canvas = Canvas::new(4, 3);
        canvas.copy(-2, -1, 4, 3, &[0; 12]);
        canvas.ink(3, 2, 3, 3, &[255; 9]);
        #[rustfmt::skip]
        assert_eq!(canvas.pixels(), &[
            0, 0, 255, 255,
            0, 0, 255, 255,
            255, 255, 255, 0,
        ]);
    }

    #[test]
    fn underlines_leave_a_gap_around_ink() {
        let mut canvas = Canvas::new(10, 3);
        // A descender in column 5, crossing the line's row.
        canvas.ink(5, 0, 1, 3, &[255; 3]);
        canvas.underline(-2, 1, 14, 1, 1);
        assert_eq!(
            &canvas.pixels()[10..20],
            &[0, 0, 0, 0, 255, 0, 255, 0, 0, 0]
        );
        assert_eq!(canvas.pixels()[0], 255);
    }

    #[test]
    fn ink_blends() {
        let mut canvas = Canvas::new(2, 1);
        canvas.ink(0, 0, 2, 1, &[0, 128]);
        assert_eq!(canvas.pixels(), &[255, 127]);
    }
}
