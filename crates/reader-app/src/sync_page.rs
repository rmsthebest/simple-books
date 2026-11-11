//! The sync screen, from settings: pairing with the website, syncing now,
//! how the last sync went, and SSH where the platform has it.

use reader_core::{Canvas, Typesetter};

use crate::{
    sync::{Activity, SshState},
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
    /// Turns SSH on or off.
    Ssh,
    Back,
}

/// Where everything is, for one view.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub view: View,
    /// Unpair was tapped once: another tap unpairs.
    pub confirming: bool,
    /// Where SSH is, if the platform has it.
    pub ssh: Option<SshState>,
    buttons: Vec<(Rect, Action, &'static str)>,
}

const TEXT_TOP: f32 = HEADER + 96.0;
const LINE: f32 = 60.0;
const CODE_SIZE: f32 = 120.0;

impl Layout {
    pub(crate) fn new(
        width: u32,
        height: u32,
        view: View,
        confirming: bool,
        ssh: Option<SshState>,
    ) -> Self {
        let (width, height) = (width as f32, height as f32);
        let content = width - 2.0 * MARGIN;
        // Back on the left, going ahead on the right (see ui).
        let mut row: Vec<(Action, &'static str, f32)> = vec![(Action::Back, "Back", 1.0)];
        match &view {
            View::NoServer => {}
            View::Unpaired { .. } => row.push((Action::Pair, "Pair", 1.0)),
            View::Paired { .. } => {
                row.push((
                    Action::Unpair,
                    if confirming {
                        "Tap again to unpair"
                    } else {
                        "Unpair"
                    },
                    1.0,
                ));
                row.push((Action::SyncNow, "Sync now", 1.0));
            }
            View::Busy(_) => row.push((Action::Stop, "Stop", 1.0)),
        }
        let shares: Vec<f32> = row.iter().map(|(_, _, share)| *share).collect();
        let top = height - BUTTON_HEIGHT - 24.0;
        let rects = ui::row(MARGIN, top, content, BUTTON_HEIGHT, &shares);
        let mut buttons: Vec<_> = rects
            .into_iter()
            .zip(row)
            .map(|(rect, (action, label, _))| (rect, action, label))
            .collect();
        // Off while another job runs, which SSH would have to wait for.
        let ssh_button = match &ssh {
            Some(SshState::On(_)) => true,
            Some(SshState::Off { .. }) => !matches!(view, View::Busy(_)),
            None => false,
        };
        if ssh_button {
            let top = top - BUTTON_HEIGHT - 24.0;
            let rect = ui::row(MARGIN, top, content, BUTTON_HEIGHT, &[1.0])[0];
            buttons.push((rect, Action::Ssh, "SSH"));
        }
        Self {
            view,
            confirming,
            ssh,
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
        status: &str,
    ) {
        canvas.clear();
        ui::header(canvas, ui, "Sync", status);
        let mut y = TEXT_TOP;
        self.draw_view(canvas, ui, &mut y);
        if let Some(last) = last
            && !matches!(
                self.view,
                View::NoServer | View::Busy(Activity::Pairing { .. })
            )
        {
            say(canvas, ui, last, &mut y);
        }
        match &self.ssh {
            Some(SshState::On(login)) => {
                let text = match &login.password {
                    Some(password) => format!(
                        "SSH is on: {}, password {password}. Anyone who can see this screen \
                         can log in, so turn it off when you're done. Sleeping turns it off \
                         too.",
                        login.command
                    ),
                    None => format!(
                        "SSH is on: {}. Anyone on this Wi-Fi can log in, so turn it off when \
                         you're done. Sleeping turns it off too.",
                        login.command
                    ),
                };
                say(canvas, ui, &text, &mut y);
            }
            Some(SshState::Off {
                problem: Some(problem),
            }) => say(canvas, ui, &format!("SSH didn't start: {problem}"), &mut y),
            _ => {}
        }
        for (rect, action, label) in &self.buttons {
            let on = match action {
                Action::Unpair => self.confirming,
                Action::Ssh => matches!(self.ssh, Some(SshState::On(_))),
                _ => false,
            };
            ui::toggle(canvas, ui, *rect, label, on);
        }
    }
}

impl Layout {
    /// What the view says, from `y` down.
    fn draw_view(&self, canvas: &mut Canvas, ui: &mut Typesetter, y: &mut f32) {
        match &self.view {
            View::NoServer => say(
                canvas,
                ui,
                "This reader doesn't know which server to sync with. Install it from your \
                 server's device page, or put the server's address in a file called server in \
                 the reader's folder.",
                y,
            ),
            View::Unpaired { server } => {
                say(canvas, ui, &format!("Server: {}", host(server)), y);
                say(
                    canvas,
                    ui,
                    "Pair this reader with your library to get your books and keep your place \
                     in them. Pair shows a code to type on the website.",
                    y,
                );
            }
            View::Paired { server, user } => {
                say(canvas, ui, &format!("Server: {}", host(server)), y);
                let whose = match user {
                    Some(user) => format!("Paired with {user}'s library."),
                    None => "Paired.".to_owned(),
                };
                say(canvas, ui, &whose, y);
                if self.confirming {
                    say(
                        canvas,
                        ui,
                        "Unpairing stops syncing. The books stay on this reader.",
                        y,
                    );
                }
            }
            View::Busy(Activity::Connecting) => say(canvas, ui, "Connecting…", y),
            View::Busy(Activity::Pairing { code: None }) => {
                say(canvas, ui, "Asking for a code…", y);
            }
            View::Busy(Activity::Pairing {
                code: Some((code, url)),
            }) => {
                say(
                    canvas,
                    ui,
                    &format!("On {}, sign in and type this code:", page(url)),
                    y,
                );
                let code_width = ui.label_width(code, CODE_SIZE, 700.0);
                let x = (canvas.width() as f32 - code_width) / 2.0;
                *y += CODE_SIZE * 0.8;
                ui.draw_label(canvas, code, (x, *y), CODE_SIZE, 700.0);
                *y += CODE_SIZE * 0.8;
                say(canvas, ui, "Waiting for it…", y);
            }
            View::Busy(Activity::Syncing(what)) => say(canvas, ui, what, y),
            View::Busy(Activity::StartingSsh) => {
                say(canvas, ui, "Turning on Wi-Fi and SSH…", y);
            }
        }
    }
}

/// Draws `text` at `y`, wrapped, and moves `y` below it.
fn say(canvas: &mut Canvas, ui: &mut Typesetter, text: &str, y: &mut f32) {
    let room = canvas.width() as f32 - 2.0 * MARGIN;
    for line in ui::wrap(ui, text, LABEL_SIZE, 400.0, room, 4) {
        ui.draw_label(canvas, &line, (MARGIN, *y), LABEL_SIZE, 400.0);
        *y += LINE;
    }
    *y += LINE / 2.0;
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
        let ssh = [
            None,
            Some(SshState::Off { problem: None }),
            Some(SshState::On(crate::SshLogin {
                command: "ssh root@k".into(),
                password: Some("abcd-efgh".into()),
            })),
        ];
        for (view, ssh) in views
            .iter()
            .flat_map(|view| ssh.iter().map(move |ssh| (view.clone(), ssh.clone())))
        {
            let layout = Layout::new(1072, 1448, view.clone(), false, ssh.clone());
            let has_ssh = layout.buttons.iter().any(|(_, a, _)| *a == Action::Ssh);
            let wanted = match (&ssh, &view) {
                (Some(SshState::On(_)), _) => true,
                (Some(_), View::Busy(_)) | (None, _) => false,
                (Some(_), _) => true,
            };
            assert_eq!(has_ssh, wanted, "{view:?} {ssh:?}");
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
