//! The screen that comes up when the Kobo is plugged into a computer, offering
//! to share its storage over USB, and the screens while it's shared
//! ([`usb_screen`]), which the platform draws with the reader stopped.

use reader_core::{Canvas, Settings, Typesetter};

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
    "Connect to copy files to or from it over USB. Your place is saved, and Simple Books \
     comes back when you eject the Kobo on the computer or unplug it.",
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
            // Connect on the right, where going ahead usually is.
            .zip([(Action::NotNow, "Not now"), (Action::Connect, "Connect")])
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

    pub(crate) fn draw(&self, canvas: &mut Canvas, ui: &mut Typesetter, status: &str) {
        canvas.clear();
        ui::header(canvas, ui, "USB", status);
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

/// Where sharing the storage with a computer has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbScreen {
    /// Connect was tapped: the reader stops and the storage is handed over.
    Connecting,
    /// The computer has the storage.
    Connected,
    /// It's back from the computer and being checked before the reader
    /// starts again.
    Checking,
    /// The storage couldn't be handed over.
    Failed,
}

impl UsbScreen {
    fn said(self) -> &'static [&'static str] {
        match self {
            UsbScreen::Connecting => &["Connecting to the computer…"],
            UsbScreen::Connected => &[
                "Connected to the computer. The Kobo's files are on the drive called \
                 KOBOeReader.",
                "Eject it on the computer, or unplug the Kobo, to come back to Simple Books.",
            ],
            UsbScreen::Checking => &["Checking the Kobo's storage, then back to Simple Books…"],
            UsbScreen::Failed => &[
                "Couldn't connect: something on the Kobo is still using its storage.",
                "Back to Simple Books in a moment.",
            ],
        }
    }
}

/// `screen` drawn whole, for a platform with no session running: while
/// the computer has the storage, the reader's files aren't there.
pub fn usb_screen(screen: UsbScreen) -> Canvas {
    let settings = Settings::default();
    let mut canvas = Canvas::new(settings.width, settings.height);
    let mut ui = Typesetter::new(settings, Some("en"));
    draw_said(&mut canvas, &mut ui, screen, "");
    canvas
}

/// `screen` on `canvas`, with `status` in the bar along the top.
pub(crate) fn draw_said(canvas: &mut Canvas, ui: &mut Typesetter, screen: UsbScreen, status: &str) {
    canvas.clear();
    ui::header(canvas, ui, "USB", status);
    let room = canvas.width() as f32 - 2.0 * MARGIN;
    let mut y = TEXT_TOP;
    for text in screen.said() {
        for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 6) {
            ui.draw_label(canvas, &line, (MARGIN, y), LABEL_SIZE, 400.0);
            y += LINE;
        }
        y += LINE / 2.0;
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
