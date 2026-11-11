//! The screen that comes up when the power button is held: switch off,
//! restart, or go to Nickel. With nothing tapped it goes to Nickel, so
//! holding the button is still the way out when taps don't work.

use reader_core::{Canvas, Typesetter};

use crate::ui::{self, BUTTON_HEIGHT, GAP, HEADER, LABEL_SIZE, MARGIN, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    PowerOff,
    Restart,
    Nickel,
    Cancel,
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    buttons: Vec<(Rect, Action, &'static str)>,
}

const TEXT_TOP: f32 = HEADER + 96.0;
const LINE: f32 = 60.0;
/// Where the first button goes, below the text.
const BUTTONS_TOP: f32 = HEADER + 400.0;

const SAID: [&str; 2] = [
    "Switch the Kobo off, restart it, or go to Nickel.",
    "If nothing is tapped, Nickel starts in 15 seconds.",
];

impl Layout {
    pub(crate) fn new(width: u32) -> Self {
        let room = width as f32 - 2.0 * MARGIN;
        let buttons = [
            (Action::PowerOff, "Power off"),
            (Action::Restart, "Restart"),
            (Action::Nickel, "Go to Nickel"),
            (Action::Cancel, "Cancel"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (action, label))| {
            // A wider gap before Cancel, which isn't one of the three.
            let extra = if action == Action::Cancel {
                2.0 * GAP
            } else {
                0.0
            };
            let rect = Rect {
                left: MARGIN,
                top: BUTTONS_TOP + i as f32 * (BUTTON_HEIGHT + 2.0 * GAP) + extra,
                width: room,
                height: BUTTON_HEIGHT,
            };
            (rect, action, label)
        })
        .collect();
        Self { buttons }
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons
            .iter()
            .find(|(rect, ..)| rect.contains(x, y))
            .map(|(_, action, _)| *action)
    }

    pub(crate) fn draw(&self, canvas: &mut Canvas, ui: &mut Typesetter, status: &str) {
        canvas.clear();
        ui::header(canvas, ui, "Power", status);
        let room = canvas.width() as f32 - 2.0 * MARGIN;
        let mut y = TEXT_TOP;
        for text in SAID {
            for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 3) {
                ui.draw_label(canvas, &line, (MARGIN, y), LABEL_SIZE, 400.0);
                y += LINE;
            }
            y += LINE / 2.0;
        }
        for (rect, _, label) in &self.buttons {
            ui::button(canvas, ui, *rect, label);
        }
    }
}

/// What's left on the panel once the Kobo is off, or while it restarts:
/// one line in the middle of a blank screen, and a smaller one below.
pub(crate) fn draw_final(canvas: &mut Canvas, ui: &mut Typesetter, said: &str, below: &str) {
    canvas.clear();
    let (width, height) = (canvas.width() as f32, canvas.height() as f32);
    for (text, size, weight, y) in [
        (said, 64.0, 600.0, height / 2.0),
        (below, LABEL_SIZE, 400.0, height / 2.0 + 96.0),
    ] {
        let text_width = ui.label_width(text, size, weight);
        ui.draw_label(canvas, text, ((width - text_width) / 2.0, y), size, weight);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_can_be_tapped() {
        let layout = Layout::new(1072);
        for (rect, action, _) in &layout.buttons {
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(layout.hit(middle.0, middle.1), Some(*action));
            assert!(rect.right() <= 1072.0 && rect.bottom() <= 1448.0);
        }
        assert_eq!(layout.hit(536.0, 300.0), None);
    }
}
