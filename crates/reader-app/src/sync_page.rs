//! The sync screen, from settings: pairing with the website, syncing now,
//! and how the last sync went.

use reader_core::{Canvas, Typesetter};

use crate::{
    sync::Activity,
    ui::{self, BUTTON_HEIGHT, HEADER, LABEL_SIZE, MARGIN, Rect},
};

/// What the screen is about, from the session's sync state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum View {
    /// No server address is known.
    NoServer,
    Unpaired {
        server: String,
    },
    Paired {
        server: String,
        user: Option<String>,
    },
    Busy(Activity),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Pair,
    SyncNow,
    Stop,
    /// The first tap asks to tap again.
    Unpair,
    Back,
}

/// Where everything is, for one view.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub view: View,
    /// Unpair was tapped once: another tap unpairs.
    pub confirming: bool,
    buttons: Vec<(Rect, Action, &'static str)>,
}

const TEXT_TOP: f32 = HEADER + 96.0;
const LINE: f32 = 60.0;
const CODE_SIZE: f32 = 120.0;

impl Layout {
    pub(crate) fn new(width: u32, height: u32, view: View, confirming: bool) -> Self {
        let (width, height) = (width as f32, height as f32);
        let content = width - 2.0 * MARGIN;
        let mut row: Vec<(Action, &'static str, f32)> = match &view {
            View::NoServer => Vec::new(),
            View::Unpaired { .. } => vec![(Action::Pair, "Pair", 1.0)],
            View::Paired { .. } => vec![
                (Action::SyncNow, "Sync now", 1.0),
                (
                    Action::Unpair,
                    if confirming {
                        "Tap again to unpair"
                    } else {
                        "Unpair"
                    },
                    1.0,
                ),
            ],
            View::Busy(_) => vec![(Action::Stop, "Stop", 1.0)],
        };
        row.push((Action::Back, "Back", 1.0));
        let shares: Vec<f32> = row.iter().map(|(_, _, share)| *share).collect();
        let top = height - BUTTON_HEIGHT - 24.0;
        let rects = ui::row(MARGIN, top, content, BUTTON_HEIGHT, &shares);
        let buttons = rects
            .into_iter()
            .zip(row)
            .map(|(rect, (action, label, _))| (rect, action, label))
            .collect();
        Self {
            view,
            confirming,
            buttons,
        }
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons
            .iter()
            .find(|(rect, ..)| rect.contains(x, y))
            .map(|(_, action, _)| *action)
    }

    /// Draws the screen. `last` says how the last sync went and how long
    /// ago, already in words.
    pub(crate) fn draw(
        &self,
        canvas: &mut Canvas,
        ui: &mut Typesetter,
        last: Option<&str>,
        battery: &str,
    ) {
        canvas.clear();
        ui::header(canvas, ui, "Sync", battery);
        let room = canvas.width() as f32 - 2.0 * MARGIN;
        let mut y = TEXT_TOP;
        let say = |canvas: &mut Canvas, ui: &mut Typesetter, text: &str, y: &mut f32| {
            for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 4) {
                ui.draw_label(canvas, &line, (MARGIN, *y), LABEL_SIZE, 400.0);
                *y += LINE;
            }
            *y += LINE / 2.0;
        };
        match &self.view {
            View::NoServer => say(
                canvas,
                ui,
                "This reader doesn't know which server to sync with. Connect it to your server \
                 from the website's Kobo page first, or put the server's address in server.txt \
                 in the reader's folder.",
                &mut y,
            ),
            View::Unpaired { server } => {
                say(canvas, ui, &format!("Server: {}", host(server)), &mut y);
                say(
                    canvas,
                    ui,
                    "Pair this reader with your library to get your books and keep your place \
                     in them. Pair shows a code to type on the website.",
                    &mut y,
                );
            }
            View::Paired { server, user } => {
                say(canvas, ui, &format!("Server: {}", host(server)), &mut y);
                let whose = match user {
                    Some(user) => format!("Paired with {user}'s library."),
                    None => "Paired.".to_owned(),
                };
                say(canvas, ui, &whose, &mut y);
                if self.confirming {
                    say(
                        canvas,
                        ui,
                        "Unpairing stops syncing. The books stay on this reader.",
                        &mut y,
                    );
                }
            }
            View::Busy(Activity::Connecting) => say(canvas, ui, "Connecting…", &mut y),
            View::Busy(Activity::Pairing { code: None }) => {
                say(canvas, ui, "Asking for a code…", &mut y);
            }
            View::Busy(Activity::Pairing {
                code: Some((code, url)),
            }) => {
                say(
                    canvas,
                    ui,
                    &format!("On {}, sign in and type this code:", page(url)),
                    &mut y,
                );
                let code_width = ui.label_width(code, CODE_SIZE, 700.0);
                let x = (canvas.width() as f32 - code_width) / 2.0;
                y += CODE_SIZE * 0.8;
                ui.draw_label(canvas, code, (x, y), CODE_SIZE, 700.0);
                y += CODE_SIZE * 0.8;
                say(canvas, ui, "Waiting for it…", &mut y);
            }
            View::Busy(Activity::Syncing(what)) => say(canvas, ui, what, &mut y),
        }
        if let Some(last) = last
            && !matches!(
                self.view,
                View::NoServer | View::Busy(Activity::Pairing { .. })
            )
        {
            say(canvas, ui, last, &mut y);
        }
        for (rect, action, label) in &self.buttons {
            let on = *action == Action::Unpair && self.confirming;
            ui::toggle(canvas, ui, *rect, label, on);
        }
    }
}

/// `https://books.example.com/` as `books.example.com`.
fn host(server: &str) -> &str {
    server
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
}

/// The page to type the code on, without its scheme.
fn page(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
}

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
pub(crate) fn ago(seconds: u64) -> String {
    let (n, unit) = match seconds {
        0..60 => return "just now".to_owned(),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_has_a_way_back() {
        let views = [
            View::NoServer,
            View::Unpaired {
                server: "https://b".into(),
            },
            View::Paired {
                server: "https://b".into(),
                user: None,
            },
            View::Busy(Activity::Connecting),
        ];
        for view in views {
            let layout = Layout::new(1072, 1448, view.clone(), false);
            let (rect, ..) = layout
                .buttons
                .iter()
                .find(|(_, action, _)| *action == Action::Back)
                .unwrap();
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(
                layout.hit(middle.0, middle.1),
                Some(Action::Back),
                "{view:?}"
            );
            for (i, (a, ..)) in layout.buttons.iter().enumerate() {
                assert!(a.right() <= 1072.0 && a.bottom() <= 1448.0);
                for (b, ..) in &layout.buttons[i + 1..] {
                    assert!(!a.overlaps(b), "{view:?}");
                }
            }
        }
    }

    #[test]
    fn times_read_naturally() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(150), "2 minutes ago");
        assert_eq!(ago(7200), "2 hours ago");
        assert_eq!(ago(86_400 * 3), "3 days ago");
        assert_eq!(host("https://books.example.com/"), "books.example.com");
    }
}
