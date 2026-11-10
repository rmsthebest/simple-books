//! The session against a fake device: what each tap, button and wait does,
//! and what's saved.

use std::{
    collections::HashMap,
    io::Cursor,
    sync::Arc,
    time::{Duration, Instant},
};

use book_format::{Align, Block, BlockKind, BookReader, Chapter, Limits, Position, Span};
use reader_app::{
    AUTO_SLEEP, Battery, Device, Event, FULL_EVERY, Flow, Light, Opener, Refresh, SLEEP_RETRY,
    SUSPEND_DELAY, Session, Turn,
};

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
        "Test book".into(),
        Vec::new(),
        Some("en".into()),
        Vec::new(),
        &chapters,
        &[],
    )
    .unwrap();
    out.into_inner()
}

fn opener(bytes: Vec<u8>) -> Opener<Cursor<Vec<u8>>> {
    let bytes = Arc::new(bytes);
    Arc::new(move || BookReader::open(Cursor::new(bytes.as_ref().clone()), Limits::default()))
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

    fn with(mut device: Fake) -> Self {
        let now = Instant::now();
        let session = Session::start(opener(book()), 7, &mut device, now).unwrap();
        Self {
            session,
            device,
            now,
        }
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
    fn tick_after(&mut self, wait: Duration) {
        self.now += wait;
        self.session
            .handle(Event::Tick, self.now, &mut self.device)
            .unwrap();
    }

    fn page(&self) -> Option<reader_core::PageRef> {
        self.session.reader().current()
    }
}

/// The middle of the page, away from links and the menu band.
const FORWARD: (f32, f32) = (800.0, 900.0);
const BACK: (f32, f32) = (100.0, 900.0);
/// The top band, which opens the menu.
const MENU: (f32, f32) = (536.0, 60.0);
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
fn exit_saves_the_place_and_the_next_start_returns_to_it() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let page = run.page();
    run.tap(MENU.0, MENU.1);
    // Exit to Nickel is the last button on the bottom row.
    assert_eq!(
        run.tap(1072.0 - 64.0 - 100.0, 6.0 * 120.0 + 60.0),
        Flow::Exit
    );
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
    let before = run.session.reader().position().unwrap();
    run.tap(MENU.0, MENU.1);
    // Text size's + button, on the fourth row.
    run.tap(1072.0 - 64.0 - 52.0, 3.0 * 120.0 + 60.0);
    assert_eq!(run.session.reader().settings().font_size, 44.0);
    let after = run.session.reader().position().unwrap();
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

#[test]
fn stepping_darkness_up_and_back_returns_to_the_page() {
    let mut run = Run::new();
    for _ in 0..8 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let before = run.session.reader().position().unwrap();
    run.tap(MENU.0, MENU.1);
    for _ in 0..3 {
        run.tap(DARKER.0, DARKER.1);
    }
    assert_eq!(run.session.reader().settings().weight, 550.0);
    for _ in 0..3 {
        run.tap(LIGHTER.0, LIGHTER.1);
    }
    assert_eq!(run.session.reader().settings().weight, 400.0);
    assert_eq!(run.session.reader().position(), Some(before));
}

#[test]
fn stepping_text_size_up_and_back_returns_to_the_page() {
    let mut run = Run::new();
    for _ in 0..7 {
        run.tap(FORWARD.0, FORWARD.1);
    }
    let before = run.session.reader().position().unwrap();
    run.tap(MENU.0, MENU.1);
    for _ in 0..3 {
        run.tap(BIGGER.0, BIGGER.1);
    }
    assert_eq!(run.session.reader().settings().font_size, 52.0);
    for _ in 0..3 {
        run.tap(SMALLER.0, SMALLER.1);
    }
    assert_eq!(run.session.reader().settings().font_size, 40.0);
    assert_eq!(run.session.reader().position(), Some(before));
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
        .find(|(x, y)| run.session.reader().link_at(*x, *y).is_some())
        .expect("the link is on the first page");
    run.tap(link.0, link.1);
    assert_eq!(run.page().map(|p| p.chapter), Some(1));
    run.tap(MENU.0, MENU.1);
    // With somewhere to return to, Return is the first of the bottom row.
    run.tap(64.0 + 100.0, 6.0 * 120.0 + 60.0);
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
    assert!(run.session.reader().page_map().is_complete());
    // The next start uses it and paginates nothing more.
    let again = Run::with(Fake {
        files: run.device.files.clone(),
        ..Fake::default()
    });
    assert!(again.session.reader().page_map().is_complete());
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
