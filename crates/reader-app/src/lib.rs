//! The reader app without any device code: what a tap, the power button or
//! the passing of time does, and what goes on the screen.
//!
//! A platform (the desktop emulator, or the Kobo) turns its input into
//! [`Event`]s for [`Session::handle`], and does what the session asks
//! through [`Device`]. Between events it waits until
//! [`Session::deadline`] and then sends [`Event::Tick`].
//!
//! - **Turning:** a tap in the left third goes back a page, anywhere else
//!   forward, and a tap on a link follows it. The [`TurnGuard`] drops turns
//!   that come too soon. Turns use the grey partial refresh, with a full one
//!   every [`FULL_EVERY`] turns to clear ghosting.
//! - **The menu** opens with a tap along the top of the page. It has the
//!   front light, warmth, text size, darkness and contrast, a way back from
//!   a link, and the way out to Nickel.
//! - **Sleep:** the power button, or [`AUTO_SLEEP`] without a touch, saves
//!   the place and suspends, except while charging, which can hang the
//!   Clara BW's kernel.
//! - **Saved:** the place in each book, the preferences, and each book's
//!   page map once it's complete, through [`Device::save`].
#![forbid(unsafe_code)]
#![allow(clippy::cast_precision_loss, reason = "pixel sizes")]

mod menu;

use std::{
    io::{Read, Seek},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use book_format::{BookReader, FormatError, Position};
pub use reader_core::Turn;
use reader_core::{
    Canvas, PageMap, Reader, ReaderError, Settings, TurnGuard, Typesetter, paginate_book,
};
use serde::{Deserialize, Serialize};

/// A full refresh every this many turns clears ghosting.
pub const FULL_EVERY: u32 = 6;
/// Sleep after this long without a touch, unless charging.
pub const AUTO_SLEEP: Duration = Duration::from_mins(10);
/// The power button is ignored this long after waking, since the press that
/// woke the device can arrive again once it's awake.
const WAKE_GRACE: Duration = Duration::from_secs(2);
/// How long the sleep screen stays up before the first try at suspending.
/// The Clara BW's display driver refuses to suspend while the panel is
/// still powered from the last refresh; KOReader waits 15 s for the same
/// reason.
pub const SUSPEND_DELAY: Duration = Duration::from_secs(15);
/// While it can't sleep, such as while charging, it tries again this often.
pub const SLEEP_RETRY: Duration = Duration::from_secs(5);
/// While the book is paginated in the background, it checks this often.
const BACKGROUND_CHECK: Duration = Duration::from_secs(1);
/// The place is saved every this many turns, besides on sleep and exit.
const SAVE_EVERY: u32 = 10;
/// A tap this far down the screen, as a fraction of its height, opens the
/// menu.
const MENU_BAND: f32 = 0.1;

/// Opens the book afresh, for the reader and for background pagination.
pub type Opener<R> = Arc<dyn Fn() -> Result<BookReader<R>, FormatError> + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error(transparent)]
    Reader(#[from] ReaderError),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// A touch, at the place it was let go, in screen pixels.
    Tap {
        x: f32,
        y: f32,
    },
    /// A page turn from a key, in the emulator.
    Turn(Turn),
    /// Return from a link, from a key in the emulator.
    Back,
    Power,
    /// Leave for Nickel at once, such as when the power button is held: the
    /// way out even if the touch panel isn't working.
    Exit,
    /// [`Session::deadline`] has passed.
    Tick,
}

/// How the panel redraws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// GL16: grey, without a flash.
    Partial,
    /// GC16 with a full update: flashes, and clears ghosting.
    Full,
}

impl Refresh {
    /// How long the Clara BW's panel takes, from `docs/clara-bw-hardware.md`.
    pub const fn duration(self) -> Duration {
        match self {
            Refresh::Partial => Duration::from_millis(281),
            Refresh::Full => Duration::from_millis(412),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// The front light. Brightness runs 0–100, warmth 0–10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Light {
    pub brightness: u8,
    pub warmth: u8,
}

/// What the session asks of the device it runs on.
pub trait Device {
    /// Puts `screen` on the panel.
    fn show(&mut self, screen: &Canvas, refresh: Refresh);
    fn battery(&mut self) -> Option<Battery>;
    /// The light as it is now, used until the reader chooses one.
    fn light(&mut self) -> Light;
    fn set_light(&mut self, light: Light);
    /// Sleeps, returning once awake again. False if it couldn't sleep, such
    /// as while charging.
    fn suspend(&mut self) -> bool;
    fn load(&mut self, name: &str) -> Option<Vec<u8>>;
    fn save(&mut self, name: &str, bytes: &[u8]);
}

/// Whether to keep running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Back to Nickel.
    Exit,
}

/// What the reader has chosen, kept across books and restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub font_size: f32,
    pub weight: f32,
    pub gamma: f32,
    pub justify: bool,
    pub hyphenate: bool,
    pub optimal_breaks: bool,
    /// `None` until changed here, so the light stays as Nickel left it.
    pub light: Option<Light>,
}

impl Default for Preferences {
    fn default() -> Self {
        let settings = Settings::default();
        Self {
            font_size: settings.font_size,
            weight: settings.weight,
            gamma: settings.gamma,
            justify: settings.justify,
            hyphenate: settings.hyphenate,
            optimal_breaks: settings.optimal_breaks,
            light: None,
        }
    }
}

impl Preferences {
    fn settings(&self) -> Settings {
        Settings {
            font_size: self.font_size,
            weight: self.weight,
            gamma: self.gamma,
            justify: self.justify,
            hyphenate: self.hyphenate,
            optimal_breaks: self.optimal_breaks,
            ..Settings::default()
        }
    }
}

const PREFERENCES: &str = "preferences.json";

fn position_file(book: u64) -> String {
    format!("position-{book:016x}.json")
}

fn pages_file(key: u64) -> String {
    format!("pages-{key:016x}.json")
}

#[derive(Debug)]
enum Mode {
    Reading,
    Menu(menu::Layout),
    /// On the sleep screen, waiting to suspend, or awake again after a
    /// failed try.
    Asleep {
        /// When to try to suspend next.
        suspend_at: Instant,
    },
}

/// The rest of the book being paginated on another thread.
struct Background {
    receive: mpsc::Receiver<PageMap>,
    stop: Arc<AtomicBool>,
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub struct Session<R> {
    open: Opener<R>,
    book_key: u64,
    reader: Reader<R>,
    preferences: Preferences,
    light: Light,
    /// For the menu and the sleep screen.
    ui: Typesetter,
    /// The page with the menu or the sleep notice drawn over it.
    screen: Canvas,
    mode: Mode,
    guard: TurnGuard,
    turns: u32,
    ignored: u32,
    last_touch: Instant,
    woke: Option<Instant>,
    background: Option<Background>,
    /// Where the reader was when the layout was first changed since the
    /// last turn. Each change finds the page from here, so stepping a
    /// setting up and back down returns to the same page rather than
    /// creeping back a page at a time.
    anchor: Option<Position>,
}

impl<R> std::fmt::Debug for Session<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("reader", &self.reader)
            .field("mode", &self.mode)
            .field("turns", &self.turns)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek + Send + 'static> Session<R> {
    /// Opens the book where it was left, and shows it.
    pub fn start(
        open: Opener<R>,
        book_key: u64,
        device: &mut dyn Device,
        now: Instant,
    ) -> Result<Self, AppError> {
        let preferences: Preferences = load_json(device, PREFERENCES).unwrap_or_default();
        let light = match preferences.light {
            Some(light) => {
                device.set_light(light);
                light
            }
            None => device.light(),
        };
        let at = load_json(device, &position_file(book_key)).unwrap_or(Position {
            chapter: 0,
            block: 0,
            offset: 0,
        });
        let settings = preferences.settings();
        let (reader, background) = open_reader(&open, book_key, &settings, at, device)?;
        let mut session = Self {
            open,
            book_key,
            reader,
            preferences,
            light,
            ui: Typesetter::new(Settings::default(), Some("en")),
            screen: Canvas::new(settings.width, settings.height),
            mode: Mode::Reading,
            guard: TurnGuard::default(),
            turns: 0,
            ignored: 0,
            last_touch: now,
            woke: None,
            background,
            anchor: None,
        };
        session.show_page(device, Refresh::Full, now)?;
        Ok(session)
    }

    pub fn reader(&self) -> &Reader<R> {
        &self.reader
    }

    /// Turns the guard has dropped so far.
    pub fn ignored_turns(&self) -> u32 {
        self.ignored
    }

    /// When to send [`Event::Tick`] if nothing else happens first.
    pub fn deadline(&self, now: Instant) -> Instant {
        let mut deadline = match self.mode {
            Mode::Asleep { suspend_at } => suspend_at,
            Mode::Reading | Mode::Menu(_) => self.last_touch + AUTO_SLEEP,
        };
        if self.background.is_some() {
            deadline = deadline.min(now + BACKGROUND_CHECK);
        }
        deadline
    }

    pub fn handle(
        &mut self,
        event: Event,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<Flow, AppError> {
        if !matches!(event, Event::Tick) {
            self.last_touch = now;
        }
        let in_menu = matches!(self.mode, Mode::Menu(_));
        let asleep = matches!(self.mode, Mode::Asleep { .. });
        match event {
            Event::Exit => {
                self.save(device);
                return Ok(Flow::Exit);
            }
            Event::Tick => self.tick(now, device)?,
            Event::Tap { .. } | Event::Power if asleep => self.wake(device, now)?,
            _ if asleep => {}
            Event::Power => {
                let just_woke = self
                    .woke
                    .is_some_and(|w| now.duration_since(w) < WAKE_GRACE);
                if !just_woke {
                    self.sleep(device, now)?;
                }
            }
            Event::Tap { x, y } if in_menu => return self.menu_tap(x, y, now, device),
            _ if in_menu => {}
            Event::Tap { x, y } => self.reading_tap(x, y, now, device)?,
            Event::Turn(turn) => self.turn(turn, now, device)?,
            Event::Back => {
                if self.guard.allow(now) && self.reader.back()? {
                    self.moved(device, now)?;
                }
            }
        }
        Ok(Flow::Continue)
    }

    pub fn preferences(&self) -> &Preferences {
        &self.preferences
    }

    /// Changes the preferences from outside the menu, such as from the
    /// emulator's keys, and shows the page again with them at the same place.
    pub fn update_preferences(
        &mut self,
        device: &mut dyn Device,
        now: Instant,
        change: impl FnOnce(&mut Preferences),
    ) -> Result<(), AppError> {
        change(&mut self.preferences);
        self.reopen(device, now)?;
        self.mode = Mode::Reading;
        self.show_page(device, Refresh::Full, now)
    }

    /// Saves the place, for a platform that's about to stop on its own, such
    /// as the emulator's window closing.
    pub fn save(&mut self, device: &mut dyn Device) {
        if let Some(position) = self.reader.position() {
            save_json(device, &position_file(self.book_key), &position);
        }
    }

    fn reading_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let (width, height) = (
            self.reader.settings().width as f32,
            self.reader.settings().height as f32,
        );
        if let Some(target) = self.reader.link_at(x, y) {
            if self.guard.allow(now) {
                self.reader.follow(target)?;
                self.moved(device, now)?;
                self.save(device);
            } else {
                self.ignored += 1;
            }
        } else if y < height * MENU_BAND {
            self.open_menu(device, now)?;
        } else if x < width / 3.0 {
            self.turn(Turn::Previous, now, device)?;
        } else {
            self.turn(Turn::Next, now, device)?;
        }
        Ok(())
    }

    fn turn(&mut self, turn: Turn, now: Instant, device: &mut dyn Device) -> Result<(), AppError> {
        if !self.guard.allow(now) {
            self.ignored += 1;
            return Ok(());
        }
        if self.reader.turn(turn)? {
            self.moved(device, now)?;
            if self.turns.is_multiple_of(SAVE_EVERY) {
                self.save(device);
            }
        }
        Ok(())
    }

    /// Shows the page after a turn or a jump.
    fn moved(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        self.anchor = None;
        self.turns += 1;
        let refresh = if self.turns.is_multiple_of(FULL_EVERY) {
            Refresh::Full
        } else {
            Refresh::Partial
        };
        self.show_page(device, refresh, now)
    }

    /// Puts the current page up, then draws its neighbours while the panel
    /// refreshes, so the next turn either way is ready.
    fn show_page(
        &mut self,
        device: &mut dyn Device,
        refresh: Refresh,
        now: Instant,
    ) -> Result<(), AppError> {
        if let Some(page) = self.reader.page()? {
            device.show(page, refresh);
        } else {
            self.screen.clear();
            device.show(&self.screen, refresh);
        }
        self.guard.refreshing(now + refresh.duration());
        self.reader.prepare()?;
        Ok(())
    }

    fn tick(&mut self, now: Instant, device: &mut dyn Device) -> Result<(), AppError> {
        if let Some(background) = &self.background
            && let Ok(map) = background.receive.try_recv()
        {
            self.reader.adopt(&map);
            let key = PageMap::key(self.book_key, self.reader.settings());
            save_json(device, &pages_file(key), &map);
            self.background = None;
        }
        match self.mode {
            Mode::Asleep { suspend_at } => {
                if now >= suspend_at {
                    if !device.battery().is_some_and(|b| b.charging) && device.suspend() {
                        self.wake(device, Instant::now().max(now))?;
                    } else {
                        self.mode = Mode::Asleep {
                            suspend_at: now + SLEEP_RETRY,
                        };
                    }
                }
            }
            Mode::Reading | Mode::Menu(_) => {
                let idle = now.duration_since(self.last_touch) >= AUTO_SLEEP;
                if idle && !device.battery().is_some_and(|b| b.charging) {
                    self.sleep(device, now)?;
                }
            }
        }
        Ok(())
    }

    fn sleep(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        self.save(device);
        self.compose_page()?;
        let (width, height) = (self.reader.settings().width, self.reader.settings().height);
        let band = 160;
        let top = height - band;
        self.screen.fill(0, top as i32, width, band, 255);
        self.screen.fill(0, top as i32, width, 4, 0);
        let text = "Asleep. Press the power button to wake.";
        let size = 40.0;
        let text_width = self.ui.label_width(text, size, 500.0);
        self.ui.draw_label(
            &mut self.screen,
            text,
            (
                (width as f32 - text_width) / 2.0,
                top as f32 + band as f32 * 0.6,
            ),
            size,
            500.0,
        );
        device.show(&self.screen, Refresh::Full);
        self.mode = Mode::Asleep {
            suspend_at: now + SUSPEND_DELAY,
        };
        Ok(())
    }

    fn wake(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        self.mode = Mode::Reading;
        self.woke = Some(now);
        self.last_touch = now;
        self.show_page(device, Refresh::Full, now)
    }

    /// Copies the current page into `screen`, to draw over.
    fn compose_page(&mut self) -> Result<(), AppError> {
        match self.reader.page()? {
            Some(page) => self.screen.clone_from(page),
            None => self.screen.clear(),
        }
        Ok(())
    }

    fn open_menu(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        let layout = menu::Layout::new(self.reader.settings().width, self.reader.can_go_back());
        self.mode = Mode::Menu(layout);
        self.draw_menu(device, now)
    }

    fn draw_menu(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        let values = self.menu_values(device);
        self.compose_page()?;
        if let Mode::Menu(layout) = &self.mode {
            layout.draw(&mut self.screen, &mut self.ui, &values);
        }
        device.show(&self.screen, Refresh::Partial);
        self.guard.refreshing(now + Refresh::Partial.duration());
        Ok(())
    }

    fn menu_values(&self, device: &mut dyn Device) -> menu::Values {
        let p = &self.preferences;
        menu::Values {
            title: self.reader.manifest().title.clone(),
            battery: match device.battery() {
                Some(Battery {
                    percent,
                    charging: true,
                }) => format!("{percent}%, charging"),
                Some(Battery { percent, .. }) => format!("{percent}%"),
                None => String::new(),
            },
            light: format!("{}%", self.light.brightness),
            warmth: self.light.warmth.to_string(),
            text_size: format!("{}", p.font_size),
            darkness: format!("{}", p.weight),
            contrast: format!("{:.1}", p.gamma),
            can_return: self.reader.can_go_back(),
        }
    }

    fn menu_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<Flow, AppError> {
        let Mode::Menu(layout) = &self.mode else {
            return Ok(Flow::Continue);
        };
        let below = y >= layout.bottom();
        let action = layout.hit(x, y);
        match action {
            None if below => self.close_menu(device, now)?,
            None => {}
            Some(menu::Action::Close) => self.close_menu(device, now)?,
            Some(menu::Action::Exit) => {
                self.save(device);
                return Ok(Flow::Exit);
            }
            Some(menu::Action::Return) => {
                self.reader.back()?;
                self.close_menu(device, now)?;
            }
            Some(menu::Action::Light(step)) => {
                self.light.brightness = stepped(self.light.brightness, step, 10, 100);
                self.light_changed(device, now)?;
            }
            Some(menu::Action::Warmth(step)) => {
                self.light.warmth = stepped(self.light.warmth, step, 1, 10);
                self.light_changed(device, now)?;
            }
            Some(menu::Action::TextSize(step)) => {
                let p = &mut self.preferences;
                p.font_size = (p.font_size + signed(step, 4.0)).clamp(24.0, 80.0);
                self.reopen(device, now)?;
            }
            Some(menu::Action::Darkness(step)) => {
                let p = &mut self.preferences;
                p.weight = (p.weight + signed(step, 50.0)).clamp(300.0, 700.0);
                self.reopen(device, now)?;
            }
            Some(menu::Action::Contrast(step)) => {
                let p = &mut self.preferences;
                // Kept to tenths, so repeated steps don't drift.
                p.gamma = ((p.gamma + signed(step, 0.2)).clamp(0.6, 3.0) * 10.0).round() / 10.0;
                self.reopen(device, now)?;
            }
        }
        Ok(Flow::Continue)
    }

    fn close_menu(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        self.mode = Mode::Reading;
        self.show_page(device, Refresh::Full, now)
    }

    fn light_changed(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        device.set_light(self.light);
        self.preferences.light = Some(self.light);
        save_json(device, PREFERENCES, &self.preferences);
        self.draw_menu(device, now)
    }

    /// Opens the book again with changed preferences, at the same place.
    fn reopen(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        save_json(device, PREFERENCES, &self.preferences);
        let at = self
            .anchor
            .or_else(|| self.reader.position())
            .unwrap_or(Position {
                chapter: 0,
                block: 0,
                offset: 0,
            });
        self.anchor = Some(at);
        self.background = None;
        let settings = self.preferences.settings();
        let (reader, background) = open_reader(&self.open, self.book_key, &settings, at, device)?;
        self.reader = reader;
        self.background = background;
        if matches!(self.mode, Mode::Menu(_)) {
            self.draw_menu(device, now)?;
        }
        Ok(())
    }
}

/// Opens the reader with its saved page map, and starts paginating the rest
/// of the book in the background if the map isn't complete.
fn open_reader<R: Read + Seek + Send + 'static>(
    open: &Opener<R>,
    book_key: u64,
    settings: &Settings,
    at: Position,
    device: &mut dyn Device,
) -> Result<(Reader<R>, Option<Background>), AppError> {
    let key = PageMap::key(book_key, settings);
    let cached: Option<PageMap> = load_json(device, &pages_file(key));
    let reader = Reader::open(open()?, settings.clone(), book_key, at, cached)?;
    if reader.page_map().is_complete() {
        return Ok((reader, None));
    }
    let (send, receive) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let mut map = reader.page_map().clone();
    let thread_open = Arc::clone(open);
    let thread_stop = Arc::clone(&stop);
    let thread_settings = settings.clone();
    std::thread::spawn(move || {
        let Ok(mut book) = thread_open() else {
            return;
        };
        let result = paginate_book(&mut book, thread_settings, &mut map, || {
            thread_stop.load(Ordering::Relaxed)
        });
        if result.is_ok() && map.is_complete() {
            let _ = send.send(map);
        }
    });
    Ok((reader, Some(Background { receive, stop })))
}

fn stepped(value: u8, step: menu::Step, by: u8, max: u8) -> u8 {
    match step {
        menu::Step::Down => value.saturating_sub(by),
        menu::Step::Up => value.saturating_add(by).min(max),
    }
}

fn signed(step: menu::Step, by: f32) -> f32 {
    match step {
        menu::Step::Down => -by,
        menu::Step::Up => by,
    }
}

fn load_json<T: serde::de::DeserializeOwned>(device: &mut dyn Device, name: &str) -> Option<T> {
    serde_json::from_slice(&device.load(name)?).ok()
}

fn save_json<T: Serialize>(device: &mut dyn Device, name: &str, value: &T) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        device.save(name, &bytes);
    }
}
