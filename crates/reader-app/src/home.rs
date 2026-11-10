//! The home screen: the book being read, large, with how far through it is,
//! then rows of covers for the books read lately and the books added
//! lately. A tap on any of them opens it where it was left.

use reader_core::{Canvas, Picture, Typesetter};

use crate::{
    library::{Entry, Library},
    ui::{self, BUTTON_HEIGHT, HEADER, LABEL_SIZE, MARGIN, Rect},
};

/// The book being read: its cover's size.
const CURRENT_COVER: (f32, f32) = (240.0, 360.0);
/// Covers in a row, and the gap between them.
const PER_ROW: usize = 5;
const ROW_GAP: f32 = 30.0;
/// A row: its label, its covers, and a line for each title.
const ROW_LABEL: f32 = 56.0;
/// Room for two lines of title under each cover.
const ROW_TITLE: f32 = 70.0;
const TITLE_LINES: usize = 2;
/// Between one row's titles and the next row's label.
const ROW_SPACE: f32 = 24.0;

const TITLE_SIZE: f32 = 48.0;
const SMALL_SIZE: f32 = 32.0;
const TILE_TITLE_SIZE: f32 = 26.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Opens the book in this file.
    Open(String),
    Library,
    Settings,
    Exit,
}

#[derive(Debug, Clone)]
struct Row {
    label: &'static str,
    /// Each cover's place and its book's file.
    tiles: Vec<(Rect, String)>,
    /// Where the row's titles go, under the covers.
    titles_at: f32,
}

/// Where everything on the home screen is, for one state of the library.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    width: f32,
    /// No books in the folder at all.
    empty: bool,
    /// The book being read, and the area a tap opens it from.
    current: Option<(String, Rect)>,
    rows: Vec<Row>,
    buttons: Vec<(Rect, Action, &'static str)>,
}

impl Layout {
    pub(crate) fn new(width: u32, height: u32, library: &Library) -> Self {
        let (width, height) = (width as f32, height as f32);
        let content = width - 2.0 * MARGIN;
        let current_top = HEADER + 24.0;
        let current = library.current().map(|entry| {
            (
                entry.file.clone(),
                Rect {
                    left: MARGIN,
                    top: current_top,
                    width: content,
                    height: CURRENT_COVER.1,
                },
            )
        });
        let current_file = current.as_ref().map(|(file, _)| file.as_str());

        let read: Vec<&Entry> = library
            .recently_opened()
            .into_iter()
            .filter(|e| Some(e.file.as_str()) != current_file)
            .take(PER_ROW)
            .collect();
        let added: Vec<&Entry> = library
            .recently_added()
            .into_iter()
            .filter(|e| Some(e.file.as_str()) != current_file)
            .take(PER_ROW)
            .collect();
        let tile_width = ((content - ROW_GAP * (PER_ROW as f32 - 1.0)) / PER_ROW as f32).floor();
        let tile_height = (tile_width * 1.5).floor();
        // With no book being read yet, the rows start under the header.
        let mut top = if current.is_some() {
            current_top + CURRENT_COVER.1 + 32.0
        } else {
            current_top
        };
        let buttons_top = height - BUTTON_HEIGHT - 24.0;
        let mut rows = Vec::new();
        for (label, entries) in [("Recently read", read), ("Recently added", added)] {
            let covers_top = top + ROW_LABEL;
            // A row that would run into the buttons is left out.
            if entries.is_empty() || covers_top + tile_height + ROW_TITLE > buttons_top {
                continue;
            }
            let tiles = entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let rect = Rect {
                        left: MARGIN + i as f32 * (tile_width + ROW_GAP),
                        top: covers_top,
                        width: tile_width,
                        // The title under the cover opens it too.
                        height: tile_height + ROW_TITLE,
                    };
                    (rect, entry.file.clone())
                })
                .collect();
            rows.push(Row {
                label,
                tiles,
                titles_at: covers_top + tile_height,
            });
            top = covers_top + tile_height + ROW_TITLE + ROW_SPACE;
        }

        let bottom = ui::row(
            MARGIN,
            buttons_top,
            content,
            BUTTON_HEIGHT,
            &[1.0, 1.0, 1.5],
        );
        let buttons = vec![
            (bottom[0], Action::Library, "Library"),
            (bottom[1], Action::Settings, "Settings"),
            (bottom[2], Action::Exit, "Exit to Nickel"),
        ];
        Self {
            width,
            empty: library.books().is_empty(),
            current,
            rows,
            buttons,
        }
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        if let Some((file, rect)) = &self.current
            && rect.contains(x, y)
        {
            return Some(Action::Open(file.clone()));
        }
        for row in &self.rows {
            for (rect, file) in &row.tiles {
                if rect.contains(x, y) {
                    return Some(Action::Open(file.clone()));
                }
            }
        }
        self.buttons
            .iter()
            .find(|(rect, ..)| rect.contains(x, y))
            .map(|(_, action, _)| action.clone())
    }

    /// Draws the screen. `cover` gives a book's cover picture, if it has
    /// one; `battery` is shown in the corner.
    pub(crate) fn draw(
        &self,
        canvas: &mut Canvas,
        ui: &mut Typesetter,
        library: &Library,
        cover: &mut dyn FnMut(&Entry) -> Option<Picture>,
        battery: &str,
    ) {
        canvas.clear();
        ui::header(canvas, ui, "Simple Books", battery);

        if let Some((file, rect)) = &self.current
            && let Some(entry) = library.get(file)
        {
            draw_current(canvas, ui, entry, *rect, cover(entry));
        }
        if self.empty {
            let room = self.width - 2.0 * MARGIN;
            let mut y = HEADER + 120.0;
            ui.draw_label(canvas, "No books yet.", (MARGIN, y), LABEL_SIZE, 600.0);
            let hint = "Books show here once they're in the books folder on the Kobo.";
            for line in ui::wrap(ui, hint, LABEL_SIZE, 400.0, room, 3) {
                y += LABEL_SIZE * 1.5;
                ui.draw_label(canvas, &line, (MARGIN, y), LABEL_SIZE, 400.0);
            }
        }

        for row in &self.rows {
            let label_y = row.tiles[0].0.top - ROW_LABEL * 0.35;
            ui.draw_label(canvas, row.label, (MARGIN, label_y), 36.0, 600.0);
            for (rect, file) in &row.tiles {
                let Some(entry) = library.get(file) else {
                    continue;
                };
                let cover_box = Rect {
                    height: row.titles_at - rect.top,
                    ..*rect
                };
                draw_cover(canvas, ui, entry, cover_box, cover(entry));
                let mut y = row.titles_at + TILE_TITLE_SIZE * 1.4;
                for line in ui::wrap(
                    ui,
                    &entry.title,
                    TILE_TITLE_SIZE,
                    500.0,
                    rect.width,
                    TITLE_LINES,
                ) {
                    ui.draw_label(canvas, &line, (rect.left, y), TILE_TITLE_SIZE, 500.0);
                    y += TILE_TITLE_SIZE * 1.2;
                }
            }
        }

        for (rect, _, label) in &self.buttons {
            ui::button(canvas, ui, *rect, label);
        }
    }
}

/// The book being read: cover on the left, its details on the right, and
/// how far through it is along the bottom.
fn draw_current(
    canvas: &mut Canvas,
    ui: &mut Typesetter,
    entry: &Entry,
    rect: Rect,
    picture: Option<Picture>,
) {
    let cover_box = Rect {
        width: CURRENT_COVER.0,
        height: CURRENT_COVER.1,
        ..rect
    };
    draw_cover(canvas, ui, entry, cover_box, picture);
    let left = rect.left + CURRENT_COVER.0 + 40.0;
    let room = rect.right() - left;
    let mut y = rect.top + TITLE_SIZE;
    for line in ui::wrap(ui, &entry.title, TITLE_SIZE, 600.0, room, 3) {
        ui.draw_label(canvas, &line, (left, y), TITLE_SIZE, 600.0);
        y += TITLE_SIZE * 1.25;
    }
    y += 16.0;
    if !entry.authors.is_empty() {
        let authors = ui::fit(ui, &entry.authors.join(", "), LABEL_SIZE, 400.0, room);
        ui.draw_label(canvas, &authors, (left, y), LABEL_SIZE, 400.0);
        y += LABEL_SIZE * 1.4;
    }
    if let Some(text) = entry.series_text() {
        let text = ui::fit(ui, &text, SMALL_SIZE, 400.0, room);
        ui.draw_label(canvas, &text, (left, y), SMALL_SIZE, 400.0);
    }

    // Progress, along the bottom of the details.
    let progress = entry.progress.unwrap_or(0.0);
    let bar = Rect {
        left,
        top: rect.bottom() - 20.0,
        width: room,
        height: 20.0,
    };
    let text = format!("{}% read", (progress * 100.0).round());
    ui.draw_label(canvas, &text, (left, bar.top - 20.0), SMALL_SIZE, 500.0);
    ui::frame(canvas, bar);
    let (bar_left, bar_top, bar_width, bar_height) = bar.pixels();
    let filled = (bar_width as f32 * progress).round() as u32;
    canvas.fill(bar_left, bar_top, filled, bar_height, 0);
}

/// A book's cover in `rect`, centred, or a plain card with its title and
/// author if it has no cover.
fn draw_cover(
    canvas: &mut Canvas,
    ui: &mut Typesetter,
    entry: &Entry,
    rect: Rect,
    picture: Option<Picture>,
) {
    if let Some(picture) = picture {
        let (width, height) = picture.fitted_size(rect.width as u32, rect.height as u32);
        let left = rect.left + (rect.width - width as f32) / 2.0;
        let top = rect.top + (rect.height - height as f32) / 2.0;
        // A thin outline, so a white cover still has an edge.
        canvas.fill(left as i32 - 2, top as i32 - 2, width + 4, height + 4, 0);
        picture.draw(canvas, left as i32, top as i32, width, height);
        return;
    }
    ui::frame(canvas, rect);
    let size = (rect.width / 7.0).clamp(20.0, 40.0);
    let room = rect.width - 2.0 * size * 0.6;
    let left = rect.left + size * 0.6;
    let mut y = rect.top + size * 2.0;
    for line in ui::wrap(ui, &entry.title, size, 600.0, room, 4) {
        ui.draw_label(canvas, &line, (left, y), size, 600.0);
        y += size * 1.25;
    }
    if let Some(author) = entry.authors.first() {
        let author = ui::fit(ui, author, size * 0.8, 400.0, room);
        y += size * 0.5;
        ui.draw_label(canvas, &author, (left, y), size * 0.8, 400.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: u32 = 1072;
    const HEIGHT: u32 = 1448;

    /// `count` books, the first `opened` of them opened, the first most
    /// recently.
    fn library(count: u64, opened: u64) -> Library {
        let books: Vec<Entry> = (0..count)
            .map(|i| Entry {
                file: format!("{i}.book"),
                size: 1,
                modified: 100 + i,
                key: i,
                title: format!("Book {i}"),
                authors: Vec::new(),
                series: None,
                cover: false,
                opened: (i < opened).then(|| 1000 - i),
                progress: None,
                id: None,
                revision: None,
                added: None,
            })
            .collect();
        let json = serde_json::json!({ "books": books });
        serde_json::from_value(json).unwrap()
    }

    fn areas(layout: &Layout) -> Vec<(Rect, Action)> {
        let mut areas: Vec<(Rect, Action)> = layout
            .current
            .iter()
            .map(|(file, rect)| (*rect, Action::Open(file.clone())))
            .collect();
        for row in &layout.rows {
            for (rect, file) in &row.tiles {
                areas.push((*rect, Action::Open(file.clone())));
            }
        }
        for (rect, action, _) in &layout.buttons {
            areas.push((*rect, action.clone()));
        }
        areas
    }

    #[test]
    fn nothing_overlaps_and_each_tap_opens_what_is_under_it() {
        let layout = Layout::new(WIDTH, HEIGHT, &library(12, 7));
        assert_eq!(
            layout.current.as_ref().map(|(file, _)| file.as_str()),
            Some("0.book")
        );
        let rows: Vec<Vec<&str>> = layout
            .rows
            .iter()
            .map(|row| row.tiles.iter().map(|(_, file)| file.as_str()).collect())
            .collect();
        // The book being read isn't repeated in the rows.
        assert_eq!(
            rows,
            [
                vec!["1.book", "2.book", "3.book", "4.book", "5.book"],
                vec!["11.book", "10.book", "9.book", "8.book", "7.book"],
            ]
        );
        let areas = areas(&layout);
        for (i, (rect, action)) in areas.iter().enumerate() {
            assert!(
                rect.left >= 0.0 && rect.right() <= WIDTH as f32,
                "{action:?}"
            );
            assert!(
                rect.top > HEADER && rect.bottom() <= HEIGHT as f32,
                "{action:?}"
            );
            for (other, other_action) in &areas[i + 1..] {
                assert!(!rect.overlaps(other), "{action:?} and {other_action:?}");
            }
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(layout.hit(middle.0, middle.1).as_ref(), Some(action));
        }
        assert_eq!(layout.hit(10.0, 10.0), None);
    }

    #[test]
    fn with_nothing_read_yet_the_rows_start_under_the_header() {
        let layout = Layout::new(WIDTH, HEIGHT, &library(3, 0));
        assert!(layout.current.is_none());
        assert!(!layout.empty);
        assert_eq!(layout.rows.len(), 1);
        assert!(layout.rows[0].tiles[0].0.top < HEADER + 200.0);

        let empty = Layout::new(WIDTH, HEIGHT, &library(0, 0));
        assert!(empty.empty && empty.rows.is_empty() && empty.current.is_none());
    }
}
