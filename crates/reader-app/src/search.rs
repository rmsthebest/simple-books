//! Search: an on-screen keyboard, and the books whose title, authors or
//! series hold every word typed so far, updated with each key.
//!
//! Matching ignores case and accents, so "hjarna" finds "Hjärna" and "e"
//! finds "é". The keys include å, ä and ö for typing them anyway.

use reader_core::{Canvas, Typesetter};

use crate::{
    library::{Entry, Library},
    shelf::{self, Sort},
    ui::{self, HEADER, LABEL_SIZE, MARGIN, Rect},
};

const QUERY_TOP: f32 = HEADER + 24.0;
const QUERY_HEIGHT: f32 = 96.0;
const RESULTS_TOP: f32 = QUERY_TOP + QUERY_HEIGHT + 16.0;
const KEY_HEIGHT: f32 = 96.0;
const KEY_GAP: f32 = 12.0;
const KEY_SIZE: f32 = 44.0;
/// The letter rows. The third is centred, with Delete after it.
const LETTERS: [&str; 3] = ["qwertyuiopå", "asdfghjklöä", "zxcvbnm"];
/// A query this long is long enough.
const MAX_QUERY: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Type(char),
    Delete,
    Clear,
    /// Back to the library.
    Back,
    Open(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    query: String,
    /// The library page to go back to.
    library_page: usize,
    results: Vec<(Rect, String)>,
    /// Matches that didn't fit.
    more: usize,
    /// Where the line under the results goes.
    note_at: f32,
    keys: Vec<(Rect, Action, String)>,
}

impl Layout {
    pub(crate) fn new(
        width: u32,
        height: u32,
        library: &Library,
        query: String,
        library_page: usize,
    ) -> Self {
        let (width, height) = (width as f32, height as f32);
        let content = width - 2.0 * MARGIN;

        // The keyboard, from the bottom up: Back, space and Clear under the
        // letters.
        let rows = LETTERS.len() as f32 + 1.0;
        let keyboard_top = height - 24.0 - rows * KEY_HEIGHT - (rows - 1.0) * KEY_GAP;
        let mut keys = Vec::new();
        let mut top = keyboard_top;
        for (i, letters) in LETTERS.iter().enumerate() {
            let count = letters.chars().count();
            let row = if i == 2 {
                // A blank, the letters, Delete taking two, and a blank: as
                // wide as the rows above.
                let mut shares = vec![1.0];
                shares.extend(std::iter::repeat_n(1.0, count));
                shares.extend([2.0, 1.0]);
                key_row(top, content, &shares)[1..=count + 1].to_vec()
            } else {
                key_row(top, content, &vec![1.0; count])
            };
            for (rect, letter) in row.iter().zip(letters.chars()) {
                keys.push((*rect, Action::Type(letter), letter.to_string()));
            }
            if i == 2 {
                keys.push((row[count], Action::Delete, "Delete".to_owned()));
            }
            top += KEY_HEIGHT + KEY_GAP;
        }
        let bottom = key_row(top, content, &[2.0, 5.0, 2.0]);
        keys.push((bottom[0], Action::Back, "Library".to_owned()));
        keys.push((bottom[1], Action::Type(' '), "space".to_owned()));
        keys.push((bottom[2], Action::Clear, "Clear".to_owned()));

        // As many results as fit above the keyboard, keeping a line for how
        // many more there are.
        let room = keyboard_top - 16.0 - RESULTS_TOP - 56.0;
        let fit = ((room / shelf::ROW_HEIGHT).floor() as usize).max(1);
        let found = matches(library, &query);
        let more = found.len().saturating_sub(fit);
        let results: Vec<(Rect, String)> = found
            .iter()
            .take(fit)
            .enumerate()
            .map(|(i, entry)| {
                let rect = Rect {
                    left: MARGIN,
                    top: RESULTS_TOP + i as f32 * shelf::ROW_HEIGHT,
                    width: content,
                    height: shelf::ROW_HEIGHT,
                };
                (rect, entry.file.clone())
            })
            .collect();
        let note_at = RESULTS_TOP + results.len() as f32 * shelf::ROW_HEIGHT + 48.0;
        Self {
            query,
            library_page,
            results,
            more,
            note_at,
            keys,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn library_page(&self) -> usize {
        self.library_page
    }

    /// The query after `action`, or `None` if it doesn't change it.
    pub(crate) fn typed(&self, action: &Action) -> Option<String> {
        let mut query = self.query.clone();
        match action {
            Action::Type(' ') if query.is_empty() || query.ends_with(' ') => return None,
            Action::Type(_) if query.chars().count() >= MAX_QUERY => return None,
            Action::Type(c) => query.push(*c),
            Action::Delete => {
                query.pop()?;
            }
            Action::Clear if query.is_empty() => return None,
            Action::Clear => query.clear(),
            Action::Back | Action::Open(_) => return None,
        }
        Some(query)
    }

    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Action> {
        if let Some((_, file)) = self.results.iter().find(|(rect, _)| rect.contains(x, y)) {
            return Some(Action::Open(file.clone()));
        }
        // Keys are hit between them too, by the nearest in the row, so a
        // finger landing on a gap still types.
        self.keys
            .iter()
            .find(|(rect, ..)| {
                let grown = Rect {
                    left: rect.left - KEY_GAP / 2.0,
                    top: rect.top - KEY_GAP / 2.0,
                    width: rect.width + KEY_GAP,
                    height: rect.height + KEY_GAP,
                };
                grown.contains(x, y)
            })
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
        ui::header(canvas, ui, "Search", battery);

        // The query, with a bar where the next letter goes.
        let content = canvas.width() as f32 - 2.0 * MARGIN;
        let field = Rect {
            left: MARGIN,
            top: QUERY_TOP,
            width: content,
            height: QUERY_HEIGHT,
        };
        ui::frame(canvas, field);
        let baseline = field.top + field.height / 2.0 + LABEL_SIZE * 0.33;
        let room = field.width - 48.0;
        let shown = fit_end(ui, &self.query, room);
        let text_width = ui.label_width(&shown, LABEL_SIZE, 400.0);
        ui.draw_label(
            canvas,
            &shown,
            (field.left + 20.0, baseline),
            LABEL_SIZE,
            400.0,
        );
        let cursor = field.left + 20.0 + text_width + 4.0;
        canvas.fill(cursor as i32, (field.top + 22.0) as i32, 3, 52, 0);

        let note = if self.query.trim().is_empty() {
            Some("Type part of a title, an author or a series.".to_owned())
        } else if self.results.is_empty() {
            Some("Nothing matches.".to_owned())
        } else if self.more > 0 {
            Some(format!("{} more; type more to narrow it down.", self.more))
        } else {
            None
        };
        for (rect, file) in &self.results {
            if let Some(entry) = library.get(file) {
                shelf::draw_row(canvas, ui, entry, *rect, Sort::Title);
            }
        }
        if let Some(note) = note {
            let note = ui::fit(ui, &note, 32.0, 400.0, content);
            ui.draw_label(canvas, &note, (MARGIN, self.note_at), 32.0, 400.0);
        }

        for (rect, _, label) in &self.keys {
            let size = if label.chars().count() == 1 {
                KEY_SIZE
            } else {
                34.0
            };
            ui::labelled(canvas, ui, *rect, label, size, 500.0, false);
        }
    }
}

fn key_row(top: f32, width: f32, shares: &[f32]) -> Vec<Rect> {
    // Keys are closer together than buttons, to fit eleven across.
    let total: f32 = shares.iter().sum();
    let unit = (width - KEY_GAP * (shares.len() as f32 - 1.0)) / total;
    let mut x = MARGIN;
    shares
        .iter()
        .map(|share| {
            let rect = Rect {
                left: x,
                top,
                width: unit * share,
                height: KEY_HEIGHT,
            };
            x += unit * share + KEY_GAP;
            rect
        })
        .collect()
}

/// The end of `text` that fits in `room`, so what's being typed stays in
/// sight.
fn fit_end(ui: &mut Typesetter, text: &str, room: f32) -> String {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        let shown: String = if start == 0 {
            text.to_owned()
        } else {
            std::iter::once('…')
                .chain(chars[start..].iter().copied())
                .collect()
        };
        if ui.label_width(&shown, LABEL_SIZE, 400.0) <= room {
            return shown;
        }
    }
    String::new()
}

/// The books with every word of `query` in their title, authors or series:
/// those whose title starts with the query first, then those with a word of
/// the title starting with it, then the rest, each by title.
pub(crate) fn matches<'a>(library: &'a Library, query: &str) -> Vec<&'a Entry> {
    let query = fold(query);
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, &Entry)> = library
        .books()
        .iter()
        .filter_map(|entry| {
            let title = fold(&entry.title);
            let mut all = title.clone();
            for author in &entry.authors {
                all.push(' ');
                all.push_str(&fold(author));
            }
            if let Some(series) = &entry.series {
                all.push(' ');
                all.push_str(&fold(&series.name));
            }
            if !words.iter().all(|word| all.contains(word)) {
                return None;
            }
            let rank = if title.starts_with(query.trim()) {
                0
            } else if title
                .split_whitespace()
                .any(|word| word.starts_with(words[0]))
            {
                1
            } else {
                2
            };
            Some((rank, entry))
        })
        .collect();
    found.sort_by(|(rank_a, a), (rank_b, b)| {
        rank_a
            .cmp(rank_b)
            .then_with(|| shelf::compare(a, b, Sort::Title))
    });
    found.into_iter().map(|(_, entry)| entry).collect()
}

/// Text as matched: lower case, accents taken off, apostrophes dropped (so
/// "enders" finds "Ender's") and other punctuation made a space.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        let plain = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
            'ç' | 'č' | 'ć' => 'c',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
            'ñ' | 'ń' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' => 'u',
            'ý' | 'ÿ' => 'y',
            'š' | 'ś' => 's',
            'ž' | 'ź' | 'ż' => 'z',
            'ł' => 'l',
            'æ' => {
                out.push('a');
                'e'
            }
            'ß' => {
                out.push('s');
                's'
            }
            '\'' | '’' => continue,
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        out.push(plain);
    }
    out
}

#[cfg(test)]
mod tests {
    use book_format::Series;

    use super::*;

    const WIDTH: u32 = 1072;
    const HEIGHT: u32 = 1448;

    fn entry(file: &str, title: &str, author: &str, series: Option<&str>) -> Entry {
        Entry {
            file: file.to_owned(),
            size: 1,
            modified: 1,
            key: 1,
            title: title.to_owned(),
            authors: vec![author.to_owned()],
            series: series.map(|name| Series {
                name: name.to_owned(),
                index: None,
            }),
            cover: false,
            opened: None,
            progress: None,
            id: None,
            revision: None,
            added: None,
        }
    }

    fn sample() -> Library {
        let books = [
            entry("hjarna", "Träna din hjärna", "Anders Hansen", None),
            entry(
                "ender",
                "Ender's Game",
                "Orson Scott Card",
                Some("Ender's Saga"),
            ),
            entry("road", "The Road", "Cormac McCarthy", None),
            entry("roadside", "Roadside Picnic", "Arkady Strugatsky", None),
            entry(
                "wizard",
                "A Wizard of Earthsea",
                "Ursula K. Le Guin",
                Some("Earthsea"),
            ),
            entry(
                "tombs",
                "The Tombs of Atuan",
                "Ursula K. Le Guin",
                Some("Earthsea"),
            ),
        ];
        serde_json::from_value(serde_json::json!({ "books": books })).unwrap()
    }

    fn found(library: &Library, query: &str) -> Vec<String> {
        matches(library, query)
            .iter()
            .map(|e| e.file.clone())
            .collect()
    }

    #[test]
    fn every_word_matches_title_author_or_series_ignoring_case_and_accents() {
        let library = sample();
        assert_eq!(found(&library, "HJARNA"), ["hjarna"]);
        assert_eq!(found(&library, "hjär"), ["hjarna"]);
        assert_eq!(found(&library, "enders"), ["ender"]);
        assert_eq!(found(&library, "le guin"), ["tombs", "wizard"]);
        assert_eq!(found(&library, "earthsea tombs"), ["tombs"]);
        assert_eq!(found(&library, "mccarthy"), ["road"]);
        assert_eq!(found(&library, "zzz"), Vec::<String>::new());
        assert_eq!(found(&library, "  "), Vec::<String>::new());
    }

    #[test]
    fn titles_starting_with_the_query_come_first() {
        let library = sample();
        // "Roadside Picnic" starts with it; "The Road" has a word starting
        // with it; both before a match only in an author.
        assert_eq!(found(&library, "road"), ["roadside", "road"]);
        // "A Wizard…" starts with it, "…of Atuan" has a word starting with
        // it, and the rest only have it inside words.
        assert_eq!(
            found(&library, "a"),
            ["wizard", "tombs", "ender", "road", "roadside", "hjarna"]
        );
    }

    #[test]
    fn typing_deleting_and_clearing() {
        let library = sample();
        let layout = Layout::new(WIDTH, HEIGHT, &library, String::new(), 0);
        assert_eq!(layout.typed(&Action::Type(' ')), None, "no leading space");
        assert_eq!(layout.typed(&Action::Delete), None);
        assert_eq!(layout.typed(&Action::Clear), None);
        assert_eq!(layout.typed(&Action::Type('r')).as_deref(), Some("r"));
        let layout = Layout::new(WIDTH, HEIGHT, &library, "ro ".to_owned(), 0);
        assert_eq!(layout.typed(&Action::Type(' ')), None, "no double space");
        assert_eq!(layout.typed(&Action::Delete).as_deref(), Some("ro"));
        assert_eq!(layout.typed(&Action::Clear).as_deref(), Some(""));
    }

    #[test]
    fn keys_and_results_dont_overlap_and_each_types_what_it_shows() {
        let library = sample();
        let layout = Layout::new(WIDTH, HEIGHT, &library, "e".to_owned(), 3);
        assert_eq!(layout.library_page(), 3);
        let mut areas: Vec<(Rect, Action)> = layout
            .results
            .iter()
            .map(|(rect, file)| (*rect, Action::Open(file.clone())))
            .collect();
        areas.extend(layout.keys.iter().map(|(rect, a, _)| (*rect, a.clone())));
        // Every letter, Delete, space, Clear and Back.
        assert_eq!(layout.keys.len(), 11 + 11 + 7 + 1 + 3);
        for (i, (rect, action)) in areas.iter().enumerate() {
            assert!(rect.top > QUERY_TOP + QUERY_HEIGHT, "{action:?}");
            assert!(rect.bottom() <= HEIGHT as f32, "{action:?}");
            assert!(rect.left >= MARGIN - 0.01 && rect.right() <= WIDTH as f32 - MARGIN + 0.01);
            for (other, other_action) in &areas[i + 1..] {
                assert!(!rect.overlaps(other), "{action:?} and {other_action:?}");
            }
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(layout.hit(middle.0, middle.1).as_ref(), Some(action));
        }
        // Between q and w, a finger still types one of them.
        let (q, w) = (layout.keys[0].0, layout.keys[1].0);
        let between = f32::midpoint(q.right(), w.left);
        assert!(matches!(
            layout.hit(between - 1.0, q.top + 10.0),
            Some(Action::Type('q'))
        ));
    }
}
