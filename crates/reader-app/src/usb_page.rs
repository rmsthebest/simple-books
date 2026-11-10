//! The screen that comes up when the Kobo is plugged into a computer: copying
//! books over USB is Nickel's job, so this offers to hand over to it.

use reader_core::{Canvas, Typesetter};

use crate::ui::{self, BUTTON_HEIGHT, HEADER, LABEL_SIZE, MARGIN, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Leave for Nickel, which does the connecting.
    Connect,
    NotNow,
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    buttons: Vec<(Rect, Action, &'static str)>,
}

const TEXT_TOP: f32 = HEADER + 96.0;
const LINE: f32 = 60.0;

const SAID: [&str; 2] = [
    "This Kobo is plugged into a computer.",
    "Connect to copy books to or from it over USB. Your place is saved, and Nickel starts \
     and does the connecting. Simple Books in the Nickelite tab comes back here.",
];

impl Layout {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width as f32, height as f32);
        let top = height - BUTTON_HEIGHT - 24.0;
        let rects = ui::row(
            MARGIN,
            top,
            width - 2.0 * MARGIN,
            BUTTON_HEIGHT,
            &[1.0, 1.0],
        );
        let buttons = rects
            .into_iter()
            .zip([(Action::Connect, "Connect"), (Action::NotNow, "Not now")])
            .map(|(rect, (action, label))| (rect, action, label))
            .collect();
        Self { buttons }
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons
            .iter()
            .find(|(rect, ..)| rect.contains(x, y))
            .map(|(_, action, _)| *action)
    }

    pub(crate) fn draw(&self, canvas: &mut Canvas, ui: &mut Typesetter, battery: &str) {
        canvas.clear();
        ui::header(canvas, ui, "USB", battery);
        let room = canvas.width() as f32 - 2.0 * MARGIN;
        let mut y = TEXT_TOP;
        for text in SAID {
            for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 6) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_buttons_can_be_tapped() {
        let layout = Layout::new(1072, 1448);
        for (rect, action, _) in &layout.buttons {
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(layout.hit(middle.0, middle.1), Some(*action));
            assert!(rect.right() <= 1072.0 && rect.bottom() <= 1448.0);
        }
        assert_eq!(layout.hit(536.0, 400.0), None);
    }
}
