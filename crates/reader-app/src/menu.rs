//! The menu: a panel over the top of the page, opened by tapping the top of
//! the screen. Each row is about a centimetre tall on the Clara BW, so it's
//! easy to hit.

use reader_core::{Canvas, Typesetter};

use crate::ui::{self, BORDER, LABEL_SIZE, Rect};

/// What a menu button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Light(Step),
    Warmth(Step),
    TextSize(Step),
    Darkness(Step),
    Contrast(Step),
    /// Return from the last link followed.
    Return,
    Home,
    Close,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Down,
    Up,
}

/// What the menu shows, read when it's drawn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Values {
    pub title: String,
    /// "84%", "84%, charging", or empty if unknown.
    pub battery: String,
    pub light: String,
    pub warmth: String,
    pub text_size: String,
    pub darkness: String,
    pub contrast: String,
    pub can_return: bool,
}

#[derive(Debug, Clone, Copy)]
struct Button {
    rect: Rect,
    action: Action,
    label: &'static str,
}

/// Where everything in the menu is, for a screen `width` wide.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    width: f32,
    margin: f32,
    row: f32,
    buttons: Vec<Button>,
    /// The panel's bottom edge.
    bottom: f32,
}

/// Makes a row's action from the step its button takes.
type StepAction = fn(Step) -> Action;

/// Rows with a label, a value and − and + buttons, after the title row.
const STEPPED: [(&str, StepAction); 5] = [
    ("Light", Action::Light),
    ("Warmth", Action::Warmth),
    ("Text size", Action::TextSize),
    ("Darkness", Action::Darkness),
    ("Contrast", Action::Contrast),
];

const TITLE_SIZE: f32 = 44.0;

impl Layout {
    pub(crate) fn new(width: u32, can_return: bool) -> Self {
        let width = width as f32;
        let margin = 64.0;
        let row = 120.0;
        let gap = 16.0;
        let button = row - gap;
        let mut buttons = Vec::new();
        let mut top = row;
        for (_, action) in STEPPED {
            let plus = width - margin - button;
            let minus = plus - 2.5 * button;
            for (left, step, label) in [(minus, Step::Down, "−"), (plus, Step::Up, "+")] {
                buttons.push(Button {
                    rect: Rect {
                        left,
                        top: top + gap / 2.0,
                        width: button,
                        height: button,
                    },
                    action: action(step),
                    label,
                });
            }
            top += row;
        }
        // Each button's share of the row: the long label gets two.
        let mut bottom_row: Vec<(Action, &'static str, f32)> = Vec::new();
        if can_return {
            bottom_row.push((Action::Return, "Return", 1.0));
        }
        bottom_row.push((Action::Home, "Home", 1.0));
        bottom_row.push((Action::Close, "Close", 1.0));
        bottom_row.push((Action::Exit, "Exit to Nickel", 2.0));
        let count = bottom_row.len() as f32;
        let shares: f32 = bottom_row.iter().map(|(_, _, share)| share).sum();
        let unit = (width - 2.0 * margin - gap * (count - 1.0)) / shares;
        let mut left = margin;
        for (action, label, share) in bottom_row {
            buttons.push(Button {
                rect: Rect {
                    left,
                    top: top + gap / 2.0,
                    width: unit * share,
                    height: button,
                },
                action,
                label,
            });
            left += unit * share + gap;
        }
        let bottom = top + row + gap;
        Self {
            width,
            margin,
            row,
            buttons,
            bottom,
        }
    }

    /// The panel's height: taps below it close the menu.
    pub(crate) fn bottom(&self) -> f32 {
        self.bottom
    }

    /// The button at (`x`, `y`). `None` on the panel between buttons.
    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons
            .iter()
            .find(|b| b.rect.contains(x, y))
            .map(|b| b.action)
    }

    /// Draws the panel over whatever `canvas` holds.
    pub(crate) fn draw(&self, canvas: &mut Canvas, ui: &mut Typesetter, values: &Values) {
        let width = self.width as u32;
        canvas.fill(0, 0, width, self.bottom as u32, 255);
        canvas.fill(0, self.bottom as i32, width, BORDER + 1, 0);

        // The title, shortened to leave room for the battery.
        let battery_width = ui.label_width(&values.battery, LABEL_SIZE, 400.0);
        let room = self.width - 2.0 * self.margin - battery_width - 32.0;
        let title = ui::fit(ui, &values.title, TITLE_SIZE, 600.0, room);
        let baseline = self.row * 0.62;
        ui.draw_label(canvas, &title, (self.margin, baseline), TITLE_SIZE, 600.0);
        ui.draw_label(
            canvas,
            &values.battery,
            (self.width - self.margin - battery_width, baseline),
            LABEL_SIZE,
            400.0,
        );

        let shown = [
            &values.light,
            &values.warmth,
            &values.text_size,
            &values.darkness,
            &values.contrast,
        ];
        for (i, ((name, _), value)) in STEPPED.iter().zip(shown).enumerate() {
            let baseline = self.row * (i as f32 + 1.62);
            ui.draw_label(canvas, name, (self.margin, baseline), LABEL_SIZE, 400.0);
            // The value sits centred between its two buttons.
            let minus = self.buttons[2 * i].rect;
            let plus = self.buttons[2 * i + 1].rect;
            let middle = f32::midpoint(minus.left + minus.width, plus.left);
            let value_width = ui.label_width(value, LABEL_SIZE, 400.0);
            ui.draw_label(
                canvas,
                value,
                (middle - value_width / 2.0, baseline),
                LABEL_SIZE,
                400.0,
            );
        }
        for button in &self.buttons {
            ui::button(canvas, ui, button.rect, button.label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_is_on_the_panel_and_apart() {
        for can_return in [false, true] {
            let layout = Layout::new(1072, can_return);
            for (i, a) in layout.buttons.iter().enumerate() {
                assert!(a.rect.top + a.rect.height <= layout.bottom());
                assert!(a.rect.left >= 0.0 && a.rect.left + a.rect.width <= 1072.0);
                for b in &layout.buttons[i + 1..] {
                    let apart = a.rect.left + a.rect.width <= b.rect.left
                        || b.rect.left + b.rect.width <= a.rect.left
                        || a.rect.top + a.rect.height <= b.rect.top
                        || b.rect.top + b.rect.height <= a.rect.top;
                    assert!(apart, "{a:?} overlaps {b:?}");
                }
            }
            let has_return = layout.buttons.iter().any(|b| b.action == Action::Return);
            assert_eq!(has_return, can_return);
        }
    }

    #[test]
    fn taps_find_their_buttons() {
        let layout = Layout::new(1072, false);
        for button in &layout.buttons {
            let r = button.rect;
            let (x, y) = (r.left + r.width / 2.0, r.top + r.height / 2.0);
            assert_eq!(layout.hit(x, y), Some(button.action));
        }
        assert_eq!(layout.hit(10.0, 10.0), None);
    }
}
