//! The session against a fake device: what each tap, button and wait does,
//! and what's saved.

use std::{
    collections::HashMap,
    io::{self, Cursor},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};

use book_format::{
    Align, Block, BlockKind, BookReader, Chapter, FormatError, Limits, Position, Span,
};
use reader_app::{
    AUTO_SLEEP, Battery, BookFile, Books, Device, Event, FULL_EVERY, Flow, Light, Network, Refresh,
    SLEEP_RETRY, SUSPEND_DELAY, Session, Sort, Ssh, SshLogin, SyncSetup, Turn,
};
use reader_core::Reader;

const WORDS: &[&str] = &[
    "the", "road", "ran", "south", "through", "a", "gray", "country", "of", "ash", "and", "rain",
];

fn paragraph(seed: usize) -> Block {
    let text: Vec<&str> = (0..60)
        .map(|i| WORDS[(seed * 7 + i * 5) % WORDS.len()])
        .collect();
    Block {
        kind: BlockKind::Paragraph { indent_first: true },
        indent: 0,
        align: Align::Default,
        spans: vec![Span::plain(text.join(" "))],
    }
}

/// Two chapters of about a dozen pages each. The first paragraph has a link
/// to the second chapter.
fn book() -> Vec<u8> {
    let mut first: Vec<Block> = (0..60).map(paragraph).collect();
    let mut link = Span::plain("a link");
    link.link = Some(Position {
        chapter: 1,
        block: 10,
        offset: 0,
    });
    first[0].spans.push(link);
    let second: Vec<Block> = (100..160).map(paragraph).collect();
    let chapters = [Chapter { blocks: first }, Chapter { blocks: second }];
    let mut out = Cursor::new(Vec::new());
    book_format::write(
        &mut out,
        book_format::About {
            title: "Test book".into(),
            language: Some("en".into()),
            ..book_format::About::default()
        },
        Vec::new(),
        &chapters,
        &[],
    )
    .unwrap();
    out.into_inner()
}

/// The test book, and a second, smaller one.
const BOOK: &str = "test.book";
const OTHER: &str = "other.book";

struct Shelf(HashMap<String, Vec<u8>>);

impl Shelf {
    fn new() -> Self {
        let mut other = Cursor::new(Vec::new());
        book_format::write(
            &mut other,
            book_format::About {
                title: "Other book".into(),
                ..book_format::About::default()
            },
            Vec::new(),
            &[Chapter {
                blocks: (200..210).map(paragraph).collect(),
            }],
            &[],
        )
        .unwrap();
        Self(HashMap::from([
            (BOOK.to_owned(), book()),
            (OTHER.to_owned(), other.into_inner()),
        ]))
    }
}

impl Books<Cursor<Vec<u8>>> for Shelf {
    fn list(&self) -> Vec<BookFile> {
        self.0
            .iter()
            .map(|(name, bytes)| BookFile {
                name: name.clone(),
                size: bytes.len() as u64,
                // The test book was added last.
                modified: if name == BOOK { 2 } else { 1 },
            })
            .collect()
    }
    fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        self.0
            .get(name)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn open(&self, name: &str) -> Result<BookReader<Cursor<Vec<u8>>>, FormatError> {
        BookReader::open(Cursor::new(self.read(name)?), Limits::default())
    }
}

#[derive(Default)]
struct Fake {
    shown: Vec<Refresh>,
    /// The last screen shown, as a checksum.
    last: u64,
    light: Option<Light>,
    battery: Option<Battery>,
    can_suspend: bool,
    suspended: u32,
    files: HashMap<String, Vec<u8>>,
    clock: u64,
    /// Whether switching on starts the reader, if that can change.
    starts_reader: Option<bool>,
}

impl Device for Fake {
    fn show(&mut self, screen: &reader_core::Canvas, refresh: Refresh) {
        self.shown.push(refresh);
        self.last = screen.pixels().iter().enumerate().fold(0u64, |h, (i, p)| {
            h.wrapping_mul(31).wrapping_add(u64::from(*p) ^ i as u64)
        });
    }
    fn battery(&mut self) -> Option<Battery> {
        self.battery
    }
    fn light(&mut self) -> Light {
        Light {
            brightness: 30,
            warmth: 4,
        }
    }
    fn set_light(&mut self, light: Light) {
        self.light = Some(light);
    }
    fn suspend(&mut self) -> bool {
        if self.can_suspend {
            self.suspended += 1;
        }
        self.can_suspend
    }
    fn load(&mut self, name: &str) -> Option<Vec<u8>> {
        self.files.get(name).cloned()
    }
    fn save(&mut self, name: &str, bytes: &[u8]) {
        self.files.insert(name.to_owned(), bytes.to_vec());
    }
    fn clock(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
    fn starts_reader(&mut self) -> Option<bool> {
        self.starts_reader
    }
    fn set_starts_reader(&mut self, on: bool) -> Result<String, String> {
        self.starts_reader = Some(on);
        Ok(if on { "On." } else { "Off." }.to_owned())
    }
}

struct Run {
    session: Session<Cursor<Vec<u8>>>,
    device: Fake,
    now: Instant,
}

impl Run {
    fn new() -> Self {
        Self::with(Fake {
            can_suspend: true,
            ..Fake::default()
        })
    }

    /// Starts on the home screen, then opens the test book, with what was
    /// shown so far forgotten.
    fn with(device: Fake) -> Self {
        let mut run = Self::at_home(device);
        run.device.shown.clear();
        run.session.open(BOOK, &mut run.device, run.now).unwrap();
        run
    }

    fn at_home(mut device: Fake) -> Self {
        let now = Instant::now();
        let session = Session::start(Arc::new(Shelf::new()), &mut device, now).unwrap();
        assert!(session.at_home());
        Self {
            session,
            device,
            now,
        }
    }

    fn reader(&self) -> &Reader<Cursor<Vec<u8>>> {
        self.session.reader().expect("a book is open")
    }

    /// Sends an event a second after the last one, past the turn guard.
    fn send(&mut self, event: Event) -> Flow {
        self.now += Duration::from_secs(1);
        self.session
            .handle(event, self.now, &mut self.device)
            .unwrap()
    }

    fn tap(&mut self, x: f32, y: f32) -> Flow {
        self.send(Event::Tap { x, y })
    }

    /// Lets `wait` pass with nothing happening, then sends the tick.
    fn tick_after(&mut self, wait: Duration) -> Flow {
        self.now += wait;
        self.session
            .handle(Event::Tick, self.now, &mut self.device)
            .unwrap()
    }

    fn page(&self) -> Option<reader_core::PageRef> {
        self.reader().current()
    }
}

/// The right and left thirds, which turn, and the middle one, which opens
/// the menu; away from links.
const FORWARD: (f32, f32) = (900.0, 900.0);
const BACK: (f32, f32) = (100.0, 900.0);
const MENU: (f32, f32) = (536.0, 900.0);
/// Darkness's − and + buttons, on the fifth row.
const DARKER: (f32, f32) = (1072.0 - 64.0 - 52.0, 4.0 * 120.0 + 60.0);
const LIGHTER: (f32, f32) = (1072.0 - 64.0 - 104.0 - 260.0 + 52.0, 4.0 * 120.0 + 60.0);
/// Text size's − and +, on the fourth row.
const BIGGER: (f32, f32) = (1072.0 - 64.0 - 52.0, 3.0 * 120.0 + 60.0);
const SMALLER: (f32, f32) = (1072.0 - 64.0 - 104.0 - 260.0 + 52.0, 3.0 * 120.0 + 60.0);

#[test]
fn it_starts_with_a_full_refresh_and_leaves_the_light_alone() {
    let run = Run::new();
    assert_eq!(run.device.shown, [Refresh::Full]);
    assert_eq!(run.device.light, None);
}

#[test]
fn taps_turn_and_every_sixth_turn_flashes() {
    let mut run = Run::new();
    let start = run.page().unwrap();
    for _ in 0..FULL_EVERY {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let fulls = run.device.shown[1..]
        .iter()
        .filter(|r| **r == Refresh::Full)
        .count();
    assert_eq!(fulls, 1);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
    assert_eq!(run.page().unwrap().page, start.page + FULL_EVERY);
    run.tap(BACK.0, BACK.1);
    assert_eq!(run.page().unwrap().page, start.page + FULL_EVERY - 1);
}

#[test]
fn a_second_tap_too_soon_is_ignored() {
    let mut run = Run::new();
    run.tap(FORWARD.0, FORWARD.1);
    let page = run.page();
    // 100 ms later, while the panel is still drawing.
    run.now += Duration::from_millis(100);
    run.session
        .handle(
            Event::Tap {
                x: FORWARD.0,
                y: FORWARD.1,
            },
            run.now,
            &mut run.device,
        )
        .unwrap();
    assert_eq!(run.page(), page);
    assert_eq!(run.session.ignored_turns(), 1);
}

#[test]
fn the_page_is_in_thirds_back_menu_forward() {
    let mut run = Run::new();
    let start = run.page().unwrap();
    // The top of the right third turns, as everywhere in it: there's no
    // band along the top for the menu any more.
    run.tap(1000.0, 40.0);
    assert!(!run.session.in_menu());
    assert!(run.page().unwrap() > start);
    run.tap(300.0, 1400.0);
    assert_eq!(run.page().unwrap(), start);
    run.tap(400.0, 40.0);
    assert!(run.session.in_menu());
    assert_eq!(run.page().unwrap(), start, "opening the menu doesn't turn");
}

#[test]
fn the_menu_changes_the_light_and_closes_with_a_full_refresh() {
    let mut run = Run::new();
    let page = run.page();
    run.tap(MENU.0, MENU.1);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Partial));
    assert_eq!(run.page(), page, "opening the menu doesn't turn");
    // The light's + button, on the second row.
    run.tap(1072.0 - 64.0 - 52.0, 120.0 + 60.0);
    assert_eq!(
        run.device.light,
        Some(Light {
            brightness: 40,
            warmth: 4
        })
    );
    // Tapping the page below the panel closes it without turning.
    run.tap(FORWARD.0, 1300.0);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
    assert_eq!(run.page(), page);
    // The light is remembered for next time.
    let again = Run::with(Fake {
        files: run.device.files.clone(),
        ..Fake::default()
    });
    assert_eq!(again.device.light.map(|l| l.brightness), Some(40));
}

#[test]
fn settings_from_the_menu_returns_to_the_book_laid_out_anew() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let page = run.page();
    run.tap(MENU.0, MENU.1);
    run.tap(MENU_SETTINGS.0, MENU_SETTINGS.1);
    assert!(run.session.in_settings());
    // Line spacing up, then the Sync page and back, then Done.
    let (x, y) = setting(LINE_SPACING, true);
    run.tap(x, y);
    run.tap(SETTINGS_SYNC.0, SETTINGS_SYNC.1);
    run.tap(SYNC_BACK.0, SYNC_BACK.1);
    assert!(run.session.in_settings());
    run.tap(SETTINGS_DONE.0, SETTINGS_DONE.1);
    assert!(!run.session.in_settings() && !run.session.at_home());
    assert!(run.reader().settings().line_height > 1.4);
    // Back on the page, near where it was: tapping the middle opens the menu.
    assert_eq!(run.page().unwrap().chapter, page.unwrap().chapter);
    run.tap(MENU.0, MENU.1);
    assert!(run.session.in_menu());
}

#[test]
fn exit_saves_the_place_and_the_next_start_returns_to_it() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let page = run.page();
    // Nickel is on the power screen now, not in the menu.
    run.send(Event::PowerHeld);
    assert_eq!(run.tap(POWER_NICKEL.0, POWER_NICKEL.1), Flow::Exit);
    let again = Run::with(Fake {
        files: run.device.files.clone(),
        ..Fake::default()
    });
    assert_eq!(again.page(), page);
}

#[test]
fn bigger_text_keeps_the_place() {
    let mut run = Run::new();
    for _ in 0..8 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let before = run.reader().position().unwrap();
    run.tap(MENU.0, MENU.1);
    // Text size's + button, on the fourth row.
    run.tap(1072.0 - 64.0 - 52.0, 3.0 * 120.0 + 60.0);
    assert_eq!(run.reader().settings().font_size, 44.0);
    let after = run.reader().position().unwrap();
    assert_eq!(after.chapter, before.chapter);
    // The page starts at or before where the old one did.
    assert!(after <= before, "{after:?} after {before:?}");
    assert!(
        after.block + 3 >= before.block,
        "{after:?} far before {before:?}"
    );
}

#[test]
fn the_power_button_sleeps_and_wakes_to_the_page() {
    let mut run = Run::new();
    run.tap(FORWARD.0, FORWARD.1);
    let page_screen = run.device.last;
    run.send(Event::Power);
    // The sleep screen is up, and it waits for the panel before suspending.
    assert_ne!(run.device.last, page_screen);
    assert_eq!(run.device.suspended, 0);
    assert!(run.session.deadline(run.now) <= run.now + SUSPEND_DELAY);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
    // Back on the page after waking, with a full refresh.
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
    assert_eq!(run.device.last, page_screen);
    // The press that woke it, arriving again, doesn't put it back to sleep.
    run.now += Duration::from_millis(500);
    run.session
        .handle(Event::Power, run.now, &mut run.device)
        .unwrap();
    assert_eq!(run.device.suspended, 1);
    // The place was saved before sleeping.
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
}

#[test]
fn while_charging_it_stays_on_the_sleep_screen_until_unplugged() {
    let mut run = Run::with(Fake {
        battery: Some(Battery {
            percent: 50,
            charging: true,
        }),
        ..Fake::default()
    });
    let page_screen = run.device.last;
    run.send(Event::Power);
    assert_eq!(run.device.suspended, 0);
    assert_ne!(run.device.last, page_screen, "the sleep screen is up");
    // Still plugged in: no sleep.
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 0);
    // Unplugged: it sleeps on the next try.
    run.device.battery = Some(Battery {
        percent: 50,
        charging: false,
    });
    run.device.can_suspend = true;
    run.tick_after(SLEEP_RETRY);
    assert_eq!(run.device.suspended, 1);
    assert_eq!(run.device.last, page_screen);
}

#[test]
fn a_tap_wakes_it_from_the_sleep_screen() {
    let mut run = Run::new();
    run.device.can_suspend = false;
    let page = run.page();
    run.send(Event::Power);
    run.tap(FORWARD.0, FORWARD.1);
    // Woken, not turned.
    assert_eq!(run.page(), page);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
}

#[test]
fn it_sleeps_after_a_while_without_a_touch() {
    let mut run = Run::new();
    assert!(run.session.deadline(run.now) <= run.now + AUTO_SLEEP);
    run.tick_after(AUTO_SLEEP);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
}

#[test]
fn a_refused_suspend_is_tried_again() {
    let mut run = Run::new();
    run.device.can_suspend = false;
    let page_screen = run.device.last;
    run.send(Event::Power);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 0);
    // Still on the sleep screen, trying again soon.
    assert_ne!(run.device.last, page_screen);
    assert!(run.session.deadline(run.now) <= run.now + SLEEP_RETRY);
    run.device.can_suspend = true;
    run.tick_after(SLEEP_RETRY);
    assert_eq!(run.device.suspended, 1);
    assert_eq!(run.device.last, page_screen);
}

/// Whether the page starts with the line that held `first`: in the same
/// block, at most a line's worth of characters before it.
fn starts_with_the_line_of(run: &Run, first: Position) -> bool {
    let at = run.reader().position().unwrap();
    at.chapter == first.chapter
        && at.block == first.block
        && at.offset <= first.offset
        && first.offset - at.offset < 80
}

#[test]
fn stepping_darkness_keeps_the_first_line_at_the_top() {
    let mut run = Run::new();
    for _ in 0..8 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let before = run.reader().position().unwrap();
    run.tap(MENU.0, MENU.1);
    for _ in 0..3 {
        run.tap(DARKER.0, DARKER.1);
        assert!(starts_with_the_line_of(&run, before));
    }
    assert_eq!(run.reader().settings().weight, 550.0);
    for _ in 0..3 {
        run.tap(LIGHTER.0, LIGHTER.1);
    }
    assert_eq!(run.reader().settings().weight, 400.0);
    assert_eq!(run.reader().position(), Some(before));
}

#[test]
fn stepping_text_size_keeps_the_first_line_at_the_top() {
    let mut run = Run::new();
    for _ in 0..7 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let before = run.reader().position().unwrap();
    // Mid-paragraph, so the line breaks around it really change.
    assert_ne!(before.offset, 0);
    run.tap(MENU.0, MENU.1);
    for _ in 0..3 {
        run.tap(BIGGER.0, BIGGER.1);
        assert!(
            starts_with_the_line_of(&run, before),
            "{:?} for {before:?}",
            run.reader().position()
        );
    }
    assert_eq!(run.reader().settings().font_size, 52.0);
    for _ in 0..3 {
        run.tap(SMALLER.0, SMALLER.1);
    }
    assert_eq!(run.reader().settings().font_size, 40.0);
    assert_eq!(run.reader().position(), Some(before));
}

#[test]
fn a_link_goes_there_and_the_menu_returns() {
    let mut run = Run::new();
    let page = run.page();
    let link = (0..1448u16)
        .step_by(4)
        .flat_map(|y: u16| {
            (0..1072u16)
                .step_by(8)
                .map(move |x| (f32::from(x), f32::from(y)))
        })
        .find(|(x, y)| run.reader().link_at(*x, *y).is_some())
        .expect("the link is on the first page");
    run.tap(link.0, link.1);
    assert_eq!(run.page().map(|p| p.chapter), Some(1));
    run.tap(MENU.0, MENU.1);
    // With somewhere to return to, Return is second on the bottom row,
    // after Close.
    run.tap(390.0, 6.0 * 120.0 + 60.0);
    assert_eq!(run.page(), page);
}

#[test]
fn the_page_map_is_saved_once_the_book_is_paginated() {
    let mut run = Run::new();
    for _ in 0..200 {
        if run.device.files.keys().any(|k| k.starts_with("pages-")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        run.send(Event::Tick);
    }
    assert!(run.device.files.keys().any(|k| k.starts_with("pages-")));
    assert!(run.reader().page_map().is_complete());
    // The next start uses it and paginates nothing more.
    let again = Run::with(Fake {
        files: run.device.files.clone(),
        ..Fake::default()
    });
    assert!(again.reader().page_map().is_complete());
}

#[test]
fn turning_by_key_works_too() {
    let mut run = Run::new();
    let page = run.page().unwrap();
    run.send(Event::Turn(Turn::Next));
    assert_eq!(run.page().unwrap().page, page.page + 1);
}

#[test]
fn exit_leaves_from_anywhere_and_saves_the_place() {
    let mut run = Run::new();
    run.tap(FORWARD.0, FORWARD.1);
    run.tap(MENU.0, MENU.1);
    assert_eq!(run.send(Event::Exit), Flow::Exit);
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
    // Asleep too.
    let mut run = Run::new();
    run.device.can_suspend = false;
    run.send(Event::Power);
    assert_eq!(run.send(Event::Exit), Flow::Exit);
}

/// On the home screen: the book being read, the first cover in the first
/// row, and Library and Settings along the bottom.
const CURRENT: (f32, f32) = (500.0, 300.0);
const FIRST_TILE: (f32, f32) = (140.0, 750.0);
const HOME_LIBRARY: (f32, f32) = (296.0, 1376.0);
const HOME_SETTINGS: (f32, f32) = (776.0, 1376.0);
/// On the settings screen: a row's − or +, the toggles, and Done.
fn setting(row: u8, up: bool) -> (f32, f32) {
    let x = if up { 967.0 } else { 926.0 - 180.0 - 41.0 };
    (x, 496.0 + f32::from(row) * 94.0)
}
const LINE_SPACING: u8 = 1;
const LIGHT: u8 = 5;
const TURN_GUARD: u8 = 7;
const JUSTIFIED: (f32, f32) = (296.0, 1253.0);
/// Done, Sync and Kobo along the bottom.
const SETTINGS_DONE: (f32, f32) = (216.0, 1376.0);
const SETTINGS_SYNC: (f32, f32) = (536.0, 1376.0);
const SETTINGS_KOBO: (f32, f32) = (856.0, 1376.0);
/// The sync page's Back, on the left of its bottom row.
const SYNC_BACK: (f32, f32) = (150.0, 1376.0);
/// On the library screen: the first two books, sorting by author, and Home.
const LIBRARY_FIRST: (f32, f32) = (536.0, 306.0);
const LIBRARY_SECOND: (f32, f32) = (536.0, 438.0);
const SORT_AUTHOR: (f32, f32) = (416.0, 172.0);
const LIBRARY_SEARCH: (f32, f32) = (896.0, 172.0);
/// On the search screen: the first result, and the keys along the bottom.
const SEARCH_FIRST: (f32, f32) = (536.0, 306.0);
const KEY_DELETE: (f32, f32) = (844.0, 1268.0);
const KEY_LIBRARY: (f32, f32) = (166.0, 1376.0);
const KEY_SPACE: (f32, f32) = (536.0, 1376.0);
const KEY_CLEAR: (f32, f32) = (906.0, 1376.0);

/// The middle of a letter's key: eleven to a row in the top two rows, and
/// seven in the third, centred with Delete after them.
#[allow(clippy::cast_precision_loss, reason = "key positions")]
fn key(letter: char) -> (f32, f32) {
    let rows = ["qwertyuiopå", "asdfghjklöä", "zxcvbnm"];
    let (row, at) = rows
        .iter()
        .enumerate()
        .find_map(|(row, keys)| keys.chars().position(|c| c == letter).map(|at| (row, at)))
        .expect("a key");
    let y = 1004.0 + row as f32 * 108.0 + 48.0;
    let x = if row == 2 {
        64.0 + (at as f32 + 1.0) * 88.0 + 38.0
    } else {
        let unit = (944.0 - 10.0 * 12.0) / 11.0;
        64.0 + at as f32 * (unit + 12.0) + unit / 2.0
    };
    (x, y)
}
const LIBRARY_HOME: (f32, f32) = (198.0, 1376.0);
/// The menu's bottom row: Close, Return (after a link), Home and Settings.
/// Home's spot is inside it with or without Return.
const MENU_HOME: (f32, f32) = (560.0, 6.0 * 120.0 + 60.0);
const MENU_SETTINGS: (f32, f32) = (900.0, 6.0 * 120.0 + 60.0);

#[test]
fn it_starts_at_home_with_the_books_listed() {
    let run = Run::at_home(Fake::default());
    assert_eq!(run.device.shown, [Refresh::Full]);
    let added: Vec<&str> = run
        .session
        .library()
        .recently_added()
        .iter()
        .map(|e| e.file.as_str())
        .collect();
    assert_eq!(added, [BOOK, OTHER]);
    assert!(run.session.reader().is_none());
    assert!(run.device.files.contains_key("library.json"));
}

#[test]
fn home_from_the_menu_and_back_to_the_same_page() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let page = run.page();
    run.tap(MENU.0, MENU.1);
    run.tap(MENU_HOME.0, MENU_HOME.1);
    assert!(run.session.at_home());
    let current = run.session.library().current().unwrap();
    assert_eq!(current.file, BOOK);
    assert!(current.progress.is_some_and(|p| p > 0.0 && p < 1.0));
    // The book being read, from the top of the home screen.
    run.tap(CURRENT.0, CURRENT.1);
    assert!(!run.session.at_home());
    assert_eq!(run.page(), page);
}

#[test]
fn another_book_opens_and_the_first_keeps_its_place() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let page = run.page();
    run.tap(MENU.0, MENU.1);
    run.tap(MENU_HOME.0, MENU_HOME.1);
    // Only one book has been read, so the first row is the books added,
    // without the one being read.
    run.tap(FIRST_TILE.0, FIRST_TILE.1);
    assert_eq!(run.reader().manifest().title, "Other book");
    run.tap(MENU.0, MENU.1);
    run.tap(MENU_HOME.0, MENU_HOME.1);
    assert_eq!(
        run.session.library().current().map(|e| e.file.as_str()),
        Some(OTHER)
    );
    // The test book is now the first of the books read lately.
    run.tap(FIRST_TILE.0, FIRST_TILE.1);
    assert_eq!(run.reader().manifest().title, "Test book");
    assert_eq!(run.page(), page);
}

#[test]
fn asleep_at_home_it_wakes_at_home() {
    let mut run = Run::at_home(Fake {
        can_suspend: true,
        ..Fake::default()
    });
    let home = run.device.last;
    run.send(Event::Power);
    assert_ne!(run.device.last, home, "the sleep screen is up");
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
    assert!(run.session.at_home());
    assert_eq!(run.device.last, home);
}

#[test]
fn the_time_in_the_status_moves_on_each_minute() {
    let mut run = Run::at_home(Fake {
        // 09:32:29 UTC; Fake's clock goes up a second each time it's read.
        clock: 1_794_389_549,
        ..Fake::default()
    });
    let shown = run.device.shown.len();
    let home = run.device.last;
    let due = run.session.deadline(run.now);
    assert!(due <= run.now + Duration::from_secs(60), "{due:?}");
    // Nothing to draw before then.
    run.session
        .handle(
            Event::Tick,
            run.now + Duration::from_secs(1),
            &mut run.device,
        )
        .unwrap();
    assert_eq!(run.device.shown.len(), shown);
    run.device.clock += 60;
    run.now = due;
    run.session
        .handle(Event::Tick, run.now, &mut run.device)
        .unwrap();
    assert_eq!(run.device.shown[shown..], [Refresh::Partial]);
    assert_ne!(run.device.last, home, "the new minute is drawn");
    assert!(run.session.at_home());
    // Not while reading: no bar there.
    let mut run = Run::with(Fake {
        clock: 1_794_389_549,
        ..Fake::default()
    });
    let shown = run.device.shown.len();
    run.device.clock += 60;
    run.now += Duration::from_secs(61);
    run.session
        .handle(Event::Tick, run.now, &mut run.device)
        .unwrap();
    assert_eq!(run.device.shown.len(), shown);
}

#[test]
fn home_has_no_way_out_to_nickel() {
    let mut run = Run::at_home(Fake::default());
    // Where Exit to Nickel was, Settings is now.
    assert_eq!(run.tap(900.0, 1376.0), Flow::Continue);
    assert!(run.session.in_settings());
}

#[test]
fn the_library_lists_every_book_by_title_and_opens_one() {
    let mut run = Run::at_home(Fake::default());
    run.tap(HOME_LIBRARY.0, HOME_LIBRARY.1);
    assert!(run.session.in_library());
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
    // "Other book" comes before "Test book".
    run.tap(LIBRARY_SECOND.0, LIBRARY_SECOND.1);
    assert_eq!(run.reader().manifest().title, "Test book");
    run.tap(MENU.0, MENU.1);
    run.tap(MENU_HOME.0, MENU_HOME.1);
    assert!(run.session.at_home());
    run.tap(HOME_LIBRARY.0, HOME_LIBRARY.1);
    run.tap(LIBRARY_FIRST.0, LIBRARY_FIRST.1);
    assert_eq!(run.reader().manifest().title, "Other book");
}

#[test]
fn the_library_sort_is_kept_and_home_goes_home() {
    let mut run = Run::at_home(Fake::default());
    run.tap(HOME_LIBRARY.0, HOME_LIBRARY.1);
    let by_title = run.device.last;
    run.tap(SORT_AUTHOR.0, SORT_AUTHOR.1);
    assert_eq!(run.session.preferences().library_sort, Sort::Author);
    assert_ne!(run.device.last, by_title, "the chosen sort is drawn");
    assert_eq!(run.device.shown.last(), Some(&Refresh::Partial));
    // Choosing it again changes nothing.
    let shown = run.device.shown.len();
    run.tap(SORT_AUTHOR.0, SORT_AUTHOR.1);
    assert_eq!(run.device.shown.len(), shown);
    run.tap(LIBRARY_HOME.0, LIBRARY_HOME.1);
    assert!(run.session.at_home());

    let again = Run::at_home(run.device);
    assert_eq!(again.session.preferences().library_sort, Sort::Author);
}

#[test]
fn asleep_in_the_library_it_wakes_there() {
    let mut run = Run::at_home(Fake {
        can_suspend: true,
        ..Fake::default()
    });
    run.tap(HOME_LIBRARY.0, HOME_LIBRARY.1);
    let library = run.device.last;
    run.send(Event::Power);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
    assert!(run.session.in_library());
    assert_eq!(run.device.last, library);
    // Turning in the library with one page does nothing.
    let shown = run.device.shown.len();
    run.send(Event::Turn(Turn::Next));
    assert_eq!(run.device.shown.len(), shown);
}

impl Run {
    /// From home to the search screen.
    fn search(&mut self) {
        self.tap(HOME_LIBRARY.0, HOME_LIBRARY.1);
        self.tap(LIBRARY_SEARCH.0, LIBRARY_SEARCH.1);
        assert_eq!(self.session.search_query(), Some(""));
    }

    fn type_text(&mut self, text: &str) {
        for letter in text.chars() {
            if letter == ' ' {
                self.tap(KEY_SPACE.0, KEY_SPACE.1);
            } else {
                let (x, y) = key(letter);
                self.tap(x, y);
            }
        }
    }
}

#[test]
fn search_finds_as_you_type_and_opens_the_book() {
    let mut run = Run::at_home(Fake::default());
    run.search();
    let empty = run.device.last;
    run.type_text("oth");
    assert_eq!(run.session.search_query(), Some("oth"));
    assert_ne!(run.device.last, empty);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Partial));
    run.tap(SEARCH_FIRST.0, SEARCH_FIRST.1);
    assert_eq!(run.reader().manifest().title, "Other book");
}

#[test]
fn every_word_typed_must_match() {
    let mut run = Run::at_home(Fake::default());
    run.search();
    // Both books are "… book", but only one is the test book.
    run.type_text("book test");
    assert_eq!(run.session.search_query(), Some("book test"));
    run.tap(SEARCH_FIRST.0, SEARCH_FIRST.1);
    assert_eq!(run.reader().manifest().title, "Test book");
}

#[test]
fn delete_clear_and_back_to_the_library() {
    let mut run = Run::at_home(Fake::default());
    run.search();
    run.type_text("te");
    run.tap(KEY_DELETE.0, KEY_DELETE.1);
    assert_eq!(run.session.search_query(), Some("t"));
    run.type_text("es");
    run.tap(KEY_CLEAR.0, KEY_CLEAR.1);
    assert_eq!(run.session.search_query(), Some(""));
    // Nothing to delete or clear: nothing is drawn.
    let shown = run.device.shown.len();
    run.tap(KEY_DELETE.0, KEY_DELETE.1);
    run.tap(KEY_CLEAR.0, KEY_CLEAR.1);
    run.tap(KEY_SPACE.0, KEY_SPACE.1);
    assert_eq!(run.device.shown.len(), shown);
    // With nothing typed there are no results to open.
    run.tap(SEARCH_FIRST.0, SEARCH_FIRST.1);
    assert!(run.session.reader().is_none());
    run.tap(KEY_LIBRARY.0, KEY_LIBRARY.1);
    assert!(run.session.in_library());
}

#[test]
fn asleep_in_search_it_wakes_with_the_query() {
    let mut run = Run::at_home(Fake {
        can_suspend: true,
        ..Fake::default()
    });
    run.search();
    run.type_text("oth");
    let screen = run.device.last;
    run.send(Event::Power);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
    assert_eq!(run.session.search_query(), Some("oth"));
    assert_eq!(run.device.last, screen);
}

impl Run {
    fn settings(&mut self) {
        if !self.session.at_home() {
            self.tap(MENU.0, MENU.1);
            self.tap(MENU_HOME.0, MENU_HOME.1);
        }
        self.tap(HOME_SETTINGS.0, HOME_SETTINGS.1);
        assert!(self.session.in_settings());
    }

    fn step(&mut self, row: u8, up: bool) {
        let (x, y) = setting(row, up);
        self.tap(x, y);
    }
}

#[test]
fn settings_lay_the_book_out_again_when_it_is_next_shown() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let block = run.reader().position().unwrap().block;
    run.settings();
    let before = run.device.last;
    run.step(LINE_SPACING, true);
    assert_eq!(run.session.preferences().line_height, 1.5);
    assert_ne!(run.device.last, before, "the value and the sample change");
    assert_eq!(run.device.shown.last(), Some(&Refresh::Partial));
    run.tap(JUSTIFIED.0, JUSTIFIED.1);
    assert!(!run.session.preferences().justify);
    run.tap(SETTINGS_DONE.0, SETTINGS_DONE.1);
    assert!(run.session.at_home());

    run.tap(CURRENT.0, CURRENT.1);
    let settings = run.reader().settings();
    assert_eq!(settings.line_height, 1.5);
    assert!(!settings.justify);
    // The page starts with the line it started with before.
    let after = run.reader().position().unwrap().block;
    assert!(after <= block && after + 1 >= block, "{block} then {after}");

    let again = Run::at_home(run.device);
    assert_eq!(again.session.preferences().line_height, 1.5);
}

#[test]
fn the_light_changes_from_settings_at_once() {
    let mut run = Run::at_home(Fake::default());
    run.settings();
    run.step(LIGHT, true);
    assert_eq!(run.device.light.map(|l| l.brightness), Some(40));
    // Stepping past the end changes and draws nothing.
    for _ in 0..10 {
        run.step(LIGHT, true);
    }
    let shown = run.device.shown.len();
    run.step(LIGHT, true);
    assert_eq!(run.device.shown.len(), shown);
    assert_eq!(run.device.light.map(|l| l.brightness), Some(100));
}

#[test]
fn with_the_turn_guard_off_quick_turns_all_count() {
    let mut run = Run::new();
    run.settings();
    for _ in 0..3 {
        run.step(TURN_GUARD, false);
    }
    assert_eq!(run.session.preferences().turn_guard_ms, 0);
    run.tap(SETTINGS_DONE.0, SETTINGS_DONE.1);
    run.tap(CURRENT.0, CURRENT.1);
    let start = run.page().unwrap();
    for _ in 0..3 {
        run.now += Duration::from_millis(50);
        run.session
            .handle(Event::Turn(Turn::Next), run.now, &mut run.device)
            .unwrap();
    }
    assert_eq!(run.session.ignored_turns(), 0);
    assert_eq!(run.page().unwrap().page, start.page + 3);
}

#[test]
fn asleep_in_settings_it_wakes_there() {
    let mut run = Run::at_home(Fake {
        can_suspend: true,
        ..Fake::default()
    });
    run.settings();
    let screen = run.device.last;
    run.send(Event::Power);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
    assert!(run.session.in_settings());
    assert_eq!(run.device.last, screen);
}

/// Counts what's asked of the network and SSH.
#[derive(Default)]
struct Online {
    ups: AtomicU32,
    downs: AtomicU32,
    ssh_starts: AtomicU32,
    ssh_stops: AtomicU32,
    /// Wi-Fi can't connect.
    unreachable: bool,
}

impl Online {
    fn counts(&self) -> [u32; 4] {
        [&self.ups, &self.downs, &self.ssh_starts, &self.ssh_stops].map(|n| n.load(SeqCst))
    }
}

/// Both traits on one value, shared with the test.
struct Shared(Arc<Online>);

impl Network for Shared {
    fn up(&self) -> Result<(), String> {
        if self.0.unreachable {
            return Err("No Wi-Fi.".to_owned());
        }
        self.0.ups.fetch_add(1, SeqCst);
        Ok(())
    }
    fn down(&self) {
        self.0.downs.fetch_add(1, SeqCst);
    }
}

impl Ssh for Shared {
    fn start(&self) -> Result<SshLogin, String> {
        self.0.ssh_starts.fetch_add(1, SeqCst);
        Ok(SshLogin {
            command: "ssh root@kobo".to_owned(),
            password: Some("abcd-efgh".to_owned()),
        })
    }
    fn stop(&self) {
        self.0.ssh_stops.fetch_add(1, SeqCst);
    }
}

const SYNC_SSH: (f32, f32) = (536.0, 1256.0);

/// On the sync screen, with no server known and SSH from `online`.
fn sync_screen(online: &Arc<Online>, dir: &std::path::Path) -> Run {
    let mut run = Run::at_home(Fake {
        can_suspend: true,
        ..Fake::default()
    });
    let setup = SyncSetup {
        dir: dir.to_owned(),
        server: None,
        name: "Test".to_owned(),
        network: Arc::new(Shared(Arc::clone(online))),
        ssh: Some(Arc::new(Shared(Arc::clone(online)))),
    };
    run.session.enable_sync(setup, &mut run.device, run.now);
    run.tap(HOME_SETTINGS.0, HOME_SETTINGS.1);
    run.tap(SETTINGS_SYNC.0, SETTINGS_SYNC.1);
    assert!(run.session.in_sync());
    run
}

impl Run {
    /// Ticks until the SSH job's done.
    fn until_idle(&mut self) {
        let give_up = Instant::now() + Duration::from_secs(5);
        while self.session.syncing() {
            assert!(Instant::now() < give_up, "the job never ended");
            std::thread::sleep(Duration::from_millis(5));
            self.tick_after(Duration::from_millis(5));
        }
    }
}

#[test]
fn ssh_keeps_the_network_up_until_its_turned_off_or_it_sleeps() {
    let online = Arc::new(Online::default());
    let dir = tempfile::tempdir().unwrap();
    let mut run = sync_screen(&online, dir.path());

    run.tap(SYNC_SSH.0, SYNC_SSH.1);
    run.until_idle();
    assert_eq!(run.session.ssh_login().as_deref(), Some("ssh root@kobo"));
    assert_eq!(online.counts(), [1, 0, 1, 0], "up, and left up");

    run.tap(SYNC_SSH.0, SYNC_SSH.1);
    assert_eq!(run.session.ssh_login(), None);
    assert_eq!(online.counts(), [1, 1, 1, 1], "off, and down");

    // On again, then to sleep: it goes off before suspending.
    run.tap(SYNC_SSH.0, SYNC_SSH.1);
    run.until_idle();
    assert!(run.session.ssh_login().is_some());
    run.send(Event::Power);
    assert_eq!(run.session.ssh_login(), None);
    assert_eq!(online.counts(), [2, 2, 2, 2]);
    run.tick_after(SUSPEND_DELAY);
    assert_eq!(run.device.suspended, 1);
}

#[test]
fn ssh_without_wifi_says_why() {
    let online = Arc::new(Online {
        unreachable: true,
        ..Online::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut run = sync_screen(&online, dir.path());
    let before = run.device.last;
    run.tap(SYNC_SSH.0, SYNC_SSH.1);
    run.until_idle();
    assert_eq!(run.session.ssh_login(), None);
    assert_eq!(
        online.counts(),
        [0, 0, 0, 0],
        "never started, nothing to undo"
    );
    assert_ne!(run.device.last, before, "it says so");
    // And it can be tried again.
    run.tap(SYNC_SSH.0, SYNC_SSH.1);
    run.until_idle();
}

const USB_NOT_NOW: (f32, f32) = (296.0, 1376.0);
const USB_CONNECT: (f32, f32) = (776.0, 1376.0);

#[test]
fn a_computer_plugged_in_offers_nickel_for_usb() {
    let mut run = Run::new();
    run.tap(FORWARD.0, FORWARD.1);
    let page = run.page();
    run.send(Event::Computer(true));
    assert!(run.session.in_usb());
    // Not now: back to the page, and no asking again while plugged in.
    run.tap(USB_NOT_NOW.0, USB_NOT_NOW.1);
    assert!(!run.session.in_usb());
    assert_eq!(run.page(), page);
    run.send(Event::Computer(true));
    assert!(!run.session.in_usb());
    // Unplugged and plugged in again, it asks again, and Connect leaves for
    // Nickel with the place saved.
    run.send(Event::Computer(false));
    run.send(Event::Computer(true));
    assert!(run.session.in_usb());
    run.device
        .files
        .retain(|name, _| !name.starts_with("position-"));
    assert_eq!(run.tap(USB_CONNECT.0, USB_CONNECT.1), Flow::Usb);
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
}

#[test]
fn unplugging_takes_the_usb_screen_away() {
    let mut run = Run::at_home(Fake::default());
    run.send(Event::Computer(true));
    assert!(run.session.in_usb());
    run.send(Event::Computer(false));
    assert!(run.session.at_home());
}

#[test]
fn plugged_in_while_asleep_it_asks_on_waking() {
    let mut run = Run::new();
    run.device.can_suspend = false;
    let page = run.page();
    run.send(Event::Power);
    run.send(Event::Computer(true));
    assert!(!run.session.in_usb(), "still asleep");
    run.send(Event::Power);
    assert!(run.session.in_usb());
    run.tap(USB_NOT_NOW.0, USB_NOT_NOW.1);
    assert_eq!(run.page(), page);
    assert!(!run.session.in_usb());
    // Sleeping from the USB screen wakes to where it came from.
    run.send(Event::Computer(false));
    run.send(Event::Computer(true));
    run.send(Event::Power);
    run.send(Event::Power);
    assert!(run.session.in_usb(), "still plugged in, so it asks again");
    run.tap(USB_NOT_NOW.0, USB_NOT_NOW.1);
    assert_eq!(run.page(), page);
}

/// On the power screen, from holding the power button.
const POWER_OFF: (f32, f32) = (536.0, 552.0);
const POWER_RESTART: (f32, f32) = (536.0, 680.0);
const POWER_NICKEL: (f32, f32) = (536.0, 808.0);
const POWER_CANCEL: (f32, f32) = (536.0, 968.0);

#[test]
fn holding_the_power_button_asks_and_cancel_goes_back() {
    let mut run = Run::new();
    run.tap(FORWARD.0, FORWARD.1);
    let page = run.page();
    run.send(Event::PowerHeld);
    assert!(run.session.in_power());
    // A press, a turn or a tap between the buttons does nothing.
    run.send(Event::Power);
    run.send(Event::Turn(Turn::Next));
    run.tap(536.0, 300.0);
    assert!(run.session.in_power());
    run.tap(POWER_CANCEL.0, POWER_CANCEL.1);
    assert!(!run.session.in_power());
    assert_eq!(run.page(), page);
}

#[test]
fn power_off_and_restart_save_the_place_and_say_so() {
    for (button, flow) in [(POWER_OFF, Flow::PowerOff), (POWER_RESTART, Flow::Restart)] {
        let mut run = Run::new();
        run.tap(FORWARD.0, FORWARD.1);
        run.send(Event::PowerHeld);
        run.device
            .files
            .retain(|name, _| !name.starts_with("position-"));
        let before = run.device.last;
        assert_eq!(run.tap(button.0, button.1), flow);
        assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
        assert_ne!(run.device.last, before, "the panel says so");
    }
    let mut run = Run::at_home(Fake::default());
    run.send(Event::PowerHeld);
    assert_eq!(run.tap(POWER_NICKEL.0, POWER_NICKEL.1), Flow::Exit);
}

#[test]
fn with_nothing_tapped_the_power_screen_goes_to_nickel() {
    let mut run = Run::new();
    run.send(Event::PowerHeld);
    let deadline = run.session.deadline(run.now);
    assert!(deadline <= run.now + Duration::from_secs(15));
    assert_eq!(run.tick_after(Duration::from_secs(5)), Flow::Continue);
    assert!(run.session.in_power());
    assert_eq!(run.tick_after(Duration::from_secs(10)), Flow::Exit);
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
}

#[test]
fn held_on_the_sleep_screen_it_asks_and_cancel_goes_back_to_the_page() {
    let mut run = Run::new();
    run.device.can_suspend = false;
    let page = run.page();
    run.send(Event::Power);
    run.send(Event::PowerHeld);
    assert!(run.session.in_power());
    run.tap(POWER_CANCEL.0, POWER_CANCEL.1);
    assert_eq!(run.page(), page);
    assert!(!run.session.in_power());
}

/// From settings: the Kobo screen.
const KOBO_START_HERE: (f32, f32) = (536.0, 482.0);
const KOBO_UNINSTALL: (f32, f32) = (536.0, 878.0);
const KOBO_BACK: (f32, f32) = (536.0, 1376.0);

#[test]
fn the_kobo_screen_turns_starting_here_on_and_off() {
    let device = Fake {
        starts_reader: Some(false),
        ..Fake::default()
    };
    let mut run = Run::at_home(device);
    run.tap(HOME_SETTINGS.0, HOME_SETTINGS.1);
    run.tap(SETTINGS_KOBO.0, SETTINGS_KOBO.1);
    run.tap(KOBO_START_HERE.0, KOBO_START_HERE.1);
    assert_eq!(run.device.starts_reader, Some(true));
    run.tap(KOBO_START_HERE.0, KOBO_START_HERE.1);
    assert_eq!(run.device.starts_reader, Some(false));
    run.tap(KOBO_BACK.0, KOBO_BACK.1);
    run.tap(SETTINGS_DONE.0, SETTINGS_DONE.1);
    assert!(run.session.at_home());
}

#[test]
fn uninstalling_takes_a_second_tap_and_saves_the_place() {
    let mut run = Run::new();
    run.tap(FORWARD.0, FORWARD.1);
    run.tap(MENU.0, MENU.1);
    run.tap(MENU_HOME.0, MENU_HOME.1);
    run.tap(HOME_SETTINGS.0, HOME_SETTINGS.1);
    run.tap(SETTINGS_KOBO.0, SETTINGS_KOBO.1);
    run.device
        .files
        .retain(|name, _| !name.starts_with("position-"));
    assert_eq!(run.tap(KOBO_UNINSTALL.0, KOBO_UNINSTALL.1), Flow::Continue);
    // A tap elsewhere takes the first one back.
    run.tap(536.0, 1100.0);
    assert_eq!(run.tap(KOBO_UNINSTALL.0, KOBO_UNINSTALL.1), Flow::Continue);
    assert_eq!(run.tap(KOBO_UNINSTALL.0, KOBO_UNINSTALL.1), Flow::Uninstall);
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
}
