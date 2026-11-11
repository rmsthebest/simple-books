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
    AUTO_OFF, AUTO_SLEEP, BATTERY_CHECK, BATTERY_EMPTY, BATTERY_LOW, Battery, BookFile, Books,
    Device, Event, FULL_EVERY, Flow, Light, Network, Refresh, SLEEP_RETRY, SUSPEND_DELAY, Session,
    Sort, Ssh, SshLogin, SyncSetup, Turn,
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
    /// And its pixels.
    pixels: Vec<u8>,
    light: Option<Light>,
    battery: Option<Battery>,
    can_suspend: bool,
    suspended: u32,
    /// Whether it can set a wake alarm, the one set, and how long each
    /// suspend lasts, by the clock, unless the alarm wakes it sooner.
    alarms: bool,
    alarm: Option<u64>,
    sleep_for: u64,
    /// Showing everything inverted, for dark mode.
    dark: bool,
    files: HashMap<String, Vec<u8>>,
    clock: u64,
    /// Whether switching on starts the reader, if that can change.
    starts_reader: Option<bool>,
    /// Updates put in place, and why the next one fails, if it should.
    updates: u32,
    update_fails: Option<String>,
}

impl Device for Fake {
    fn show(&mut self, screen: &reader_core::Canvas, refresh: Refresh) {
        self.shown.push(refresh);
        self.last = screen.pixels().iter().enumerate().fold(0u64, |h, (i, p)| {
            h.wrapping_mul(31).wrapping_add(u64::from(*p) ^ i as u64)
        });
        self.pixels.clear();
        self.pixels.extend_from_slice(screen.pixels());
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
            let woken = self.clock + self.sleep_for;
            self.clock = match self.alarm {
                Some(at) if at < woken => at,
                _ => woken,
            };
        }
        self.can_suspend
    }
    fn set_dark(&mut self, on: bool) {
        self.dark = on;
    }
    fn wake_at(&mut self, at: Option<u64>) -> bool {
        if self.alarms {
            self.alarm = at;
        }
        self.alarms
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
    fn update(&mut self) -> Result<(), String> {
        if let Some(why) = self.update_fails.take() {
            return Err(why);
        }
        self.updates += 1;
        Ok(())
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

    fn at_home(device: Fake) -> Self {
        Self::at_home_with(Shelf::new(), device)
    }

    fn at_home_with(shelf: Shelf, mut device: Fake) -> Self {
        let now = Instant::now();
        let session = Session::start(Arc::new(shelf), &mut device, now).unwrap();
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
    // So does Close, at the right end of the bottom row.
    run.tap(MENU.0, MENU.1);
    assert!(run.session.in_menu());
    run.tap(MENU_CLOSE.0, MENU_CLOSE.1);
    assert!(!run.session.in_menu());
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

fn on_battery(percent: u8) -> Battery {
    Battery {
        percent,
        charging: false,
    }
}

#[test]
fn an_empty_battery_saves_the_place_says_so_and_switches_off() {
    let mut run = Run::with(Fake {
        battery: Some(on_battery(40)),
        can_suspend: true,
        ..Fake::default()
    });
    run.tap(FORWARD.0, FORWARD.1);
    run.device
        .files
        .retain(|name, _| !name.starts_with("position-"));
    run.device.battery = Some(on_battery(BATTERY_EMPTY));
    let page = run.device.last;
    // It's checked once a minute, so the deadline says when.
    assert!(run.session.deadline(run.now) <= run.now + BATTERY_CHECK);
    assert_eq!(run.tick_after(BATTERY_CHECK), Flow::PowerOff);
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
    assert_ne!(run.device.last, page, "the panel says so");
}

#[test]
fn an_empty_battery_on_the_charger_carries_on() {
    let mut run = Run::with(Fake {
        battery: Some(Battery {
            percent: 1,
            charging: true,
        }),
        can_suspend: true,
        ..Fake::default()
    });
    assert_eq!(run.tick_after(BATTERY_CHECK), Flow::Continue);
    assert_eq!(run.tap(FORWARD.0, FORWARD.1), Flow::Continue);
}

#[test]
fn waking_on_an_empty_battery_switches_off_at_once() {
    let mut run = Run::with(Fake {
        battery: Some(on_battery(40)),
        can_suspend: true,
        ..Fake::default()
    });
    run.tick_after(BATTERY_CHECK);
    run.send(Event::Power);
    // Drained while it slept.
    run.device.battery = Some(on_battery(2));
    assert_eq!(run.tick_after(SUSPEND_DELAY), Flow::Continue);
    assert_eq!(run.device.suspended, 1);
    // The clock the checks go by stood still while it slept, so the check
    // is due straight away.
    assert!(run.session.deadline(run.now) <= run.now);
    assert_eq!(run.tick_after(Duration::ZERO), Flow::PowerOff);
}

#[test]
fn asleep_for_a_week_it_switches_off_on_the_sleep_screen() {
    let mut run = Run::with(Fake {
        can_suspend: true,
        alarms: true,
        sleep_for: AUTO_OFF.as_secs() * 2,
        ..Fake::default()
    });
    run.send(Event::Power);
    let sleep_screen = run.device.last;
    assert_eq!(run.tick_after(SUSPEND_DELAY), Flow::PowerOff);
    assert_eq!(run.device.suspended, 1);
    assert_eq!(run.device.last, sleep_screen, "the cover stays up");
}

#[test]
fn woken_sooner_it_wakes_as_usual_without_the_alarm() {
    let mut run = Run::with(Fake {
        can_suspend: true,
        alarms: true,
        sleep_for: 3600,
        ..Fake::default()
    });
    let page = run.device.last;
    run.send(Event::Power);
    assert_eq!(run.tick_after(SUSPEND_DELAY), Flow::Continue);
    assert_eq!(run.device.suspended, 1);
    assert_eq!(run.device.alarm, None);
    assert_eq!(run.device.last, page);
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
    // With somewhere to return to, Return is first on the bottom row.
    run.tap(160.0, 6.0 * 120.0 + 60.0);
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
    // The page went up again with its number under it.
    assert_eq!(run.reader().page_number().map(|(page, _)| page), Some(1));
    assert_eq!(run.device.shown.last(), Some(&Refresh::Partial));
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
    let x = if up { 970.0 } else { 932.0 - 180.0 - 38.0 };
    (x, 463.0 + f32::from(row) * 88.0)
}
const FONT: u8 = 0;
/// The font row's − sits further left, to leave room for the name.
const FONT_DOWN: (f32, f32) = (242.0, 463.0);
const LINE_SPACING: u8 = 2;
const DARKNESS: u8 = 4;
const LIGHT: u8 = 6;
const TURN_GUARD: u8 = 8;
const JUSTIFIED: (f32, f32) = (296.0, 1253.0);
const DARK: (f32, f32) = (856.0, 1253.0);
/// Sync, Kobo and Done along the bottom.
const SETTINGS_SYNC: (f32, f32) = (216.0, 1376.0);
const SETTINGS_KOBO: (f32, f32) = (536.0, 1376.0);
const SETTINGS_DONE: (f32, f32) = (856.0, 1376.0);
/// The sync page's Back, on the right of its bottom row.
const SYNC_BACK: (f32, f32) = (1000.0, 1376.0);
/// On the library screen: the first two books, sorting by author, and Home.
const LIBRARY_FIRST: (f32, f32) = (536.0, 306.0);
const LIBRARY_SECOND: (f32, f32) = (536.0, 438.0);
const SORT_AUTHOR: (f32, f32) = (416.0, 172.0);
const LIBRARY_SEARCH: (f32, f32) = (896.0, 172.0);
/// On the search screen: the first result, and the keys along the bottom.
const SEARCH_FIRST: (f32, f32) = (536.0, 306.0);
const KEY_DELETE: (f32, f32) = (844.0, 1268.0);
const KEY_LIBRARY: (f32, f32) = (906.0, 1376.0);
const KEY_SPACE: (f32, f32) = (536.0, 1376.0);
const KEY_CLEAR: (f32, f32) = (166.0, 1376.0);

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
const LIBRARY_HOME: (f32, f32) = (874.0, 1376.0);
/// The menu's bottom row: Return (after a link), Home, Settings and Close.
/// Home's and Settings' spots are inside them with or without Return.
const MENU_HOME: (f32, f32) = (315.0, 6.0 * 120.0 + 60.0);
const MENU_SETTINGS: (f32, f32) = (610.0, 6.0 * 120.0 + 60.0);
const MENU_CLOSE: (f32, f32) = (1000.0, 6.0 * 120.0 + 60.0);

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
    // The first tick, for the battery, has nothing to draw.
    run.now += Duration::from_secs(1);
    run.session
        .handle(Event::Tick, run.now, &mut run.device)
        .unwrap();
    assert_eq!(run.device.shown.len(), shown);
    let due = run.session.deadline(run.now);
    assert!(due <= run.now + Duration::from_secs(60), "{due:?}");
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
fn dark_mode_is_kept_and_left_off_for_switching_off() {
    let mut run = Run::new();
    run.settings();
    assert!(!run.device.dark);
    run.tap(DARK.0, DARK.1);
    assert!(run.session.preferences().dark);
    assert!(run.device.dark);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
    // Kept for next time.
    run.device.dark = false;
    let mut run = Run::at_home(run.device);
    assert!(run.device.dark);
    // What's left on the panel when it's off is as printed.
    run.send(Event::PowerHeld);
    run.tap(POWER_OFF.0, POWER_OFF.1);
    assert!(!run.device.dark);
}

#[test]
fn a_font_found_on_the_device_sets_the_book() {
    let fonts = concat!(env!("CARGO_MANIFEST_DIR"), "/../reader-core/fonts");
    reader_core::find_fonts(&[fonts.into()]);
    let mut run = Run::new();
    run.settings();
    let before = run.device.last;
    run.step(FONT, true);
    assert_eq!(
        run.session.preferences().font.as_deref(),
        Some("Intel One Mono")
    );
    assert_ne!(run.device.last, before, "the name and the sample change");
    // It comes only in regular and bold, so there's no darkness to step.
    let weight = run.session.preferences().weight;
    run.step(DARKNESS, true);
    assert_eq!(run.session.preferences().weight, weight);
    run.tap(SETTINGS_DONE.0, SETTINGS_DONE.1);
    run.tap(CURRENT.0, CURRENT.1);
    assert_eq!(
        run.reader().settings().font.as_deref(),
        Some("Intel One Mono")
    );

    let mut again = Run::at_home(run.device);
    assert_eq!(
        again.session.preferences().font.as_deref(),
        Some("Intel One Mono")
    );
    // − goes back to Literata, kept as none.
    again.settings();
    again.tap(FONT_DOWN.0, FONT_DOWN.1);
    assert_eq!(again.session.preferences().font, None);
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
        updates: false,
    };
    run.session.enable_sync(setup, &mut run.device);
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
    let offer = run.device.last;
    assert_eq!(run.tap(USB_CONNECT.0, USB_CONNECT.1), Flow::Usb);
    assert!(run.device.files.keys().any(|k| k.starts_with("position-")));
    // Nickel takes a while, so the screen says so first.
    assert_ne!(run.device.last, offer);
    assert_eq!(run.device.shown.last(), Some(&Refresh::Full));
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
const KOBO_UNINSTALL: (f32, f32) = (536.0, 788.0);
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

const KOBO_UPDATE: (f32, f32) = (300.0, 1376.0);

/// On the Kobo screen of a reader at v1, which updates itself.
fn on_the_kobo_screen(dir: &std::path::Path, device: Fake) -> Run {
    if !dir.join("version").exists() {
        std::fs::write(dir.join("version"), "v1\n").unwrap();
    }
    let mut run = Run::at_home(device);
    let setup = SyncSetup {
        dir: dir.to_owned(),
        server: None,
        name: "Test".to_owned(),
        network: Arc::new(reader_app::AlwaysOnline),
        ssh: None,
        updates: true,
    };
    run.session.enable_sync(setup, &mut run.device);
    run.tap(HOME_SETTINGS.0, HOME_SETTINGS.1);
    run.tap(SETTINGS_KOBO.0, SETTINGS_KOBO.1);
    run
}

/// As [`on_the_kobo_screen`], having downloaded v2.
fn with_an_update(dir: &std::path::Path, device: Fake) -> Run {
    std::fs::create_dir_all(dir.join("update")).unwrap();
    std::fs::write(
        dir.join("update/ready.json"),
        r#"{"version":"v2","files":[]}"#,
    )
    .unwrap();
    on_the_kobo_screen(dir, device)
}

#[test]
fn update_puts_the_downloaded_version_in_place_and_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = with_an_update(dir.path(), Fake::default());
    assert_eq!(run.tap(KOBO_UPDATE.0, KOBO_UPDATE.1), Flow::Update);
    assert_eq!(run.device.updates, 1);
}

#[test]
fn an_update_that_fails_says_why_and_stays() {
    let dir = tempfile::tempdir().unwrap();
    let device = Fake {
        update_fails: Some("No room.".to_owned()),
        ..Fake::default()
    };
    let mut run = with_an_update(dir.path(), device);
    let before = run.device.last;
    assert_eq!(run.tap(KOBO_UPDATE.0, KOBO_UPDATE.1), Flow::Continue);
    assert_ne!(run.device.last, before, "it says why");
    assert_eq!(run.tap(KOBO_UPDATE.0, KOBO_UPDATE.1), Flow::Update);
}

#[test]
fn a_low_battery_waits_for_the_charger_to_update() {
    let dir = tempfile::tempdir().unwrap();
    let device = Fake {
        battery: Some(on_battery(BATTERY_LOW)),
        ..Fake::default()
    };
    let mut run = with_an_update(dir.path(), device);
    let before = run.device.last;
    assert_eq!(run.tap(KOBO_UPDATE.0, KOBO_UPDATE.1), Flow::Continue);
    assert_eq!(run.device.updates, 0);
    assert_ne!(run.device.last, before, "it says why");
    run.device.battery = Some(Battery {
        percent: BATTERY_LOW,
        charging: true,
    });
    assert_eq!(run.tap(KOBO_UPDATE.0, KOBO_UPDATE.1), Flow::Update);
}

/// On the sync screen without SSH, Update is just above the bottom row.
const SYNC_UPDATE: (f32, f32) = (536.0, 1256.0);

#[test]
fn the_sync_screen_offers_the_update_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = with_an_update(dir.path(), Fake::default());
    run.tap(KOBO_BACK.0 + 400.0, KOBO_BACK.1);
    run.tap(SETTINGS_SYNC.0, SETTINGS_SYNC.1);
    assert!(run.session.in_sync());
    assert_eq!(run.tap(SYNC_UPDATE.0, SYNC_UPDATE.1), Flow::Update);
    assert_eq!(run.device.updates, 1);
}

#[test]
fn a_new_version_says_it_is_up_until_tapped() {
    let mut run = Run::at_home(Fake::default());
    let home = run.device.last;
    run.session.updated("v2", &mut run.device, run.now);
    assert!(run.session.showing_updated());
    assert_ne!(run.device.last, home);
    // Where Settings is on the home screen: this tap only goes on.
    run.tap(HOME_SETTINGS.0, HOME_SETTINGS.1);
    assert!(!run.session.showing_updated());
    assert!(run.session.at_home());
    assert_eq!(run.device.last, home);
}

#[test]
fn with_nothing_downloaded_there_is_no_update() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = on_the_kobo_screen(dir.path(), Fake::default());
    // The whole row is Back.
    assert_eq!(run.tap(KOBO_UPDATE.0, KOBO_UPDATE.1), Flow::Continue);
    assert!(run.session.in_settings());
    assert_eq!(run.device.updates, 0);
}

/// A dictionary file with "road" in it.
fn dictionary() -> Vec<u8> {
    let mut builder = dictionary::Builder::default();
    builder.add(
        dictionary::Entry {
            word: "road".to_owned(),
            class: "noun".to_owned(),
            senses: vec![dictionary::Sense {
                translations: "väg".to_owned(),
                ..dictionary::Sense::default()
            }],
        },
        &[],
    );
    builder.finish("a test dictionary").into_bytes()
}

impl Run {
    /// Where on the page a word is, the first found going down the middle
    /// of the left third, with its text.
    fn word(&self, wanted: &str) -> (f32, f32) {
        let reader = self.reader();
        for y in (100..1400u16).step_by(10) {
            for x in (60..1000u16).step_by(20) {
                let (x, y) = (f32::from(x), f32::from(y));
                if reader.word_at(x, y).is_some_and(|w| w.text == wanted) {
                    return (x, y);
                }
            }
        }
        panic!("no {wanted:?} on the page");
    }
}

#[test]
fn holding_a_word_looks_it_up_and_a_tap_puts_the_page_back() {
    let mut run = Run::new();
    run.device
        .files
        .insert(reader_app::DICTIONARY.to_owned(), dictionary());
    // Settle the page's neighbours, so the page's own screen is known.
    run.tick_after(Duration::from_secs(1));
    let page = run.page();
    let (x, y) = run.word("road");
    let plain = run.device.last;
    run.send(Event::Hold { x, y });
    assert_eq!(run.session.looking_up(), Some("road"));
    assert_eq!(run.device.shown.last(), Some(&Refresh::Partial));
    assert_ne!(run.device.last, plain, "the translation is over the page");
    // A tap in the right third closes it, without turning.
    run.tap(FORWARD.0, FORWARD.1);
    assert_eq!(run.session.looking_up(), None);
    assert_eq!(run.page(), page);
    assert_eq!(run.device.last, plain);
    // A word that isn't in it says so, and the turn button closes that and
    // turns.
    let (x, y) = run.word("south");
    run.send(Event::Hold { x, y });
    assert_eq!(run.session.looking_up(), Some("south"));
    run.send(Event::Turn(Turn::Next));
    assert_eq!(run.session.looking_up(), None);
    assert!(run.page() > page);
}

#[test]
fn without_the_dictionary_holding_a_word_says_so() {
    let mut run = Run::new();
    let (x, y) = run.word("road");
    let plain = run.device.last;
    run.send(Event::Hold { x, y });
    assert_eq!(run.session.looking_up(), Some("road"));
    assert_ne!(run.device.last, plain);
    // Put there later, it's found then.
    run.tap(FORWARD.0, FORWARD.1);
    run.device
        .files
        .insert(reader_app::DICTIONARY.to_owned(), dictionary());
    run.send(Event::Hold { x, y });
    assert_eq!(run.session.looking_up(), Some("road"));
}

#[test]
fn holding_anywhere_but_a_page_is_a_tap() {
    let mut run = Run::new();
    // In the middle third, away from words: the bottom margin.
    run.send(Event::Hold {
        x: 536.0,
        y: 1430.0,
    });
    assert_eq!(run.session.looking_up(), None);
    assert!(!run.session.in_menu(), "a hold on the page isn't a tap");
    run.tap(MENU.0, MENU.1);
    assert!(run.session.in_menu());
    run.send(Event::Hold {
        x: MENU_SETTINGS.0,
        y: MENU_SETTINGS.1,
    });
    assert!(run.session.in_settings(), "but it is in the menu");
}

/// A book with a cover all one gray, 60 by 90.
const COVERED: &str = "covered.book";
const COVER_GRAY: u8 = 90;

fn covered_book() -> Vec<u8> {
    let mut png = Vec::new();
    image::GrayImage::from_pixel(60, 90, image::Luma([COVER_GRAY]))
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let mut out = Cursor::new(Vec::new());
    book_format::write(
        &mut out,
        book_format::About {
            title: "Covered".into(),
            cover: Some(0),
            ..book_format::About::default()
        },
        Vec::new(),
        &[Chapter {
            blocks: (300..310).map(paragraph).collect(),
        }],
        &[book_format::NewImage {
            info: book_format::ImageInfo {
                width: 60,
                height: 90,
                kind: book_format::ImageKind::Png,
            },
            bytes: png,
        }],
    )
    .unwrap();
    out.into_inner()
}

fn covered_shelf() -> Shelf {
    let mut shelf = Shelf::new();
    shelf.0.insert(COVERED.to_owned(), covered_book());
    shelf
}

impl Fake {
    fn pixel(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * 1072 + x]
    }

    /// Whether the last screen was the cover, filling the screen's height
    /// in the middle, with white either side.
    fn shows_the_cover(&self) -> bool {
        [(536, 10), (536, 724), (536, 1440)]
            .iter()
            .all(|&(x, y)| self.pixel(x, y) == COVER_GRAY)
            && self.pixel(5, 724) == 255
            && self.pixel(1066, 724) == 255
    }

    /// Whether it was the notice along the bottom: its black rule.
    fn shows_the_asleep_notice(&self) -> bool {
        self.pixel(536, 1448 - 160 + 1) == 0
    }
}

#[test]
fn asleep_it_shows_the_cover_of_the_book_being_read() {
    let mut run = Run::at_home_with(
        covered_shelf(),
        Fake {
            can_suspend: true,
            ..Fake::default()
        },
    );
    run.session.open(COVERED, &mut run.device, run.now).unwrap();
    run.send(Event::Power);
    assert!(run.device.shows_the_cover());
    assert!(!run.device.shows_the_asleep_notice());
    // Awake, it's back on the page.
    run.tick_after(SUSPEND_DELAY);
    assert!(!run.device.shows_the_cover());
    assert!(run.page().is_some());

    // On the home screen with nothing open, the book home shows first.
    let mut home = Run::at_home_with(
        covered_shelf(),
        Fake {
            files: run.device.files.clone(),
            ..Fake::default()
        },
    );
    assert!(home.session.reader().is_none());
    home.send(Event::Power);
    assert!(home.device.shows_the_cover());
}

#[test]
fn without_a_cover_sleep_says_so_over_the_screen() {
    let mut run = Run::at_home_with(covered_shelf(), Fake::default());
    run.session.open(BOOK, &mut run.device, run.now).unwrap();
    run.send(Event::Power);
    assert!(run.device.shows_the_asleep_notice());
    assert!(!run.device.shows_the_cover());
}
