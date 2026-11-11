//! The Kobo screen, from settings: whether Simple Books starts when the
//! Kobo is switched on, uninstalling it, and updating it to the version
//! a sync downloaded.

use reader_core::{Canvas, Typesetter};

use crate::ui::{self, BUTTON_HEIGHT, GAP, HEADER, LABEL_SIZE, MARGIN, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Start Simple Books when switched on, or Nickel.
    StartHere,
    Uninstall,
    /// Start the version downloaded.
    Update,
    Back,
}

/// What the screen shows besides its buttons.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct State {
    /// Whether it starts when switched on; `None` where that can't change.
    pub starts_here: Option<bool>,
    /// What the last change said, or why it failed.
    pub said: Option<String>,
    /// Uninstall was tapped once; a second tap does it.
    pub confirming: bool,
    /// The version running, where it updates itself.
    pub version: Option<String>,
    /// The version downloaded, ready to update to.
    pub update: Option<String>,
    /// Why updating didn't work.
    pub update_said: Option<String>,
    /// About the device: room for books, the battery's health.
    pub about: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    buttons: Vec<(Rect, Action)>,
    pub(crate) state: State,
}

const TEXT_TOP: f32 = HEADER + 96.0;
const LINE: f32 = 60.0;
const START_TOP: f32 = HEADER + 330.0;
const UNINSTALL_TOP: f32 = START_TOP + BUTTON_HEIGHT + 210.0;

const START_SAID: &str = "When this is on, the Kobo opens Simple Books when it's switched on, \
     instead of Nickel. To get Nickel once, keep a finger on the screen while it starts.";
const UNINSTALL_SAID: &str = "Uninstalling puts the Kobo back as it was: it starts Nickel, \
     and Simple Books is gone from it. Your books stay in .adds/simple-books, to delete over USB.";

impl Layout {
    pub(crate) fn new(width: u32, height: u32, state: State) -> Self {
        let (width, height) = (width as f32, height as f32);
        let room = width - 2.0 * MARGIN;
        let mut buttons = Vec::new();
        if state.starts_here.is_some() {
            let rect = Rect {
                left: MARGIN,
                top: START_TOP,
                width: room,
                height: BUTTON_HEIGHT,
            };
            buttons.push((rect, Action::StartHere));
        }
        let uninstall = Rect {
            left: MARGIN,
            top: UNINSTALL_TOP,
            width: room,
            height: BUTTON_HEIGHT,
        };
        buttons.push((uninstall, Action::Uninstall));
        // The way out at the right, Update before it.
        let bottom = height - BUTTON_HEIGHT - 24.0;
        if state.update.is_some() {
            let row = ui::row(MARGIN, bottom, room, BUTTON_HEIGHT, &[1.0, 1.0]);
            buttons.push((row[0], Action::Update));
            buttons.push((row[1], Action::Back));
        } else {
            let row = ui::row(MARGIN, bottom, room, BUTTON_HEIGHT, &[1.0]);
            buttons.push((row[0], Action::Back));
        }
        Self { buttons, state }
    }

    /// What to say about the version, above the bottom row.
    fn version_said(&self) -> Option<String> {
        let state = &self.state;
        if let Some(said) = &state.update_said {
            return Some(said.clone());
        }
        let version = state.version.as_deref().unwrap_or("an unknown version");
        match &state.update {
            Some(update) => Some(format!(
                "This is Simple Books {version}. {update} is downloaded: Update starts it."
            )),
            None if state.version.is_some() => Some(format!(
                "This is Simple Books {version}. New versions come from your server as it syncs."
            )),
            None => None,
        }
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|(_, action)| *action)
    }

    pub(crate) fn draw(&self, canvas: &mut Canvas, ui: &mut Typesetter, status: &str) {
        canvas.clear();
        ui::header(canvas, ui, "Kobo", status);
        let room = canvas.width() as f32 - 2.0 * MARGIN;
        let paragraph = |canvas: &mut Canvas, ui: &mut Typesetter, text: &str, mut y: f32| {
            for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 4) {
                ui.draw_label(canvas, &line, (MARGIN, y), LABEL_SIZE, 400.0);
                y += LINE;
            }
            y
        };
        if self.state.starts_here.is_some() {
            paragraph(canvas, ui, START_SAID, TEXT_TOP);
        }
        if let Some(said) = &self.state.said {
            paragraph(canvas, ui, said, START_TOP + BUTTON_HEIGHT + GAP + LINE);
        }
        // Where the next line would go: what's below must start lower.
        let uninstall_end = paragraph(
            canvas,
            ui,
            UNINSTALL_SAID,
            UNINSTALL_TOP + BUTTON_HEIGHT + GAP + LINE,
        );
        let bottom = canvas.height() as f32 - BUTTON_HEIGHT - 24.0;
        let version = self
            .version_said()
            .map(|said| ui::wrap(ui, &said, LABEL_SIZE, 400.0, room, 3))
            .unwrap_or_default();
        let version_top = bottom - GAP - LINE * (version.len() as f32 - 1.0) - 12.0;
        // One line, just above the version, if there's room.
        if let Some(about) = &self.state.about {
            let y = version_top - LINE - GAP;
            if let [line] = ui::wrap(ui, about, LABEL_SIZE, 400.0, room, 2).as_slice()
                && y >= uninstall_end
            {
                ui.draw_label(canvas, line, (MARGIN, y), LABEL_SIZE, 400.0);
            }
        }
        let mut y = version_top;
        for line in version {
            ui.draw_label(canvas, &line, (MARGIN, y), LABEL_SIZE, 400.0);
            y += LINE;
        }
        for (rect, action) in &self.buttons {
            match action {
                Action::StartHere => ui::toggle(
                    canvas,
                    ui,
                    *rect,
                    "Start Simple Books when switched on",
                    self.state.starts_here == Some(true),
                ),
                Action::Uninstall => ui::button(
                    canvas,
                    ui,
                    *rect,
                    if self.state.confirming {
                        "Tap again to uninstall"
                    } else {
                        "Uninstall Simple Books"
                    },
                ),
                Action::Update => ui::button(canvas, ui, *rect, "Update"),
                Action::Back => ui::button(canvas, ui, *rect, "Back"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_is_there_once_one_is_downloaded_with_back_at_the_right() {
        let actions = |update: Option<&str>| {
            let state = State {
                version: Some("v1".into()),
                update: update.map(str::to_owned),
                ..State::default()
            };
            let layout = Layout::new(1072, 1448, state);
            let mut bottom: Vec<(f32, Action)> = layout
                .buttons
                .iter()
                .filter(|(rect, _)| rect.bottom() > 1400.0)
                .map(|(rect, action)| (rect.left, *action))
                .collect();
            bottom.sort_by(|a, b| a.0.total_cmp(&b.0));
            bottom
                .into_iter()
                .map(|(_, action)| action)
                .collect::<Vec<_>>()
        };
        assert_eq!(actions(None), [Action::Back]);
        assert_eq!(actions(Some("v2")), [Action::Update, Action::Back]);
    }

    #[test]
    fn the_start_toggle_is_only_there_when_it_can_change() {
        for (starts_here, wanted) in [(None, false), (Some(false), true), (Some(true), true)] {
            let state = State {
                starts_here,
                ..State::default()
            };
            let layout = Layout::new(1072, 1448, state);
            let has = layout.buttons.iter().any(|(_, a)| *a == Action::StartHere);
            assert_eq!(has, wanted);
            for (rect, action) in &layout.buttons {
                let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
                assert_eq!(layout.hit(middle.0, middle.1), Some(*action));
                assert!(rect.bottom() <= 1448.0);
            }
        }
    }
}
