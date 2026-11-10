//! The library screen: every book, a screen of them at a time, sorted by
//! title, author or series. Page buttons move through them, since scrolling
//! on e-ink is slow and smeary. A tap on a book opens it where it was left.

use std::cmp::Ordering;

use reader_core::{Canvas, Turn, Typesetter};
use serde::{Deserialize, Serialize};

use crate::{
    library::{Entry, Library},
    ui::{self, BUTTON_HEIGHT, HEADER, LABEL_SIZE, MARGIN, Rect},
};

const SORT_TOP: f32 = HEADER + 24.0;
const SORT_HEIGHT: f32 = 88.0;
/// A book in the list: its title, then its authors and series.
const ROW_HEIGHT: f32 = 132.0;
const TITLE_SIZE: f32 = 38.0;
const DETAIL_SIZE: f32 = 30.0;

/// How the library is sorted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    /// By title, without a leading "The", "A" or "An".
    #[default]
    Title,
    /// By the first author's surname, then by title.
    Author,
    /// Books in a series first, by series and their place in it, then the
    /// rest by title.
    Series,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Opens the book in this file.
    Open(String),
    Sort(Sort),
    Page(Turn),
    Home,
}

/// Where everything on one page of the library is.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    sort: Sort,
    page: usize,
    pages: usize,
    /// Each book on this page, and its file.
    rows: Vec<(Rect, String)>,
    buttons: Vec<(Rect, Action, &'static str)>,
    /// Where "Page 2 of 5" goes, between the page buttons.
    page_label: Rect,
}

impl Layout {
    /// Page `page` of the library sorted by `sort`; the last page if there
    /// are fewer.
    pub(crate) fn new(width: u32, height: u32, library: &Library, sort: Sort, page: usize) -> Self {
        let (width, height) = (width as f32, height as f32);
        let content = width - 2.0 * MARGIN;
        let mut buttons = Vec::new();
        let sorts = [
            (Sort::Title, "Title"),
            (Sort::Author, "Author"),
            (Sort::Series, "Series"),
        ];
        let sort_row = ui::row(MARGIN, SORT_TOP, content, SORT_HEIGHT, &[1.0; 3]);
        for (rect, (sort, label)) in sort_row.into_iter().zip(sorts) {
            buttons.push((rect, Action::Sort(sort), label));
        }

        let bottom_top = height - BUTTON_HEIGHT - 24.0;
        let list_top = SORT_TOP + SORT_HEIGHT + 24.0;
        let per_page = (((bottom_top - 16.0 - list_top) / ROW_HEIGHT).floor() as usize).max(1);
        let books = sorted(library, sort);
        let pages = books.len().div_ceil(per_page).max(1);
        let page = page.min(pages - 1);
        let rows = books
            .iter()
            .skip(page * per_page)
            .take(per_page)
            .enumerate()
            .map(|(i, entry)| {
                let rect = Rect {
                    left: MARGIN,
                    top: list_top + i as f32 * ROW_HEIGHT,
                    width: content,
                    height: ROW_HEIGHT,
                };
                (rect, entry.file.clone())
            })
            .collect();

        let bottom = ui::row(
            MARGIN,
            bottom_top,
            content,
            BUTTON_HEIGHT,
            &[1.5, 1.0, 1.5, 1.0],
        );
        buttons.push((bottom[0], Action::Home, "Home"));
        // A page button only where there's a page to go to.
        if page > 0 {
            buttons.push((bottom[1], Action::Page(Turn::Previous), "‹"));
        }
        if page + 1 < pages {
            buttons.push((bottom[3], Action::Page(Turn::Next), "›"));
        }
        Self {
            sort,
            page,
            pages,
            rows,
            buttons,
            page_label: bottom[2],
        }
    }

    pub(crate) fn sort(&self) -> Sort {
        self.sort
    }

    pub(crate) fn page(&self) -> usize {
        self.page
    }

    pub(crate) fn pages(&self) -> usize {
        self.pages
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        if let Some((_, file)) = self.rows.iter().find(|(rect, _)| rect.contains(x, y)) {
            return Some(Action::Open(file.clone()));
        }
        self.buttons
            .iter()
            .find(|(rect, ..)| rect.contains(x, y))
            .map(|(_, action, _)| action.clone())
    }

    pub(crate) fn draw(
        &self,
        canvas: &mut Canvas,
        ui: &mut Typesetter,
        library: &Library,
        battery: &str,
    ) {
        canvas.clear();
        ui::header(canvas, ui, "Library", battery);
        for (rect, action, label) in &self.buttons {
            let on = *action == Action::Sort(self.sort);
            ui::toggle(canvas, ui, *rect, label, on);
        }
        let page = if self.pages > 1 {
            format!("Page {} of {}", self.page + 1, self.pages)
        } else {
            let count = library.books().len();
            if count == 1 {
                "1 book".to_owned()
            } else {
                format!("{count} books")
            }
        };
        let page = ui::fit(ui, &page, LABEL_SIZE, 400.0, self.page_label.width);
        let page_width = ui.label_width(&page, LABEL_SIZE, 400.0);
        let baseline = self.page_label.top + self.page_label.height / 2.0 + LABEL_SIZE * 0.33;
        let x = self.page_label.left + (self.page_label.width - page_width) / 2.0;
        ui.draw_label(canvas, &page, (x, baseline), LABEL_SIZE, 400.0);

        if self.rows.is_empty() {
            let top = SORT_TOP + SORT_HEIGHT + 24.0 + 60.0;
            ui.draw_label(canvas, "No books yet.", (MARGIN, top), LABEL_SIZE, 400.0);
        }
        for (rect, file) in &self.rows {
            if let Some(entry) = library.get(file) {
                draw_row(canvas, ui, entry, *rect, self.sort);
            }
        }
    }
}

/// A book in the list: its title, with how far through it is if it's been
/// opened, and under that its authors and series.
fn draw_row(canvas: &mut Canvas, ui: &mut Typesetter, entry: &Entry, rect: Rect, sort: Sort) {
    let title_baseline = rect.top + 52.0;
    let mut title_room = rect.width;
    if entry.opened.is_some() {
        let progress = entry.progress.unwrap_or(0.0);
        let text = format!("{}%", (progress * 100.0).round());
        let width = ui.label_width(&text, DETAIL_SIZE, 400.0);
        let at = (rect.right() - width, title_baseline);
        ui.draw_label(canvas, &text, at, DETAIL_SIZE, 400.0);
        title_room -= width + 32.0;
    }
    let title = ui::fit(ui, &entry.title, TITLE_SIZE, 600.0, title_room);
    ui.draw_label(
        canvas,
        &title,
        (rect.left, title_baseline),
        TITLE_SIZE,
        600.0,
    );

    let detail = ui::fit(ui, &detail(entry, sort), DETAIL_SIZE, 400.0, rect.width);
    let at = (rect.left, rect.top + 96.0);
    ui.draw_label(canvas, &detail, at, DETAIL_SIZE, 400.0);
    let (left, _, width, _) = rect.pixels();
    canvas.fill(left, rect.bottom() as i32 - 2, width, 2, 150);
}

/// The line under a title: authors, then series; series first when sorted
/// by series.
fn detail(entry: &Entry, sort: Sort) -> String {
    let authors = Some(entry.authors.join(", ")).filter(|a| !a.is_empty());
    let series = entry.series_text();
    let parts = match sort {
        Sort::Series => [series, authors],
        Sort::Title | Sort::Author => [authors, series],
    };
    parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// Every book in the library, in the order `sort` puts them.
pub(crate) fn sorted(library: &Library, sort: Sort) -> Vec<&Entry> {
    let mut books: Vec<&Entry> = library.books().iter().collect();
    books.sort_by(|a, b| compare(a, b, sort));
    books
}

fn compare(a: &Entry, b: &Entry, sort: Sort) -> Ordering {
    let by_title = || {
        title_key(&a.title)
            .cmp(&title_key(&b.title))
            .then_with(|| a.file.cmp(&b.file))
    };
    match sort {
        Sort::Title => by_title(),
        Sort::Author => author_key(a).cmp(&author_key(b)).then_with(by_title),
        Sort::Series => {
            let name = |e: &Entry| {
                let series = e.series.as_ref();
                (series.is_none(), series.map(|s| s.name.to_lowercase()))
            };
            let index = |e: &Entry| e.series.as_ref().and_then(|s| s.index);
            name(a)
                .cmp(&name(b))
                .then_with(|| match (index(a), index(b)) {
                    (Some(a), Some(b)) => a.total_cmp(&b),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                })
                .then_with(by_title)
        }
    }
}

/// A title as sorted: lower case, without a leading article.
fn title_key(title: &str) -> String {
    let lower = title.trim().to_lowercase();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = lower.strip_prefix(article) {
            return rest.trim_start().to_owned();
        }
    }
    lower
}

/// Books without an author last, then by the first author's surname: the
/// part before a comma, as in "Burns, Anna", or else the last word.
fn author_key(entry: &Entry) -> (bool, String, String) {
    let Some(author) = entry.authors.first() else {
        return (true, String::new(), String::new());
    };
    let author = author.trim().to_lowercase();
    let surname = match author.split_once(',') {
        Some((surname, _)) => surname.trim().to_owned(),
        None => author.split_whitespace().last().unwrap_or("").to_owned(),
    };
    (false, surname, author)
}

#[cfg(test)]
mod tests {
    use book_format::Series;

    use super::*;

    const WIDTH: u32 = 1072;
    const HEIGHT: u32 = 1448;

    fn entry(file: &str, title: &str, author: &str, series: Option<(&str, f64)>) -> Entry {
        Entry {
            file: file.to_owned(),
            size: 1,
            modified: 1,
            key: 1,
            title: title.to_owned(),
            authors: if author.is_empty() {
                Vec::new()
            } else {
                vec![author.to_owned()]
            },
            series: series.map(|(name, index)| Series {
                name: name.to_owned(),
                index: Some(index),
            }),
            cover: false,
            opened: None,
            progress: None,
        }
    }

    fn library(books: &[Entry]) -> Library {
        serde_json::from_value(serde_json::json!({ "books": books })).unwrap()
    }

    fn sample() -> Library {
        library(&[
            entry("road", "The Road", "Cormac McCarthy", None),
            entry(
                "tombs",
                "The Tombs of Atuan",
                "Ursula K. Le Guin",
                Some(("Earthsea", 2.0)),
            ),
            entry(
                "wizard",
                "A Wizard of Earthsea",
                "Ursula K. Le Guin",
                Some(("Earthsea", 1.0)),
            ),
            entry("dune", "Dune", "Frank Herbert", Some(("Dune", 1.0))),
            entry("anon", "Beowulf", "", None),
            entry("milk", "Milkman", "Burns, Anna", None),
        ])
    }

    fn files(library: &Library, sort: Sort) -> Vec<&str> {
        sorted(library, sort)
            .iter()
            .map(|e| e.file.as_str())
            .collect()
    }

    #[test]
    fn sorting_by_title_author_and_series() {
        let library = sample();
        assert_eq!(
            files(&library, Sort::Title),
            ["anon", "dune", "milk", "road", "tombs", "wizard"]
        );
        assert_eq!(
            files(&library, Sort::Author),
            ["milk", "tombs", "wizard", "dune", "road", "anon"]
        );
        assert_eq!(
            files(&library, Sort::Series),
            ["dune", "wizard", "tombs", "anon", "milk", "road"]
        );
    }

    #[test]
    fn the_detail_line_puts_the_sort_first() {
        let library = sample();
        let tombs = library.get("tombs").unwrap();
        assert_eq!(
            detail(tombs, Sort::Title),
            "Ursula K. Le Guin · Earthsea, book 2"
        );
        assert_eq!(
            detail(tombs, Sort::Series),
            "Earthsea, book 2 · Ursula K. Le Guin"
        );
        assert_eq!(detail(library.get("anon").unwrap(), Sort::Title), "");
    }

    #[test]
    fn pages_hold_every_book_once_and_buttons_only_go_where_there_are_pages() {
        let books: Vec<Entry> = (0..20)
            .map(|i| entry(&format!("{i:02}"), &format!("Book {i:02}"), "", None))
            .collect();
        let library = library(&books);
        let first = Layout::new(WIDTH, HEIGHT, &library, Sort::Title, 0);
        let per_page = first.rows.len();
        assert!(per_page >= 6, "{per_page}");
        assert_eq!(first.pages, 20usize.div_ceil(per_page));

        let mut seen = Vec::new();
        for page in 0..first.pages {
            let layout = Layout::new(WIDTH, HEIGHT, &library, Sort::Title, page);
            let actions: Vec<&Action> = layout.buttons.iter().map(|(_, a, _)| a).collect();
            assert_eq!(actions.contains(&&Action::Page(Turn::Previous)), page > 0);
            assert_eq!(
                actions.contains(&&Action::Page(Turn::Next)),
                page + 1 < first.pages
            );
            seen.extend(layout.rows.iter().map(|(_, file)| file.clone()));

            // Nothing overlaps, everything is on screen, and a tap in the
            // middle of each is what it shows.
            let mut areas: Vec<(Rect, Action)> = layout
                .rows
                .iter()
                .map(|(rect, file)| (*rect, Action::Open(file.clone())))
                .collect();
            areas.extend(layout.buttons.iter().map(|(rect, a, _)| (*rect, a.clone())));
            for (i, (rect, action)) in areas.iter().enumerate() {
                assert!(
                    rect.top > HEADER && rect.bottom() <= HEIGHT as f32,
                    "{action:?}"
                );
                assert!(
                    rect.left >= 0.0 && rect.right() <= WIDTH as f32,
                    "{action:?}"
                );
                for (other, other_action) in &areas[i + 1..] {
                    assert!(!rect.overlaps(other), "{action:?} and {other_action:?}");
                }
                let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
                assert_eq!(layout.hit(middle.0, middle.1).as_ref(), Some(action));
            }
        }
        let expected: Vec<String> = (0..20).map(|i| format!("{i:02}")).collect();
        assert_eq!(seen, expected);

        // A page past the end shows the last one.
        let past = Layout::new(WIDTH, HEIGHT, &library, Sort::Title, 99);
        assert_eq!(past.page, first.pages - 1);
    }
}
