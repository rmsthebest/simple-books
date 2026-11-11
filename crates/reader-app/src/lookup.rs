//! What a finger resting on a word shows: the word outlined, and beside it
//! a panel with its Swedish translations from the English–Swedish
//! dictionary ([`dictionary`]).
//!
//! The panel goes on the side of the word with more room, just below or
//! above it, never over it. For each entry it shows the word and its class,
//! then each sense's translations, and one example. What doesn't fit is
//! left out. The last line credits Folkets lexikon, whose licence asks for
//! it. Any tap closes it.

use dictionary::Entry;
use reader_core::{Canvas, Typesetter, WordArea};

use crate::ui::{self, BORDER, Rect};

/// What looking a word up found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Found {
    Entries(Vec<Entry>),
    /// Not in the dictionary: the word as it was looked up.
    Nothing(String),
    /// No dictionary to look in, and why.
    NoDictionary(String),
}

/// Between the word and the panel.
const GAP: f32 = 24.0;
/// Between the panel and the screen's edges.
const EDGE: f32 = 32.0;
/// Inside the panel's outline.
const PADDING: f32 = 32.0;
/// Around the word, inside its outline.
const OUTLINE_PADDING: f32 = 6.0;
const WORD_SIZE: f32 = 48.0;
const CLASS_SIZE: f32 = 34.0;
const SENSE_SIZE: f32 = 38.0;
const EXAMPLE_SIZE: f32 = 32.0;
const CREDIT_SIZE: f32 = 26.0;
/// A sense's lines are indented, and an example's more.
const SENSE_INDENT: f32 = 28.0;
const EXAMPLE_INDENT: f32 = 56.0;
/// Lines are this many times their text size apart.
const LEADING: f32 = 1.3;
pub(crate) const CREDIT: &str = "Folkets lexikon, KTH · CC BY-SA 2.5";

/// One line of the panel: pieces of text side by side, each with its size
/// and weight.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    pieces: Vec<(String, f32, f32)>,
    indent: f32,
    /// Extra space above it.
    space: f32,
}

impl Line {
    fn new(text: String, size: f32, weight: f32, indent: f32) -> Self {
        Self {
            pieces: vec![(text, size, weight)],
            indent,
            space: 0.0,
        }
    }

    fn height(&self) -> f32 {
        let size = self.pieces.iter().map(|p| p.1).fold(0.0, f32::max);
        self.space + size * LEADING
    }
}

/// Outlines `word` on `canvas` and draws the panel for `found` beside it.
/// Returns where the panel went.
pub(crate) fn draw(
    canvas: &mut Canvas,
    ui: &mut Typesetter,
    word: &WordArea,
    found: &Found,
) -> Rect {
    outline(canvas, word);
    let width = canvas.width() as f32;
    let height = canvas.height() as f32;
    let room = width - 2.0 * EDGE - 2.0 * PADDING;
    let lines = lines(ui, found, room);
    let credit = Line::new(CREDIT.to_owned(), CREDIT_SIZE, 400.0, 0.0);
    let credit_height = credit.height() + 8.0;

    // Below the word if it's in the top half, else above it.
    let below = word.top + word.bottom < height;
    let (start, end) = if below {
        (word.bottom + OUTLINE_PADDING + GAP, height - EDGE)
    } else {
        (EDGE, word.top - OUTLINE_PADDING - GAP)
    };
    let available = end - start - 2.0 * PADDING - credit_height;
    let mut used = 0.0;
    let mut shown = 0;
    for line in &lines {
        if used + line.height() > available {
            break;
        }
        used += line.height();
        shown += 1;
    }
    let panel_height = used + credit_height + 2.0 * PADDING;
    let panel = Rect {
        left: EDGE,
        top: if below { start } else { end - panel_height },
        width: width - 2.0 * EDGE,
        height: panel_height,
    };
    ui::frame(canvas, panel);
    let mut y = panel.top + PADDING;
    for line in lines.iter().take(shown) {
        draw_line(canvas, ui, line, panel.left + PADDING, y);
        y += line.height();
    }
    let rule = y + 4.0;
    canvas.fill(
        (panel.left + PADDING) as i32,
        rule as i32,
        room as u32,
        1,
        160,
    );
    draw_line(canvas, ui, &credit, panel.left + PADDING, rule + 8.0);
    panel
}

/// A box `BORDER` thick around the word, leaving the word as it is.
fn outline(canvas: &mut Canvas, word: &WordArea) {
    let left = (word.left - OUTLINE_PADDING - BORDER as f32).round() as i32;
    let top = (word.top - BORDER as f32).round() as i32;
    let right = (word.right + OUTLINE_PADDING + BORDER as f32).round() as i32;
    let bottom = (word.bottom + BORDER as f32).round() as i32;
    let (width, height) = ((right - left).max(0) as u32, (bottom - top).max(0) as u32);
    canvas.fill(left, top, width, BORDER, 0);
    canvas.fill(left, bottom - BORDER as i32, width, BORDER, 0);
    canvas.fill(left, top, BORDER, height, 0);
    canvas.fill(right - BORDER as i32, top, BORDER, height, 0);
}

/// The line's top at `top`: its baseline goes a line's ascent below.
fn draw_line(canvas: &mut Canvas, ui: &mut Typesetter, line: &Line, left: f32, top: f32) {
    let size = line.pieces.iter().map(|p| p.1).fold(0.0, f32::max);
    let baseline = top + line.space + size * 0.95;
    let mut x = left + line.indent;
    for (text, size, weight) in &line.pieces {
        ui.draw_label(canvas, text, (x, baseline), *size, *weight);
        x += ui.label_width(text, *size, *weight);
    }
}

/// Everything there is to show for `found`, best first, in lines no wider
/// than `room`.
fn lines(ui: &mut Typesetter, found: &Found, room: f32) -> Vec<Line> {
    let mut out = Vec::new();
    let mut wrapped = |out: &mut Vec<Line>, text: &str, size, weight, indent, most| {
        for text in ui::wrap(ui, text, size, weight, room - indent, most) {
            out.push(Line::new(text, size, weight, indent));
        }
    };
    match found {
        Found::Entries(entries) => {
            for (index, entry) in entries.iter().enumerate() {
                let mut heading = Line::new(entry.word.clone(), WORD_SIZE, 700.0, 0.0);
                if !entry.class.is_empty() {
                    heading
                        .pieces
                        .push((format!("   {}", entry.class), CLASS_SIZE, 400.0));
                }
                if index > 0 {
                    heading.space = 16.0;
                }
                out.push(heading);
                let numbered = entry.senses.len() > 1;
                let mut example = None;
                for (number, sense) in entry.senses.iter().enumerate() {
                    let meaning = match (sense.translations.is_empty(), sense.note.is_empty()) {
                        (false, true) => sense.translations.clone(),
                        (false, false) => format!("{} ({})", sense.translations, sense.note),
                        (true, false) => sense.note.clone(),
                        (true, true) => match &sense.example {
                            Some((english, swedish)) => format!("{english}: {swedish}"),
                            None => continue,
                        },
                    };
                    let text = if numbered {
                        format!("{}. {meaning}", number + 1)
                    } else {
                        meaning
                    };
                    wrapped(&mut out, &text, SENSE_SIZE, 400.0, SENSE_INDENT, 3);
                    if example.is_none() && !sense.translations.is_empty() {
                        example.clone_from(&sense.example);
                    }
                }
                if let Some((english, swedish)) = example {
                    let text = format!("“{english}”: {swedish}");
                    wrapped(&mut out, &text, EXAMPLE_SIZE, 400.0, EXAMPLE_INDENT, 2);
                }
            }
        }
        Found::Nothing(word) => {
            out.push(Line::new(word.clone(), WORD_SIZE, 700.0, 0.0));
            wrapped(
                &mut out,
                "Not in the English–Swedish dictionary.",
                SENSE_SIZE,
                400.0,
                0.0,
                2,
            );
        }
        Found::NoDictionary(why) => {
            out.push(Line::new("No dictionary".to_owned(), WORD_SIZE, 700.0, 0.0));
            wrapped(&mut out, why, SENSE_SIZE, 400.0, 0.0, 4);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use dictionary::Sense;
    use reader_core::Settings;

    use super::*;

    fn run() -> Vec<Entry> {
        let sense = |translations: &str, example: Option<(&str, &str)>| Sense {
            translations: translations.to_owned(),
            note: String::new(),
            example: example.map(|(e, s)| (e.to_owned(), s.to_owned())),
        };
        vec![Entry {
            word: "run".to_owned(),
            class: "verb".to_owned(),
            senses: (0..30)
                .map(|n| {
                    sense(
                        &format!("springa, löpa, rusa {n}"),
                        Some(("the buses don't run on Sundays", "bussarna går inte")),
                    )
                })
                .collect(),
        }]
    }

    fn word(top: f32) -> WordArea {
        WordArea {
            left: 300.0,
            top,
            right: 420.0,
            bottom: top + 60.0,
            text: "running,".to_owned(),
        }
    }

    #[test]
    fn the_panel_goes_beside_the_word_and_inside_the_screen() {
        let settings = Settings::default();
        let mut ui = Typesetter::new(settings.clone(), Some("en"));
        let screen = Rect {
            left: 0.0,
            top: 0.0,
            width: settings.width as f32,
            height: settings.height as f32,
        };
        for top in [80.0, 600.0, 800.0, 1300.0] {
            let mut canvas = Canvas::new(settings.width, settings.height);
            let word = word(top);
            let panel = draw(&mut canvas, &mut ui, &word, &Found::Entries(run()));
            let outlined = Rect {
                left: word.left - OUTLINE_PADDING - 3.0,
                top: word.top - 3.0,
                width: word.right - word.left + 2.0 * OUTLINE_PADDING + 6.0,
                height: word.bottom - word.top + 6.0,
            };
            assert!(!panel.overlaps(&outlined), "{panel:?} over {word:?}");
            assert!(
                panel.top >= 0.0 && panel.bottom() <= screen.height,
                "{panel:?}"
            );
            // Thirty senses don't fit: it fills the room it has.
            assert!(panel.height > screen.height / 2.0 - 120.0, "{panel:?}");
        }
    }

    #[test]
    fn a_short_answer_has_a_small_panel() {
        let settings = Settings::default();
        let mut ui = Typesetter::new(settings.clone(), Some("en"));
        let mut canvas = Canvas::new(settings.width, settings.height);
        let panel = draw(
            &mut canvas,
            &mut ui,
            &word(100.0),
            &Found::Nothing("running".to_owned()),
        );
        assert!(panel.height < 300.0, "{panel:?}");
        assert!(panel.top > 160.0, "below the word: {panel:?}");
    }

    #[test]
    fn each_entry_has_its_word_class_senses_and_one_example() {
        let mut ui = Typesetter::new(Settings::default(), Some("en"));
        let mut entries = run();
        entries[0].senses.truncate(2);
        entries[0].senses[1].example = None;
        let lines = lines(&mut ui, &Found::Entries(entries), 2000.0);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.pieces.iter().map(|p| p.0.as_str()).collect())
            .collect();
        assert_eq!(
            text,
            [
                "run   verb",
                "1. springa, löpa, rusa 0",
                "2. springa, löpa, rusa 1",
                "“the buses don't run on Sundays”: bussarna går inte"
            ]
        );
    }
}
