//! The settings screen, from the home screen: how pages look, the light,
//! and the turn guard, each a row of − and + like the reader's menu, with a
//! sample paragraph at the top laid out as a book's page would be.
//!
//! The reader's menu changes some of the same settings over the page; both
//! step them through [`Setting::step`].

use std::io::Cursor;

use book_format::{About, Align, Block, BlockKind, BookReader, Chapter, Limits, Position, Span};
use reader_core::{Canvas, Reader, Settings, Typesetter};

use crate::{
    Light, Preferences,
    menu::Step,
    ui::{self, BUTTON_HEIGHT, HEADER, LABEL_SIZE, MARGIN, Rect},
};

/// The sample's part of the screen, under the header.
const PREVIEW_TOP: f32 = HEADER + 3.0;
const PREVIEW_HEIGHT: f32 = 330.0;
const ROW: f32 = 94.0;
const ROWS_TOP: f32 = PREVIEW_TOP + PREVIEW_HEIGHT + 12.0;
const TOGGLE_HEIGHT: f32 = 88.0;
/// Between a row's − and +.
const VALUE_WIDTH: f32 = 180.0;

/// A setting stepped with − and +.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Setting {
    TextSize,
    LineSpacing,
    Margins,
    Darkness,
    Contrast,
    Light,
    Warmth,
    TurnGuard,
}

/// The rows, in order, with their names.
const ROWS: [(Setting, &str); 8] = [
    (Setting::TextSize, "Text size"),
    (Setting::LineSpacing, "Line spacing"),
    (Setting::Margins, "Margins"),
    (Setting::Darkness, "Darkness"),
    (Setting::Contrast, "Contrast"),
    (Setting::Light, "Light"),
    (Setting::Warmth, "Warmth"),
    (Setting::TurnGuard, "Turn guard"),
];

/// What a step changed, so the caller knows what to do again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Changed {
    Nothing,
    /// Where lines and pages break: the book is laid out again.
    Layout,
    /// Only how text is drawn: pages are drawn again.
    Drawing,
    /// The front light, which the device is told.
    Light,
    /// The turn guard's window.
    Guard,
}

impl Setting {
    /// Takes one step, within the setting's range.
    pub(crate) fn step(self, step: Step, p: &mut Preferences, light: &mut Light) -> Changed {
        let up = step == Step::Up;
        let before = (p.clone(), *light);
        let changed = match self {
            Setting::TextSize => {
                p.font_size = (p.font_size + if up { 4.0 } else { -4.0 }).clamp(24.0, 80.0);
                Changed::Layout
            }
            Setting::LineSpacing => {
                p.line_height =
                    tenths((p.line_height + if up { 0.1 } else { -0.1 }).clamp(1.0, 2.2));
                Changed::Layout
            }
            Setting::Margins => {
                p.margin = if up {
                    (p.margin + 16).min(192)
                } else {
                    p.margin.saturating_sub(16).max(16)
                };
                Changed::Layout
            }
            Setting::Darkness => {
                p.weight = (p.weight + if up { 50.0 } else { -50.0 }).clamp(300.0, 700.0);
                Changed::Layout
            }
            Setting::Contrast => {
                p.gamma = tenths((p.gamma + if up { 0.2 } else { -0.2 }).clamp(0.6, 3.0));
                Changed::Drawing
            }
            Setting::Light => {
                light.brightness = stepped(light.brightness, up, 10, 100);
                Changed::Light
            }
            Setting::Warmth => {
                light.warmth = stepped(light.warmth, up, 1, 10);
                Changed::Light
            }
            Setting::TurnGuard => {
                p.turn_guard_ms = if up {
                    (p.turn_guard_ms + 100).min(1000)
                } else {
                    p.turn_guard_ms.saturating_sub(100)
                };
                Changed::Guard
            }
        };
        if before == (p.clone(), *light) {
            Changed::Nothing
        } else {
            changed
        }
    }

    /// The setting's value as shown.
    pub(crate) fn value(self, p: &Preferences, light: Light) -> String {
        match self {
            Setting::TextSize => format!("{}", p.font_size),
            Setting::LineSpacing => format!("{:.1}", p.line_height),
            Setting::Margins => p.margin.to_string(),
            Setting::Darkness => format!("{}", p.weight),
            Setting::Contrast => format!("{:.1}", p.gamma),
            Setting::Light => format!("{}%", light.brightness),
            Setting::Warmth => light.warmth.to_string(),
            Setting::TurnGuard if p.turn_guard_ms == 0 => "Off".to_owned(),
            Setting::TurnGuard => format!("{} ms", p.turn_guard_ms),
        }
    }
}

fn tenths(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

fn stepped(value: u8, up: bool, by: u8, max: u8) -> u8 {
    if up {
        value.saturating_add(by).min(max)
    } else {
        value.saturating_sub(by)
    }
}

/// An on and off setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Toggle {
    Justify,
    Hyphenate,
}

impl Toggle {
    pub(crate) fn flip(self, p: &mut Preferences) {
        match self {
            Toggle::Justify => p.justify = !p.justify,
            Toggle::Hyphenate => p.hyphenate = !p.hyphenate,
        }
    }

    fn is_on(self, p: &Preferences) -> bool {
        match self {
            Toggle::Justify => p.justify,
            Toggle::Hyphenate => p.hyphenate,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Step(Setting, Step),
    Toggle(Toggle),
    Done,
}

/// Where everything on the settings screen is. It's the same whatever the
/// values, so it's made once.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    buttons: Vec<(Rect, Action, &'static str)>,
}

impl Layout {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width as f32, height as f32);
        let content = width - 2.0 * MARGIN;
        let mut buttons = Vec::new();
        let side = ROW - 12.0;
        for (i, (setting, _)) in ROWS.iter().enumerate() {
            let top = ROWS_TOP + i as f32 * ROW + 6.0;
            let plus = width - MARGIN - side;
            // Room between them for the widest value, "1000 ms".
            let minus = plus - side - VALUE_WIDTH;
            for (left, step, label) in [(minus, Step::Down, "−"), (plus, Step::Up, "+")] {
                let rect = Rect {
                    left,
                    top,
                    width: side,
                    height: side,
                };
                buttons.push((rect, Action::Step(*setting, step), label));
            }
        }
        let toggles_top = ROWS_TOP + ROWS.len() as f32 * ROW + 8.0;
        let toggles = ui::row(MARGIN, toggles_top, content, TOGGLE_HEIGHT, &[1.0, 1.0]);
        buttons.push((toggles[0], Action::Toggle(Toggle::Justify), "Justified"));
        buttons.push((toggles[1], Action::Toggle(Toggle::Hyphenate), "Hyphenated"));
        let done = Rect {
            left: MARGIN,
            top: height - BUTTON_HEIGHT - 24.0,
            width: content,
            height: BUTTON_HEIGHT,
        };
        buttons.push((done, Action::Done, "Done"));
        Self { buttons }
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons
            .iter()
            .find(|(rect, ..)| rect.contains(x, y))
            .map(|(_, action, _)| *action)
    }

    /// Draws the screen, with `preview`, a page of the sample paragraph laid
    /// out with these settings, at the top.
    pub(crate) fn draw(
        &self,
        canvas: &mut Canvas,
        ui: &mut Typesetter,
        p: &Preferences,
        light: Light,
        preview: Option<&Canvas>,
        battery: &str,
    ) {
        canvas.clear();
        ui::header(canvas, ui, "Settings", battery);
        if let Some(page) = preview {
            // The page from its top margin down, at its real size and with
            // its side margins, so it looks as a book will.
            let width = page.width().min(canvas.width());
            let from = p.settings().margin_top.saturating_sub(16) as usize;
            let rows = whole_lines(page, from, PREVIEW_HEIGHT as usize);
            let start = from * page.width() as usize;
            if let Some(pixels) = page
                .pixels()
                .get(start..start + rows * page.width() as usize)
            {
                canvas.copy(0, PREVIEW_TOP as i32, width, rows as u32, pixels);
            }
        }
        let rule = (PREVIEW_TOP + PREVIEW_HEIGHT) as i32;
        canvas.fill(0, rule, canvas.width(), ui::BORDER, 0);

        for (i, (setting, name)) in ROWS.iter().enumerate() {
            let baseline = ROWS_TOP + i as f32 * ROW + ROW / 2.0 + LABEL_SIZE * 0.33;
            ui.draw_label(canvas, name, (MARGIN, baseline), LABEL_SIZE, 400.0);
            let minus = self.buttons[2 * i].0;
            let plus = self.buttons[2 * i + 1].0;
            let value = setting.value(p, light);
            let middle = f32::midpoint(minus.right(), plus.left);
            let value_width = ui.label_width(&value, LABEL_SIZE, 400.0);
            let at = (middle - value_width / 2.0, baseline);
            ui.draw_label(canvas, &value, at, LABEL_SIZE, 400.0);
        }
        for (rect, action, label) in &self.buttons {
            let on = match action {
                Action::Toggle(toggle) => toggle.is_on(p),
                _ => false,
            };
            ui::toggle(canvas, ui, *rect, label, on);
        }
    }
}

/// How many of the `most` rows from `from` to show so no line is cut
/// through: up to the last blank row, between two lines.
fn whole_lines(page: &Canvas, from: usize, most: usize) -> usize {
    let width = page.width() as usize;
    let blank = |y: usize| {
        page.pixels()
            .get(y * width..(y + 1) * width)
            .is_none_or(|row| row.iter().all(|g| *g > 240))
    };
    (1..=most)
        .rev()
        .find(|rows| blank(from + rows - 1))
        .unwrap_or(0)
}

/// The sample paragraph, with the soft hyphens the server would put in.
const SAMPLE: &str = "It is a truth uni\u{ad}ver\u{ad}sal\u{ad}ly ac\u{ad}knowl\u{ad}edged, \
    that a single man in pos\u{ad}ses\u{ad}sion of a good for\u{ad}tune must be in want of a \
    wife. How\u{ad}ever little known the feel\u{ad}ings or views of such a man may be on his \
    first en\u{ad}ter\u{ad}ing a neigh\u{ad}bour\u{ad}hood, this truth is so well fixed in \
    the minds of the sur\u{ad}round\u{ad}ing fam\u{ad}i\u{ad}lies, that he is con\u{ad}sid\
    \u{ad}ered the right\u{ad}ful prop\u{ad}er\u{ad}ty of some one or other of their \
    daugh\u{ad}ters.";

/// The first page of a book holding only the sample, laid out with
/// `settings`.
pub(crate) fn preview(settings: &Settings) -> Option<Canvas> {
    let paragraph = Block {
        kind: BlockKind::Paragraph { indent_first: true },
        indent: 0,
        align: Align::Default,
        spans: vec![Span::plain(SAMPLE.to_owned())],
    };
    let mut bytes = Cursor::new(Vec::new());
    book_format::write(
        &mut bytes,
        About {
            title: "Sample".to_owned(),
            language: Some("en".to_owned()),
            ..About::default()
        },
        Vec::new(),
        &[Chapter {
            blocks: vec![paragraph],
        }],
        &[],
    )
    .ok()?;
    bytes.set_position(0);
    let book = BookReader::open(bytes, Limits::default()).ok()?;
    let start = Position {
        chapter: 0,
        block: 0,
        offset: 0,
    };
    let mut reader = Reader::open(book, settings.clone(), 0, start, None).ok()?;
    reader.page().ok()?.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light() -> Light {
        Light {
            brightness: 30,
            warmth: 4,
        }
    }

    #[test]
    fn steps_stay_in_range_and_a_step_at_the_end_changes_nothing() {
        let mut p = Preferences::default();
        let mut light = light();
        for _ in 0..40 {
            Setting::LineSpacing.step(Step::Up, &mut p, &mut light);
            Setting::TurnGuard.step(Step::Down, &mut p, &mut light);
            Setting::Light.step(Step::Up, &mut p, &mut light);
        }
        assert_eq!(p.line_height, 2.2);
        assert_eq!(p.turn_guard_ms, 0);
        assert_eq!(Setting::TurnGuard.value(&p, light), "Off");
        assert_eq!(light.brightness, 100);
        assert_eq!(
            Setting::LineSpacing.step(Step::Up, &mut p, &mut light),
            Changed::Nothing
        );
        assert_eq!(
            Setting::LineSpacing.step(Step::Down, &mut p, &mut light),
            Changed::Layout
        );
        assert_eq!(Setting::LineSpacing.value(&p, light), "2.1");
        assert_eq!(
            Setting::Contrast.step(Step::Up, &mut p, &mut light),
            Changed::Drawing
        );
        for _ in 0..40 {
            Setting::Margins.step(Step::Down, &mut p, &mut light);
        }
        assert_eq!(p.margin, 16);
    }

    #[test]
    fn the_preview_follows_the_settings() {
        let p = Preferences::default();
        let page = preview(&p.settings()).unwrap();
        let ink = |page: &Canvas| page.pixels().iter().filter(|g| **g < 128).count();
        let mut bigger = p.clone();
        bigger.font_size += 12.0;
        let big = preview(&bigger.settings()).unwrap();
        assert!(ink(&big) > ink(&page));
        // A wider margin leaves the first columns past the old one blank.
        let mut wide = p.clone();
        wide.margin = 160;
        let narrow_ink = column_ink(&page, 100);
        assert!(narrow_ink > 0);
        assert_eq!(column_ink(&preview(&wide.settings()).unwrap(), 100), 0);
    }

    fn column_ink(page: &Canvas, x: usize) -> usize {
        let width = page.width() as usize;
        page.pixels()
            .chunks(width)
            .filter(|row| row[x] < 128)
            .count()
    }

    #[test]
    fn the_preview_stops_between_lines() {
        let page = preview(&Preferences::default().settings()).unwrap();
        let from = 56;
        let rows = whole_lines(&page, from, 330);
        assert!(rows > 200 && rows <= 330, "{rows}");
        let width = page.width() as usize;
        let last = &page.pixels()[(from + rows - 1) * width..(from + rows) * width];
        assert!(last.iter().all(|g| *g > 240));
    }

    #[test]
    fn nothing_overlaps_and_each_button_is_what_it_shows() {
        let layout = Layout::new(1072, 1448);
        assert_eq!(layout.buttons.len(), 2 * ROWS.len() + 3);
        for (i, (rect, action, _)) in layout.buttons.iter().enumerate() {
            assert!(rect.top > PREVIEW_TOP + PREVIEW_HEIGHT, "{action:?}");
            assert!(
                rect.bottom() <= 1448.0 && rect.right() <= 1072.0,
                "{action:?}"
            );
            for (other, other_action, _) in &layout.buttons[i + 1..] {
                assert!(!rect.overlaps(other), "{action:?} and {other_action:?}");
            }
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(layout.hit(middle.0, middle.1), Some(*action));
        }
    }
}
