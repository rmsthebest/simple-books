//! Said once a new version is up, after Update: so the home screen
//! coming back doesn't leave anyone wondering whether it worked. Any tap,
//! OK or not, goes on to where it would have started, so there's nothing
//! to hit.

use reader_core::{Canvas, Typesetter};

use crate::ui::{self, BUTTON_HEIGHT, HEADER, LABEL_SIZE, MARGIN, Rect};

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    ok: Rect,
    version: String,
}

const TEXT_TOP: f32 = HEADER + 96.0;
const LINE: f32 = 60.0;

impl Layout {
    pub(crate) fn new(width: u32, height: u32, version: &str) -> Self {
        let (width, height) = (width as f32, height as f32);
        let ok = ui::row(
            MARGIN,
            height - BUTTON_HEIGHT - 24.0,
            width - 2.0 * MARGIN,
            BUTTON_HEIGHT,
            &[1.0],
        )[0];
        Self {
            ok,
            version: version.to_owned(),
        }
    }

    pub(crate) fn draw(&self, canvas: &mut Canvas, ui: &mut Typesetter, status: &str) {
        canvas.clear();
        ui::header(canvas, ui, "Updated", status);
        let room = canvas.width() as f32 - 2.0 * MARGIN;
        let said = [
            format!("Simple Books is updated to {}.", self.version),
            "Your books, places and settings are as they were.".to_owned(),
        ];
        let mut y = TEXT_TOP;
        for text in &said {
            for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 4) {
                ui.draw_label(canvas, &line, (MARGIN, y), LABEL_SIZE, 400.0);
                y += LINE;
            }
            y += LINE / 2.0;
        }
        ui::button(canvas, ui, self.ok, "OK");
    }
}
