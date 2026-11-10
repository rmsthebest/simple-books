//! What the menu, the home screen and the library draw with: rectangles,
//! outlined buttons, and text shortened or wrapped to fit.

use reader_core::{Canvas, Typesetter};

/// A button's outline, in pixels.
pub(crate) const BORDER: u32 = 3;
/// Button labels, and most other text outside the book.
pub(crate) const LABEL_SIZE: f32 = 40.0;
/// The space at each side of the home and library screens.
pub(crate) const MARGIN: f32 = 64.0;
/// The bar along the top of the home and library screens.
pub(crate) const HEADER: f32 = 104.0;
/// The buttons along the bottom of the home and library screens.
pub(crate) const BUTTON_HEIGHT: f32 = 96.0;
/// Between buttons in a row.
pub(crate) const GAP: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }

    pub(crate) fn right(&self) -> f32 {
        self.left + self.width
    }

    pub(crate) fn bottom(&self) -> f32 {
        self.top + self.height
    }

    /// Whole pixels: left, top, width and height.
    pub(crate) fn pixels(&self) -> (i32, i32, u32, u32) {
        (
            self.left.round() as i32,
            self.top.round() as i32,
            self.width.round() as u32,
            self.height.round() as u32,
        )
    }

    /// Whether the two share any pixel.
    #[cfg(test)]
    pub(crate) fn overlaps(&self, other: &Rect) -> bool {
        self.left < other.right()
            && other.left < self.right()
            && self.top < other.bottom()
            && other.top < self.bottom()
    }
}

/// A black outline `BORDER` thick, white inside.
pub(crate) fn frame(canvas: &mut Canvas, rect: Rect) {
    let (left, top, width, height) = rect.pixels();
    canvas.fill(left, top, width, height, 0);
    let inset = BORDER as i32;
    canvas.fill(
        left + inset,
        top + inset,
        width.saturating_sub(2 * BORDER),
        height.saturating_sub(2 * BORDER),
        255,
    );
}

/// The screen's title on the left of the bar along the top, `right` (such
/// as the battery) on its right, and a rule under it.
pub(crate) fn header(canvas: &mut Canvas, ui: &mut Typesetter, title: &str, right: &str) {
    let width = canvas.width() as f32;
    let baseline = HEADER * 0.62;
    let right_width = ui.label_width(right, LABEL_SIZE, 400.0);
    let room = width - 2.0 * MARGIN - right_width - 32.0;
    let title = fit(ui, title, 44.0, 600.0, room);
    ui.draw_label(canvas, &title, (MARGIN, baseline), 44.0, 600.0);
    ui.draw_label(
        canvas,
        right,
        (width - MARGIN - right_width, baseline),
        LABEL_SIZE,
        400.0,
    );
    canvas.fill(0, HEADER as i32, width as u32, BORDER, 0);
}

/// Rectangles side by side from `left`, `width` across between them, each
/// taking its share of it after a [`GAP`] between each two.
pub(crate) fn row(left: f32, top: f32, width: f32, height: f32, shares: &[f32]) -> Vec<Rect> {
    let total: f32 = shares.iter().sum();
    let unit = (width - GAP * (shares.len() as f32 - 1.0)) / total;
    let mut x = left;
    shares
        .iter()
        .map(|share| {
            let rect = Rect {
                left: x,
                top,
                width: unit * share,
                height,
            };
            x += unit * share + GAP;
            rect
        })
        .collect()
}

/// An outlined button with its label in the middle. A single character,
/// such as − or +, is drawn larger.
pub(crate) fn button(canvas: &mut Canvas, ui: &mut Typesetter, rect: Rect, label: &str) {
    toggle(canvas, ui, rect, label, false);
}

/// A button that's one of a choice, such as how to sort: the chosen one is
/// gray with a heavier outline and a bolder label.
pub(crate) fn toggle(canvas: &mut Canvas, ui: &mut Typesetter, rect: Rect, label: &str, on: bool) {
    let size = if label.chars().count() == 1 {
        64.0
    } else {
        LABEL_SIZE
    };
    let weight = if on { 700.0 } else { 500.0 };
    labelled(canvas, ui, rect, label, size, weight, on);
}

/// An outlined button with `label` centred at `size`, gray inside if `on`.
pub(crate) fn labelled(
    canvas: &mut Canvas,
    ui: &mut Typesetter,
    rect: Rect,
    label: &str,
    size: f32,
    weight: f32,
    on: bool,
) {
    frame(canvas, rect);
    if on {
        let (left, top, width, height) = rect.pixels();
        let inset = 2 * BORDER;
        canvas.fill(
            left + inset as i32,
            top + inset as i32,
            width.saturating_sub(2 * inset),
            height.saturating_sub(2 * inset),
            210,
        );
    }
    let label = fit(ui, label, size, weight, rect.width - 24.0);
    let label_width = ui.label_width(&label, size, weight);
    let x = rect.left + (rect.width - label_width) / 2.0;
    let baseline = rect.top + rect.height / 2.0 + size * 0.33;
    ui.draw_label(canvas, &label, (x, baseline), size, weight);
}

/// `text`, cut short with an ellipsis if it's wider than `room`.
pub(crate) fn fit(ui: &mut Typesetter, text: &str, size: f32, weight: f32, room: f32) -> String {
    if ui.label_width(text, size, weight) <= room {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let shortened: String = chars[..keep]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_owned()
            + "…";
        if ui.label_width(&shortened, size, weight) <= room {
            return shortened;
        }
    }
    "…".to_owned()
}

/// `text` broken into at most `lines` lines no wider than `room`, at
/// spaces. What doesn't fit is cut short with an ellipsis on the last line.
pub(crate) fn wrap(
    ui: &mut Typesetter,
    text: &str,
    size: f32,
    weight: f32,
    room: f32,
    lines: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut words = text.split_whitespace().peekable();
    while let Some(first) = words.next() {
        if out.len() + 1 == lines {
            // The last line takes the rest, shortened if need be.
            let rest: Vec<&str> = std::iter::once(first).chain(words).collect();
            out.push(fit(ui, &rest.join(" "), size, weight, room));
            return out;
        }
        let mut line = first.to_owned();
        while let Some(next) = words.peek() {
            let longer = format!("{line} {next}");
            if ui.label_width(&longer, size, weight) > room {
                break;
            }
            line = longer;
            words.next();
        }
        out.push(fit(ui, &line, size, weight, room));
    }
    out
}

#[cfg(test)]
mod tests {
    use reader_core::Settings;

    use super::*;

    #[test]
    fn a_row_shares_its_width() {
        let rects = row(64.0, 10.0, 944.0, 96.0, &[1.0, 2.0, 1.0]);
        let widths: Vec<f32> = rects.iter().map(|r| r.width).collect();
        assert_eq!(widths, [228.0, 456.0, 228.0]);
        assert_eq!(rects[1].left, 64.0 + 228.0 + GAP);
        assert_eq!(rects[2].right(), 64.0 + 944.0);
    }

    #[test]
    fn long_titles_are_shortened() {
        let mut ui = Typesetter::new(Settings::default(), Some("en"));
        let long = "A Smarter Way to Learn HTML & CSS: Learn it faster. Remember it longer.";
        let short = fit(&mut ui, long, 44.0, 600.0, 600.0);
        assert!(short.ends_with('…'), "{short}");
        assert!(ui.label_width(&short, 44.0, 600.0) <= 600.0);
        assert_eq!(fit(&mut ui, "Milkman", 44.0, 600.0, 600.0), "Milkman");
    }

    #[test]
    fn wrapping_fills_lines_and_shortens_the_last() {
        let mut ui = Typesetter::new(Settings::default(), Some("en"));
        let text = "A Smarter Way to Learn HTML & CSS: Learn it faster. Remember it longer.";
        let lines = wrap(&mut ui, text, 40.0, 500.0, 400.0, 3);
        assert_eq!(lines.len(), 3);
        assert!(
            lines
                .iter()
                .all(|l| ui.label_width(l, 40.0, 500.0) <= 400.0)
        );
        assert!(lines[2].ends_with('…'), "{lines:?}");
        assert_eq!(lines[0].split(' ').next(), Some("A"));
        assert_eq!(wrap(&mut ui, "Dune", 40.0, 500.0, 400.0, 3), ["Dune"]);
        assert_eq!(
            wrap(&mut ui, "", 40.0, 500.0, 400.0, 3),
            Vec::<String>::new()
        );
    }
}
