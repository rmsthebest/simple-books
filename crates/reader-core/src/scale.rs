//! Scaling gray images to the size they're placed at.
//!
//! Each output pixel is the average of the source pixels it covers, part
//! pixels at its edges counted in proportion: a box filter, the right one
//! for shrinking. Images only ever shrink here (they're never enlarged past
//! their size, and the converter already fits them to the screen), so it's
//! mostly a few percent either way, as for a cover on a page with margins.
//!
//! It works in whole numbers, one axis at a time, and runs about ten times
//! faster than the `image` crate's general resize. On the Clara BW that took
//! most of the 0.7 s a full-page cover needed.

/// The weights of one output pixel's sources add up to this.
const ONE: u32 = 1 << 12;

/// For each output pixel along one axis, the first source pixel it covers
/// and how much of each it takes.
struct Spans {
    starts: Vec<usize>,
    /// Where each output pixel's weights are in `weights`: from its entry
    /// to the next one's.
    offsets: Vec<usize>,
    weights: Vec<u32>,
}

impl Spans {
    fn new(from: usize, to: usize) -> Self {
        let mut spans = Self {
            starts: Vec::with_capacity(to),
            offsets: Vec::with_capacity(to + 1),
            weights: Vec::new(),
        };
        let ratio = from as f64 / to as f64;
        for out in 0..to {
            let low = out as f64 * ratio;
            let high = (out as f64 + 1.0) * ratio;
            let first = low.floor() as usize;
            let last = (high.ceil() as usize).clamp(first + 1, from);
            spans.starts.push(first);
            spans.offsets.push(spans.weights.len());
            // Rounding the running total, rather than each weight, makes the
            // weights add up to exactly ONE.
            let mut covered = 0.0;
            let mut given = 0;
            for source in first..last {
                let overlap = high.min(source as f64 + 1.0) - low.max(source as f64);
                covered += overlap.max(0.0) / (high - low);
                let total = (covered * f64::from(ONE)).round() as u32;
                spans.weights.push(total.min(ONE) - given);
                given = total.min(ONE);
            }
            if let Some(last) = spans.weights.last_mut() {
                *last += ONE - given;
            }
        }
        spans.offsets.push(spans.weights.len());
        spans
    }

    fn get(&self, out: usize) -> (usize, &[u32]) {
        (
            self.starts[out],
            &self.weights[self.offsets[out]..self.offsets[out + 1]],
        )
    }
}

fn round(sum: u32) -> u8 {
    ((sum + ONE / 2) / ONE).min(255) as u8
}

/// Scales `pixels`, `width` by `height`, one byte a pixel, to `to_width`
/// by `to_height`.
pub(crate) fn scale(
    pixels: &[u8],
    (width, height): (usize, usize),
    (to_width, to_height): (usize, usize),
) -> Vec<u8> {
    if width == 0 || height == 0 || to_width == 0 || to_height == 0 {
        return vec![255; to_width * to_height];
    }
    // Across each source row first, then down the columns.
    let across = Spans::new(width, to_width);
    let mut narrow = vec![0u8; to_width * height];
    for (row, out) in pixels
        .chunks_exact(width)
        .zip(narrow.chunks_exact_mut(to_width))
    {
        for (x, pixel) in out.iter_mut().enumerate() {
            let (start, weights) = across.get(x);
            let sum: u32 = row[start..]
                .iter()
                .zip(weights)
                .map(|(p, w)| u32::from(*p) * w)
                .sum();
            *pixel = round(sum);
        }
    }
    let down = Spans::new(height, to_height);
    let mut scaled = vec![0u8; to_width * to_height];
    let mut sums = vec![0u32; to_width];
    for (y, out) in scaled.chunks_exact_mut(to_width).enumerate() {
        let (start, weights) = down.get(y);
        sums.fill(0);
        for (row, weight) in narrow.chunks_exact(to_width).skip(start).zip(weights) {
            for (sum, pixel) in sums.iter_mut().zip(row) {
                *sum += u32::from(*pixel) * weight;
            }
        }
        for (pixel, sum) in out.iter_mut().zip(&sums) {
            *pixel = round(*sum);
        }
    }
    scaled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_add_up_for_any_ratio() {
        for (from, to) in [(955, 860), (1448, 1304), (10, 3), (3, 10), (7, 7), (1, 5)] {
            let spans = Spans::new(from, to);
            for out in 0..to {
                let (start, weights) = spans.get(out);
                assert_eq!(weights.iter().sum::<u32>(), ONE, "{from}→{to} at {out}");
                assert!(start + weights.len() <= from);
            }
        }
    }

    #[test]
    fn flat_gray_stays_flat() {
        let pixels = vec![137; 955 * 40];
        let scaled = scale(&pixels, (955, 40), (860, 36));
        assert_eq!(scaled.len(), 860 * 36);
        assert!(scaled.iter().all(|p| *p == 137));
    }

    #[test]
    fn halving_averages_each_square() {
        #[rustfmt::skip]
        let pixels = [
            0, 0, 255, 255,
            0, 0, 255, 255,
            100, 200, 10, 10,
            100, 200, 30, 30,
        ];
        assert_eq!(scale(&pixels, (4, 4), (2, 2)), [0, 255, 150, 20]);
    }

    #[test]
    fn the_same_size_is_a_copy() {
        let pixels: Vec<u8> = (0..=255).collect();
        assert_eq!(scale(&pixels, (16, 16), (16, 16)), pixels);
    }

    #[test]
    fn a_sharp_edge_blends_only_where_it_falls_inside_a_pixel() {
        // Black then white, 9 pixels to 6: the fifth pixel straddles.
        let pixels = [0, 0, 0, 0, 255, 255, 255, 255, 255];
        let scaled = scale(&pixels, (9, 1), (6, 1));
        assert_eq!(&scaled[..2], [0, 0]);
        assert!(scaled[2] > 0 && scaled[2] < 255, "{scaled:?}");
        assert_eq!(&scaled[3..], [255, 255, 255]);
    }
}
