//! The reader app without any device code: what a tap, the power button or
//! the passing of time does, and what goes on the screen.
//!
//! It starts on the home screen ([`home`]), with the books the platform
//! lists through [`Books`], kept in a [`Library`]. The home screen leads to
//! the library screen ([`shelf`]), every book sorted a screen at a time,
//! and from there to [`search`], with an on-screen keyboard. The home
//! screen also leads to [`settings`].
//!
//! A platform (the desktop emulator, or the Kobo) turns its input into
//! [`Event`]s for [`Session::handle`], and does what the session asks
//! through [`Device`]. Between events it waits until
//! [`Session::deadline`] and then sends [`Event::Tick`].
//!
//! - **Turning:** a tap in the left third goes back a page, one in the
//!   right third forward, and a tap on a link follows it. The [`TurnGuard`] drops turns
//!   that come too soon. Turns use the grey partial refresh, with a full one
//!   every [`FULL_EVERY`] turns to clear ghosting.
//! - **The menu** opens with a tap in the middle third. It has the
//!   front light, warmth, text size, darkness and contrast, a way back from
//!   a link, and [`settings`], which return to the book. The way out to
//!   Nickel is on the power screen ([`power_page`]), held power button.
//! - **Looking up:** a finger resting on a word ([`Event::Hold`]) shows
//!   its Swedish translations ([`lookup`]), from the dictionary file
//!   [`DICTIONARY`], read the first time it's needed. A tap closes it.
//! - **Sleep:** the power button, or [`AUTO_SLEEP`] without a touch, saves
//!   the place, puts the cover of the book being read on the screen (or a
//!   notice, without one) and suspends, except while charging, which can hang the
//!   Clara BW's kernel.
//! - **Saved:** the place in each book, the preferences, and each book's
//!   page map once it's complete, through [`Device::save`].
//! - **Sync:** with [`Session::enable_sync`], books come from the server and
//!   places go both ways, on another thread ([`sync`]). It syncs on
//!   starting, on waking once [`SYNC_EVERY`] has passed, and when asked on
//!   the sync screen ([`sync_page`]), from settings. Going to sleep sends
//!   any new places first, and nothing else.
#![forbid(unsafe_code)]
#![allow(clippy::cast_precision_loss, reason = "pixel sizes")]

mod book;
mod folder;
mod home;
mod kobo_page;
mod library;
mod lookup;
mod menu;
mod place;
mod power_page;
mod search;
mod settings;
mod shelf;
mod sync;
mod sync_page;
mod ui;
mod usb_page;

use std::{
    collections::{BTreeMap, HashMap},
    io::{self, Read, Seek},
    sync::Arc,
    time::{Duration, Instant},
};

use book_format::{BookReader, FormatError, Position};
use dictionary::Dictionary;
pub use reader_core::Turn;
use reader_core::{Canvas, Picture, Reader, ReaderError, Settings, TurnGuard, Typesetter};
use serde::{Deserialize, Serialize};

pub use folder::Folder;
pub use library::{Entry, Library};
use place::SavedPlace;
use reader_sync::{Report, Spot, Status};
pub use shelf::Sort;
pub use sync::{AlwaysOnline, Network, Ssh, SshLogin, SyncSetup};

/// A full refresh every this many turns clears ghosting.
pub const FULL_EVERY: u32 = 6;
/// Sleep after this long without a touch, unless charging.
pub const AUTO_SLEEP: Duration = Duration::from_mins(10);
/// The power button is ignored this long after waking, since the press that
/// woke the device can arrive again once it's awake.
const WAKE_GRACE: Duration = Duration::from_secs(2);
/// How long the power screen waits for a tap before going to Nickel.
const POWER_WAIT: Duration = Duration::from_secs(15);
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
/// How long a finger rests before it's a [`Event::Hold`].
pub const LONG_PRESS: Duration = Duration::from_millis(500);
/// The longest the places from before sleeping may take to send before
/// it suspends anyway; Wi-Fi alone can take most of a minute.
pub const SLEEP_SEND_LONGEST: Duration = Duration::from_secs(60);
/// On waking, it syncs if it hasn't for this long.
pub const SYNC_EVERY: Duration = Duration::from_mins(30);
/// While a sync runs, the session looks for news this often.
const SYNC_CHECK: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error(transparent)]
    Reader(#[from] ReaderError),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// A touch, at the place the finger came down, in screen pixels; sent
    /// when it lifts.
    Tap {
        x: f32,
        y: f32,
    },
    /// A finger resting [`LONG_PRESS`] where it came down, sent while it's
    /// still there: looks up the word under it while reading. Elsewhere
    /// it's a [`Event::Tap`]. The lift that follows sends nothing.
    Hold {
        x: f32,
        y: f32,
    },
    /// A page turn from a key, in the emulator.
    Turn(Turn),
    /// Return from a link, from a key in the emulator.
    Back,
    Power,
    /// The power button has been held down: asks whether to power off,
    /// restart or go to Nickel, and goes to Nickel if nothing is tapped.
    PowerHeld,
    /// Leave for Nickel at once.
    Exit,
    /// [`Session::deadline`] has passed.
    Tick,
    /// The device was plugged into a computer (true), or unplugged from
    /// it (false). A charger doesn't count.
    Computer(bool),
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
    /// The English–Swedish dictionary file, if there is one: [`DICTIONARY`]
    /// beside the saved files, unless the platform keeps it elsewhere.
    fn dictionary(&mut self) -> Option<Vec<u8>> {
        self.load(DICTIONARY)
    }
    fn save(&mut self, name: &str, bytes: &[u8]);
    /// The time, in seconds since 1970, for when books were last opened.
    fn clock(&mut self) -> u64;
    /// How far ahead of UTC local time is at `clock`, in seconds, for the
    /// time in the status. UTC where the platform can't tell.
    fn utc_offset(&mut self, clock: u64) -> i32 {
        let _ = clock;
        0
    }
    /// Whether switching the device on starts the reader: `None` where
    /// that can't change, as in the emulator.
    fn starts_reader(&mut self) -> Option<bool> {
        None
    }
    /// Makes switching on start the reader, or not. Returns what to say on
    /// the screen, or why it couldn't.
    fn set_starts_reader(&mut self, on: bool) -> Result<String, String> {
        let _ = on;
        Err("That can't change here.".to_owned())
    }
}

/// A book file where the platform keeps books.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookFile {
    pub name: String,
    pub size: u64,
    /// In seconds since 1970.
    pub modified: u64,
}

/// Where the books are: a folder on the Kobo and in the emulator.
pub trait Books<R>: Send + Sync {
    fn list(&self) -> Vec<BookFile>;
    /// The whole file, to index it.
    fn read(&self, name: &str) -> io::Result<Vec<u8>>;
    /// Opens a book to read, as often as needed: the reader and the
    /// background pagination each open their own.
    fn open(&self, name: &str) -> Result<BookReader<R>, FormatError>;
}

/// A book's key: 64-bit FNV-1a of the whole file, so a changed file is a
/// new book, with its own saved place and page maps.
pub fn book_key(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Whether to keep running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Back to Nickel.
    Exit,
    /// Switch the device off. The place is saved, and the panel says so.
    PowerOff,
    /// Restart the device. The place is saved, and the panel says so.
    Restart,
    /// Back to Nickel for USB, which the platform leaves again for the
    /// reader once the computer is unplugged. The place is saved.
    Usb,
    /// Back to Nickel for good: the platform uninstalls the reader. The
    /// place is saved.
    Uninstall,
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
    /// How the library screen is sorted.
    pub library_sort: Sort,
    /// Line height as a multiple of the text size.
    pub line_height: f32,
    /// The page's side margins, in pixels.
    pub margin: u32,
    /// Turns sooner than this after the last one are dropped; 0 for none.
    pub turn_guard_ms: u32,
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
            library_sort: Sort::default(),
            line_height: settings.line_height,
            margin: settings.margin_side,
            turn_guard_ms: 300,
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
            line_height: self.line_height,
            margin_side: self.margin,
            ..Settings::default()
        }
    }

    fn turn_guard(&self) -> TurnGuard {
        TurnGuard::new(Duration::from_millis(u64::from(self.turn_guard_ms)))
    }
}

const PREFERENCES: &str = "preferences.json";
/// Covers kept in memory, for redrawing the home screen without reading them
/// again.
const COVERS_KEPT: usize = 24;

#[derive(Debug)]
enum Mode {
    Home(home::Layout),
    Library(shelf::Layout),
    Search(search::Layout),
    Settings(settings::Layout),
    Sync(sync_page::Layout),
    /// Plugged into a computer: offering to hand over to Nickel.
    Usb {
        layout: usb_page::Layout,
        back: Back,
    },
    /// From settings: starting when switched on, and uninstalling.
    Kobo(kobo_page::Layout),
    /// The power button was held: power off, restart or Nickel.
    Power {
        layout: power_page::Layout,
        back: Back,
        /// When it goes to Nickel, with nothing tapped.
        nickel_at: Instant,
    },
    Reading,
    /// A word looked up, over the page.
    Lookup {
        /// The word as printed.
        word: String,
    },
    Menu(menu::Layout),
    /// On the sleep screen, waiting to suspend, or awake again after a
    /// failed try.
    Asleep {
        /// When to try to suspend next.
        suspend_at: Instant,
        /// Sending the places read before sleeping: how long to wait for
        /// it before stopping it to suspend.
        sending_until: Option<Instant>,
        back: Back,
    },
}

/// The screen to go back to after sleeping.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Back {
    Home,
    /// The library, at this page.
    Library(usize),
    /// Search, with this typed, going back to this library page.
    Search(String, usize),
    Settings,
    Sync,
    /// The page of the open book, without the menu.
    Reading,
}

pub struct Session<R> {
    books: Arc<dyn Books<R>>,
    library: Library,
    /// Kept while on the home screen, so going back to it is instant.
    book: Option<book::OpenBook<R>>,
    covers: HashMap<u64, Option<Picture>>,
    /// The settings screen's sample, and the settings it was laid out with.
    preview: Option<(Settings, Option<Canvas>)>,
    preferences: Preferences,
    light: Light,
    /// For the menu, the home screen and the sleep screen.
    ui: Typesetter,
    /// Whatever isn't a plain page: the home screen, or the page with the
    /// menu or the sleep notice drawn over it.
    screen: Canvas,
    mode: Mode,
    guard: TurnGuard,
    turns: u32,
    ignored: u32,
    last_touch: Instant,
    woke: Option<Instant>,
    /// Set by [`Session::enable_sync`].
    syncer: Option<sync::Syncer>,
    /// Plugged into a computer, from [`Event::Computer`].
    computer: bool,
    /// Not now was tapped since it was plugged in, so don't ask again.
    usb_declined: bool,
    /// Where settings' Done goes: home, or the book its menu was opened from.
    settings_from: Back,
    /// When the status shown should next change: at the next minute.
    status_due: Option<Instant>,
    /// Whether the status shown says Wi-Fi is up.
    status_wifi: bool,
    /// Read the first time a word is looked up.
    dictionary: Option<Dictionary>,
    /// The cover slept on last, by book key, shrunk to the screen: kept,
    /// so sleeping again doesn't decode it again. `None` for a book
    /// without one.
    sleep_cover: Option<(u64, Option<Picture>)>,
}

impl<R> std::fmt::Debug for Session<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("book", &self.book)
            .field("mode", &self.mode)
            .field("turns", &self.turns)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek + Send + 'static> Session<R> {
    /// Brings the library up to date and shows the home screen.
    pub fn start(
        books: Arc<dyn Books<R>>,
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
        let mut library = Library::load(device);
        if library.refresh(books.as_ref(), device) {
            library.save(device);
        }
        let settings = preferences.settings();
        let mut session = Self {
            books,
            library,
            book: None,
            covers: HashMap::new(),
            preview: None,
            guard: preferences.turn_guard(),
            preferences,
            light,
            ui: Typesetter::new(Settings::default(), Some("en")),
            screen: Canvas::new(settings.width, settings.height),
            mode: Mode::Reading,
            turns: 0,
            ignored: 0,
            last_touch: now,
            woke: None,
            syncer: None,
            computer: false,
            usb_declined: false,
            settings_from: Back::Home,
            status_due: None,
            status_wifi: false,
            dictionary: None,
            sleep_cover: None,
        };
        session.go_home(device, Refresh::Full, now);
        Ok(session)
    }

    pub fn library(&self) -> &Library {
        &self.library
    }

    /// The open book's reader, if a book has been opened.
    pub fn reader(&self) -> Option<&Reader<R>> {
        self.book.as_ref().map(|book| &book.reader)
    }

    /// Whether the home screen is up.
    pub fn at_home(&self) -> bool {
        matches!(self.mode, Mode::Home(_))
    }

    /// The word looked up, while its translations are up.
    pub fn looking_up(&self) -> Option<&str> {
        match &self.mode {
            Mode::Lookup { word } => Some(word),
            _ => None,
        }
    }

    /// Whether the book's menu is up.
    pub fn in_menu(&self) -> bool {
        matches!(self.mode, Mode::Menu(_))
    }

    /// Whether the settings screen is up.
    pub fn in_settings(&self) -> bool {
        matches!(self.mode, Mode::Settings(_))
    }

    /// Whether the screen offering to connect over USB is up.
    pub fn in_usb(&self) -> bool {
        matches!(self.mode, Mode::Usb { .. })
    }

    /// Whether the power screen is up, after the power button was held.
    pub fn in_power(&self) -> bool {
        matches!(self.mode, Mode::Power { .. })
    }

    /// Whether the sync screen is up.
    pub fn in_sync(&self) -> bool {
        matches!(self.mode, Mode::Sync(_))
    }

    /// Whether a sync or a pairing is under way.
    pub fn syncing(&self) -> bool {
        self.syncer.as_ref().is_some_and(sync::Syncer::busy)
    }

    /// The code being shown to pair with, if there is one.
    pub fn pairing_code(&self) -> Option<&str> {
        match self.syncer.as_ref()?.activity.as_ref()? {
            sync::Activity::Pairing {
                code: Some((code, _)),
            } => Some(code),
            _ => None,
        }
    }

    /// The command to log in with, while SSH is on.
    pub fn ssh_login(&self) -> Option<String> {
        match self.syncer.as_ref()?.ssh()? {
            sync::SshState::On(login) => Some(login.command),
            sync::SshState::Off { .. } => None,
        }
    }

    /// How the last sync went, as the sync screen says it.
    pub fn last_sync(&self) -> Option<&str> {
        let last = self.syncer.as_ref()?.status.last.as_ref()?;
        Some(&last.text)
    }

    /// When the last sync or pairing ended, in seconds since 1970.
    pub fn last_sync_at(&self) -> Option<u64> {
        Some(self.syncer.as_ref()?.status.last.as_ref()?.at)
    }

    /// Turns on syncing with a server, and syncs if already paired.
    pub fn enable_sync(&mut self, setup: SyncSetup, device: &mut dyn Device, now: Instant) {
        let status = load_json(device, sync::STATUS).unwrap_or_default();
        let syncer = sync::Syncer::new(setup, status);
        if self.library.apply_sync(&syncer.state()) {
            self.library.save(device);
        }
        self.syncer = Some(syncer);
        self.start_sync(device, now);
    }

    /// Starts a sync, unless one's running or it isn't paired.
    fn start_sync(&mut self, device: &mut dyn Device, now: Instant) {
        let local = self.local_places(device);
        if let Some(syncer) = self.syncer.as_mut()
            && syncer.paired()
        {
            syncer.sync(local, now);
        }
    }

    /// Where the reader is in each synced book it's opened, to send.
    fn local_places(&mut self, device: &mut dyn Device) -> BTreeMap<String, Spot> {
        self.save(device);
        let mut places = BTreeMap::new();
        for entry in self.library.books() {
            let Some(id) = &entry.id else {
                continue;
            };
            let Some(saved) =
                load_json::<SavedPlace>(device, &book::position_file(entry.place_key()))
            else {
                continue;
            };
            let Some(revision) = saved.revision.or(entry.revision) else {
                continue;
            };
            if saved.at == 0 {
                continue;
            }
            let status = if saved.progress >= 0.999 {
                Status::Finished
            } else {
                Status::Reading
            };
            places.insert(
                id.clone(),
                Spot {
                    chapter: saved.position.chapter,
                    block: saved.position.block,
                    offset: saved.position.offset,
                    revision,
                    percent: saved.progress * 100.0,
                    status,
                    at: saved.at as i64,
                },
            );
        }
        places
    }

    /// Takes in what the sync thread has said, and shows it.
    fn sync_news(&mut self, device: &mut dyn Device, now: Instant) {
        let Some(syncer) = self.syncer.as_mut() else {
            return;
        };
        let updates = syncer.updates();
        if updates.is_empty() {
            return;
        }
        let mut library_changed = false;
        let mut ended = false;
        for update in updates {
            match update {
                sync::Update::Synced(report) => {
                    library_changed |= self.synced(&report, device);
                    self.ended(true, sync::summary(&report), device);
                }
                sync::Update::Failed(why) => self.ended(false, why, device),
                sync::Update::Unpaired => self.ended(
                    false,
                    "This reader was removed on the website. Pair it again.".to_owned(),
                    device,
                ),
                sync::Update::Done => ended = true,
                sync::Update::Connecting
                | sync::Update::Progress(_)
                | sync::Update::Code { .. }
                | sync::Update::Paired { .. }
                | sync::Update::SshOn(_)
                | sync::Update::SshFailed(_) => {}
            }
        }
        if ended && let Some(syncer) = &self.syncer {
            save_json(device, sync::STATUS, &syncer.status);
        }
        match &self.mode {
            Mode::Sync(_) => self.show_sync(device, Refresh::Partial, now),
            Mode::Home(_) if library_changed => self.go_home(device, Refresh::Partial, now),
            Mode::Library(layout) if library_changed => {
                let page = layout.page();
                self.show_library(page, device, Refresh::Partial, now);
            }
            _ => {}
        }
    }

    fn ended(&mut self, worked: bool, text: String, device: &mut dyn Device) {
        let at = device.clock();
        if let Some(syncer) = self.syncer.as_mut() {
            syncer.status.last = Some(sync::Last { at, worked, text });
        }
    }

    /// Brings the library up to date after a sync, and moves to places
    /// read elsewhere. Returns whether the library changed.
    fn synced(&mut self, report: &Report, device: &mut dyn Device) -> bool {
        let Some(syncer) = &self.syncer else {
            return false;
        };
        let state = syncer.state();
        let mut changed = self.library.refresh(self.books.as_ref(), device);
        changed |= self.library.apply_sync(&state);
        let showing = matches!(
            self.mode,
            Mode::Reading
                | Mode::Menu(_)
                | Mode::Asleep {
                    back: Back::Reading,
                    ..
                }
        );
        for (id, spot) in &report.places {
            let Some(entry) = self.library.by_id(id).cloned() else {
                continue;
            };
            let open = self
                .book
                .as_ref()
                .is_some_and(|book| book.file == entry.file);
            if open && showing {
                // Being read here, so here wins; it's sent next time.
                continue;
            }
            let place = SavedPlace {
                position: Position {
                    chapter: spot.chapter,
                    block: spot.block,
                    offset: spot.offset,
                },
                file: 0,
                revision: Some(spot.revision),
                progress: (spot.percent / 100.0).clamp(0.0, 1.0),
                at: u64::try_from(spot.at).unwrap_or(0),
            };
            save_json(device, &book::position_file(entry.place_key()), &place);
            self.library.set_progress(&entry.file, place.progress);
            if open {
                // Opened again from the new place. Not saved first, which
                // would put the old place back.
                self.book = None;
            }
            changed = true;
        }
        // A book that's gone, or whose file was replaced, is opened afresh.
        if !showing
            && let Some(book) = &self.book
            && self
                .library
                .get(&book.file)
                .is_none_or(|e| e.key != book.key)
        {
            self.book = None;
        }
        if changed {
            self.library.save(device);
        }
        changed
    }

    /// What the sync screen is about now.
    fn sync_view(&self) -> sync_page::View {
        let Some(syncer) = &self.syncer else {
            return sync_page::View::NoServer;
        };
        let Some(server) = syncer.setup.server.clone() else {
            return sync_page::View::NoServer;
        };
        if let Some(activity) = &syncer.activity {
            return sync_page::View::Busy(activity.clone());
        }
        if syncer.paired() {
            sync_page::View::Paired {
                server,
                user: syncer.status.user.clone(),
            }
        } else {
            sync_page::View::Unpaired { server }
        }
    }

    fn show_sync(&mut self, device: &mut dyn Device, refresh: Refresh, now: Instant) {
        let view = self.sync_view();
        let confirming = match &self.mode {
            Mode::Sync(layout) => layout.confirming && layout.view == view,
            _ => false,
        };
        let settings = Settings::default();
        let ssh = self.syncer.as_ref().and_then(sync::Syncer::ssh);
        let layout = sync_page::Layout::new(settings.width, settings.height, view, confirming, ssh);
        let status = self.status_text(device, now);
        let clock = device.clock();
        let last = self
            .syncer
            .as_ref()
            .and_then(|syncer| syncer.status.last.as_ref())
            .map(|last| last.shown(clock));
        layout.draw(&mut self.screen, &mut self.ui, last.as_deref(), &status);
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        self.mode = Mode::Sync(layout);
    }

    fn sync_tap(&mut self, x: f32, y: f32, now: Instant, device: &mut dyn Device) {
        let Mode::Sync(layout) = &mut self.mode else {
            return;
        };
        let action = layout.hit(x, y);
        let confirming = layout.confirming;
        layout.confirming = false;
        match action {
            Some(sync_page::Action::Back) => {
                self.show_settings(device, Refresh::Full, now);
                return;
            }
            Some(sync_page::Action::Pair) => {
                if let Some(syncer) = self.syncer.as_mut() {
                    syncer.pair(now);
                }
            }
            Some(sync_page::Action::SyncNow) => self.start_sync(device, now),
            Some(sync_page::Action::Stop) => {
                if let Some(syncer) = &self.syncer {
                    syncer.stop();
                }
            }
            Some(sync_page::Action::Unpair) if confirming => {
                if let Some(syncer) = self.syncer.as_mut() {
                    syncer.unpair();
                    save_json(device, sync::STATUS, &syncer.status);
                }
            }
            Some(sync_page::Action::Unpair) => layout.confirming = true,
            Some(sync_page::Action::Ssh) => {
                if let Some(syncer) = self.syncer.as_mut() {
                    if syncer.ssh_login().is_some() {
                        syncer.ssh_stop();
                    } else {
                        syncer.ssh_start();
                    }
                }
            }
            None => return,
        }
        self.show_sync(device, Refresh::Partial, now);
    }

    /// Whether the library screen is up.
    pub fn in_library(&self) -> bool {
        matches!(self.mode, Mode::Library(_))
    }

    /// What's typed, if the search screen is up.
    pub fn search_query(&self) -> Option<&str> {
        match &self.mode {
            Mode::Search(layout) => Some(layout.query()),
            _ => None,
        }
    }

    /// Turns the guard has dropped so far.
    pub fn ignored_turns(&self) -> u32 {
        self.ignored
    }

    /// When to send [`Event::Tick`] if nothing else happens first.
    pub fn deadline(&self, now: Instant) -> Instant {
        let mut deadline = match self.mode {
            Mode::Asleep { suspend_at, .. } => suspend_at,
            Mode::Power { nickel_at, .. } => nickel_at,
            Mode::Home(_)
            | Mode::Library(_)
            | Mode::Search(_)
            | Mode::Settings(_)
            | Mode::Kobo(_)
            | Mode::Sync(_)
            | Mode::Usb { .. }
            | Mode::Reading
            | Mode::Lookup { .. }
            | Mode::Menu(_) => self.last_touch + AUTO_SLEEP,
        };
        if self.book.as_ref().is_some_and(book::OpenBook::paginating) {
            deadline = deadline.min(now + BACKGROUND_CHECK);
        }
        if self.syncing() {
            deadline = deadline.min(now + SYNC_CHECK);
        }
        if let Some(due) = self.status_due
            && self.status_band().is_some()
        {
            deadline = deadline.min(due);
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
        // Only a page has words to look up.
        let event = match event {
            Event::Hold { x, y } if !matches!(self.mode, Mode::Reading) => Event::Tap { x, y },
            event => event,
        };
        let at_home = matches!(self.mode, Mode::Home(_));
        let in_library = matches!(self.mode, Mode::Library(_));
        let in_search = matches!(self.mode, Mode::Search(_));
        let in_settings = matches!(self.mode, Mode::Settings(_));
        let in_kobo = matches!(self.mode, Mode::Kobo(_));
        let in_sync = matches!(self.mode, Mode::Sync(_));
        let in_usb = matches!(self.mode, Mode::Usb { .. });
        let power_until = match self.mode {
            Mode::Power { nickel_at, .. } => Some(nickel_at),
            _ => None,
        };
        let in_menu = matches!(self.mode, Mode::Menu(_));
        let in_lookup = matches!(self.mode, Mode::Lookup { .. });
        let asleep = matches!(self.mode, Mode::Asleep { .. });
        match event {
            Event::Exit => {
                self.save(device);
                return Ok(Flow::Exit);
            }
            Event::Tick if power_until.is_some_and(|at| now >= at) => {
                // Nothing tapped: maybe the touch panel isn't working.
                self.save(device);
                return Ok(Flow::Exit);
            }
            Event::PowerHeld => self.offer_power(device, now),
            Event::Tap { x, y } if power_until.is_some() => {
                return self.power_tap(x, y, now, device);
            }
            Event::Power | Event::Turn(_) | Event::Back if power_until.is_some() => {}
            Event::Tick => self.tick(now, device)?,
            Event::Computer(plugged) => self.computer(plugged, device, now)?,
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
            Event::Tap { x, y } if at_home => return self.home_tap(x, y, now, device),
            Event::Tap { x, y } if in_menu => return self.menu_tap(x, y, now, device),
            Event::Tap { x, y } if in_library => self.library_tap(x, y, now, device)?,
            Event::Turn(turn) if in_library => self.library_page(turn, now, device),
            Event::Tap { x, y } if in_search => self.search_tap(x, y, now, device)?,
            Event::Tap { x, y } if in_settings => self.settings_tap(x, y, now, device)?,
            Event::Tap { x, y } if in_kobo => return Ok(self.kobo_tap(x, y, now, device)),
            Event::Tap { x, y } if in_sync => self.sync_tap(x, y, now, device),
            Event::Tap { x, y } if in_usb => return self.usb_tap(x, y, now, device),
            Event::Tap { .. } | Event::Hold { .. } | Event::Back if in_lookup => {
                self.mode = Mode::Reading;
                self.show_page(device, Refresh::Full, now)?;
            }
            Event::Turn(turn) if in_lookup => {
                self.mode = Mode::Reading;
                let turns = self.turns;
                self.turn(turn, now, device)?;
                if self.turns == turns {
                    self.show_page(device, Refresh::Full, now)?;
                }
            }
            _ if at_home
                || in_library
                || in_search
                || in_settings
                || in_kobo
                || in_sync
                || in_usb
                || in_menu => {}
            Event::Tap { x, y } => self.reading_tap(x, y, now, device)?,
            Event::Hold { x, y } => self.look_up(x, y, now, device)?,
            Event::Turn(turn) => self.turn(turn, now, device)?,
            Event::Back => {
                let went_back = match self.book.as_mut() {
                    Some(book) => self.guard.allow(now) && book.reader.back()?,
                    None => false,
                };
                if went_back {
                    self.moved(device, now)?;
                }
            }
        }
        Ok(Flow::Continue)
    }

    pub fn preferences(&self) -> &Preferences {
        &self.preferences
    }

    /// Opens the book in `file`, from the library, where it was left.
    pub fn open(
        &mut self,
        file: &str,
        device: &mut dyn Device,
        now: Instant,
    ) -> Result<(), AppError> {
        let Some(entry) = self.library.get(file).cloned() else {
            return Ok(());
        };
        let settings = self.preferences.settings();
        match self.book.as_mut() {
            Some(book) if book.file == file => {
                // The settings screen changed how pages look since.
                if *book.reader.settings() != settings {
                    book.relayout(&settings, device)?;
                }
            }
            _ => {
                self.leave_book(device);
                self.book = Some(book::OpenBook::open(
                    &self.books,
                    &entry,
                    &settings,
                    device,
                )?);
            }
        }
        let when = device.clock();
        self.library.opened(file, when);
        self.library.save(device);
        self.mode = Mode::Reading;
        self.show_page(device, Refresh::Full, now)
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
        self.relayout(device, now)?;
        if self.book.is_some() && !self.at_home() {
            self.mode = Mode::Reading;
            self.show_page(device, Refresh::Full, now)?;
        }
        Ok(())
    }

    /// Saves the place and the library, for a platform that's about to stop
    /// on its own, such as the emulator's window closing.
    pub fn save(&mut self, device: &mut dyn Device) {
        if let Some(book) = &self.book {
            book.save(device);
            if let Some(progress) = book.progress() {
                self.library.set_progress(&book.file, progress);
            }
        }
        self.library.save(device);
    }

    /// Saves the open book's place and closes it.
    fn leave_book(&mut self, device: &mut dyn Device) {
        self.save(device);
        self.book = None;
    }

    fn home_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<Flow, AppError> {
        let Mode::Home(layout) = &self.mode else {
            return Ok(Flow::Continue);
        };
        match layout.hit(x, y) {
            Some(home::Action::Open(file)) => self.open(&file, device, now)?,
            Some(home::Action::Library) => self.show_library(0, device, Refresh::Full, now),
            Some(home::Action::Settings) => {
                self.settings_from = Back::Home;
                self.show_settings(device, Refresh::Full, now);
            }
            None => {}
        }
        Ok(Flow::Continue)
    }

    /// Shows the home screen, saving the open book's place first.
    fn go_home(&mut self, device: &mut dyn Device, refresh: Refresh, now: Instant) {
        self.save(device);
        let settings = Settings::default();
        let layout = home::Layout::new(settings.width, settings.height, &self.library);
        self.draw_home(&layout, device, now);
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        self.mode = Mode::Home(layout);
    }

    /// Shows page `page` of the library, sorted as last chosen.
    fn show_library(
        &mut self,
        page: usize,
        device: &mut dyn Device,
        refresh: Refresh,
        now: Instant,
    ) {
        let settings = Settings::default();
        let sort = self.preferences.library_sort;
        let layout = shelf::Layout::new(settings.width, settings.height, &self.library, sort, page);
        let status = self.status_text(device, now);
        layout.draw(&mut self.screen, &mut self.ui, &self.library, &status);
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        self.mode = Mode::Library(layout);
    }

    fn library_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let Mode::Library(layout) = &self.mode else {
            return Ok(());
        };
        match layout.hit(x, y) {
            Some(shelf::Action::Open(file)) => self.open(&file, device, now)?,
            Some(shelf::Action::Sort(sort)) => {
                if sort != layout.sort() {
                    self.preferences.library_sort = sort;
                    save_json(device, PREFERENCES, &self.preferences);
                    self.show_library(0, device, Refresh::Partial, now);
                }
            }
            Some(shelf::Action::Page(turn)) => self.library_page(turn, now, device),
            Some(shelf::Action::Search) => {
                let page = layout.page();
                self.show_search(String::new(), page, device, Refresh::Full, now);
            }
            Some(shelf::Action::Home) => self.go_home(device, Refresh::Full, now),
            None => {}
        }
        Ok(())
    }

    /// The library's next or previous page, if there is one. Dropped while
    /// the last page is still being drawn, like a turn.
    fn library_page(&mut self, turn: Turn, now: Instant, device: &mut dyn Device) {
        let Mode::Library(layout) = &self.mode else {
            return;
        };
        let page = match turn {
            Turn::Next => layout.page() + 1,
            Turn::Previous => layout.page().saturating_sub(1),
        };
        if page == layout.page() || page >= layout.pages() || !self.guard.allow(now) {
            return;
        }
        self.show_library(page, device, Refresh::Partial, now);
    }

    /// Shows the search screen with `query` typed, to go back to library
    /// page `page`.
    fn show_search(
        &mut self,
        query: String,
        page: usize,
        device: &mut dyn Device,
        refresh: Refresh,
        now: Instant,
    ) {
        let settings = Settings::default();
        let layout =
            search::Layout::new(settings.width, settings.height, &self.library, query, page);
        let status = self.status_text(device, now);
        layout.draw(&mut self.screen, &mut self.ui, &self.library, &status);
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        self.mode = Mode::Search(layout);
    }

    /// A key or a result. Keys aren't held back by the turn guard, so
    /// quick typing isn't lost.
    fn search_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let Mode::Search(layout) = &self.mode else {
            return Ok(());
        };
        let page = layout.library_page();
        match layout.hit(x, y) {
            Some(search::Action::Open(file)) => self.open(&file, device, now)?,
            Some(search::Action::Back) => self.show_library(page, device, Refresh::Full, now),
            Some(action) => {
                if let Some(query) = layout.typed(&action) {
                    self.show_search(query, page, device, Refresh::Partial, now);
                }
            }
            None => {}
        }
        Ok(())
    }

    /// Shows the settings screen, with the sample laid out again if how
    /// pages look has changed.
    fn show_settings(&mut self, device: &mut dyn Device, refresh: Refresh, now: Instant) {
        let settings = self.preferences.settings();
        if self
            .preview
            .as_ref()
            .is_none_or(|(laid_out, _)| *laid_out != settings)
        {
            let page = settings::preview(&settings);
            self.preview = Some((settings.clone(), page));
        }
        let layout = match &self.mode {
            Mode::Settings(layout) => layout.clone(),
            _ => settings::Layout::new(settings.width, settings.height),
        };
        let status = self.status_text(device, now);
        let preview = self.preview.as_ref().and_then(|(_, page)| page.as_ref());
        layout.draw(
            &mut self.screen,
            &mut self.ui,
            &self.preferences,
            self.light,
            preview,
            &status,
        );
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        self.mode = Mode::Settings(layout);
    }

    /// A step or a toggle is saved at once; the open book is laid out with
    /// the new settings when it's next shown.
    fn settings_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let Mode::Settings(layout) = &self.mode else {
            return Ok(());
        };
        match layout.hit(x, y) {
            Some(settings::Action::Step(setting, step)) => {
                let changed = setting.step(step, &mut self.preferences, &mut self.light);
                if changed == settings::Changed::Nothing {
                    return Ok(());
                }
                self.setting_changed(changed, device);
                self.show_settings(device, Refresh::Partial, now);
            }
            Some(settings::Action::Toggle(toggle)) => {
                toggle.flip(&mut self.preferences);
                save_json(device, PREFERENCES, &self.preferences);
                self.show_settings(device, Refresh::Partial, now);
            }
            Some(settings::Action::Sync) => self.show_sync(device, Refresh::Full, now),
            Some(settings::Action::Kobo) => {
                let state = kobo_page::State {
                    starts_here: device.starts_reader(),
                    ..kobo_page::State::default()
                };
                self.show_kobo(state, device, Refresh::Full, now);
            }
            Some(settings::Action::Done) => {
                let back = std::mem::replace(&mut self.settings_from, Back::Home);
                self.go_back(back, device, now)?;
            }
            None => {}
        }
        Ok(())
    }

    fn show_kobo(
        &mut self,
        state: kobo_page::State,
        device: &mut dyn Device,
        refresh: Refresh,
        now: Instant,
    ) {
        let settings = Settings::default();
        let layout = kobo_page::Layout::new(settings.width, settings.height, state);
        let status = self.status_text(device, now);
        layout.draw(&mut self.screen, &mut self.ui, &status);
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        self.mode = Mode::Kobo(layout);
    }

    fn kobo_tap(&mut self, x: f32, y: f32, now: Instant, device: &mut dyn Device) -> Flow {
        let Mode::Kobo(layout) = &self.mode else {
            return Flow::Continue;
        };
        let mut state = layout.state.clone();
        match layout.hit(x, y) {
            Some(kobo_page::Action::StartHere) => {
                let on = state.starts_here != Some(true);
                state.said = Some(match device.set_starts_reader(on) {
                    Ok(said) | Err(said) => said,
                });
                state.starts_here = device.starts_reader();
                state.confirming = false;
            }
            Some(kobo_page::Action::Uninstall) if state.confirming => {
                self.save(device);
                return Flow::Uninstall;
            }
            Some(kobo_page::Action::Uninstall) => state.confirming = true,
            Some(kobo_page::Action::Back) => {
                self.show_settings(device, Refresh::Full, now);
                return Flow::Continue;
            }
            // A tap anywhere else takes back an Uninstall not yet confirmed.
            None if state.confirming => state.confirming = false,
            None => return Flow::Continue,
        }
        self.show_kobo(state, device, Refresh::Partial, now);
        Flow::Continue
    }

    /// Saves a changed setting, and passes on what the device or the guard
    /// needs to know.
    fn setting_changed(&mut self, changed: settings::Changed, device: &mut dyn Device) {
        match changed {
            settings::Changed::Light => {
                device.set_light(self.light);
                self.preferences.light = Some(self.light);
            }
            settings::Changed::Guard => self.guard.window = self.preferences.turn_guard().window,
            settings::Changed::Layout | settings::Changed::Drawing | settings::Changed::Nothing => {
            }
        }
        save_json(device, PREFERENCES, &self.preferences);
    }

    /// The status for the bar along the top: the local time, Wi-Fi while
    /// it's up, and the battery, such as "Wed 11 Nov 11:09 · Wi-Fi · 84%".
    /// Notes when it should next change.
    fn status_text(&mut self, device: &mut dyn Device, now: Instant) -> String {
        let clock = device.clock();
        let wifi = self.network_up();
        self.status_due = Some(now + Duration::from_secs(60 - clock % 60));
        self.status_wifi = wifi;
        let mut parts = Vec::new();
        if let Some(time) = local_time(clock, device.utc_offset(clock)) {
            parts.push(time);
        }
        if wifi {
            parts.push("Wi-Fi".to_owned());
        }
        match device.battery() {
            Some(Battery {
                percent,
                charging: true,
            }) => parts.push(format!("{percent}% ↑")),
            Some(Battery { percent, .. }) => parts.push(format!("{percent}%")),
            None => {}
        }
        parts.join(" · ")
    }

    fn network_up(&self) -> bool {
        self.syncer
            .as_ref()
            .is_some_and(|syncer| syncer.setup.network.is_up())
    }

    /// Where the current screen shows the status, if it does: the top and
    /// bottom of its band, and the baseline.
    fn status_band(&self) -> Option<((f32, f32), f32)> {
        match &self.mode {
            Mode::Home(_)
            | Mode::Library(_)
            | Mode::Search(_)
            | Mode::Settings(_)
            | Mode::Kobo(_)
            | Mode::Sync(_)
            | Mode::Usb { .. }
            | Mode::Power { .. } => Some(((0.0, ui::HEADER), ui::HEADER * 0.62)),
            Mode::Menu(layout) => Some(layout.status_band()),
            Mode::Reading | Mode::Lookup { .. } | Mode::Asleep { .. } => None,
        }
    }

    /// Draws the status again where it is, once a minute has passed or
    /// Wi-Fi has come or gone, leaving the rest of the screen.
    fn refresh_status(&mut self, device: &mut dyn Device, now: Instant) {
        let Some(band) = self.status_band() else {
            self.status_due = None;
            return;
        };
        let due = self.status_due.is_some_and(|due| now >= due);
        if !due && self.network_up() == self.status_wifi {
            return;
        }
        let text = self.status_text(device, now);
        ui::status(&mut self.screen, &mut self.ui, &text, band.0, band.1);
        device.show(&self.screen, Refresh::Partial);
    }

    /// Draws the home screen into `screen`.
    fn draw_home(&mut self, layout: &home::Layout, device: &mut dyn Device, now: Instant) {
        let status = self.status_text(device, now);
        let Self {
            library,
            covers,
            ui,
            screen,
            ..
        } = self;
        if covers.len() > COVERS_KEPT {
            covers.clear();
        }
        let mut cover = |entry: &Entry| -> Option<Picture> {
            if !entry.cover {
                return None;
            }
            covers
                .entry(entry.key)
                .or_insert_with(|| {
                    device
                        .load(&library::cover_file(entry.key))
                        .and_then(|bytes| Picture::from_bytes(&bytes))
                })
                .clone()
        };
        layout.draw(screen, ui, library, &mut cover, &status);
    }

    fn reading_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let Some(book) = self.book.as_mut() else {
            return Ok(());
        };
        let width = book.reader.settings().width as f32;
        if let Some(target) = book.reader.link_at(x, y) {
            if self.guard.allow(now) {
                book.reader.follow(target)?;
                self.moved(device, now)?;
                self.save(device);
            } else {
                self.ignored += 1;
            }
        } else if x < width / 3.0 {
            self.turn(Turn::Previous, now, device)?;
        } else if x < width * 2.0 / 3.0 {
            self.open_menu(device, now)?;
        } else {
            self.turn(Turn::Next, now, device)?;
        }
        Ok(())
    }

    /// Outlines the word nearest (`x`, `y`) and shows what it means.
    fn look_up(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<(), AppError> {
        let Some(word) = self
            .book
            .as_ref()
            .and_then(|book| book.reader.word_at(x, y))
        else {
            return Ok(());
        };
        let normal = dictionary::normalize(&word.text);
        if normal.is_empty() {
            return Ok(());
        }
        // Not kept when it's missing, so it can be put there later.
        let mut why = String::new();
        if self.dictionary.is_none() {
            match load_dictionary(device) {
                Ok(dictionary) => self.dictionary = Some(dictionary),
                Err(error) => why = error,
            }
        }
        let found = match &self.dictionary {
            Some(dictionary) => {
                let entries = dictionary.lookup(&word.text);
                if entries.is_empty() {
                    lookup::Found::Nothing(normal)
                } else {
                    lookup::Found::Entries(entries)
                }
            }
            None => lookup::Found::NoDictionary(why),
        };
        self.compose_page()?;
        lookup::draw(&mut self.screen, &mut self.ui, &word, &found);
        device.show(&self.screen, Refresh::Partial);
        self.guard.refreshing(now + Refresh::Partial.duration());
        self.mode = Mode::Lookup { word: word.text };
        Ok(())
    }

    fn turn(&mut self, turn: Turn, now: Instant, device: &mut dyn Device) -> Result<(), AppError> {
        let Some(book) = self.book.as_mut() else {
            return Ok(());
        };
        if !self.guard.allow(now) {
            self.ignored += 1;
            return Ok(());
        }
        if book.reader.turn(turn)? {
            self.moved(device, now)?;
            if self.turns.is_multiple_of(SAVE_EVERY) {
                self.save(device);
            }
        }
        Ok(())
    }

    /// Shows the page after a turn or a jump.
    fn moved(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        if let Some(book) = self.book.as_mut() {
            book.moved();
        }
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
        if self.book.is_none() {
            return Ok(());
        }
        self.compose_page()?;
        device.show(&self.screen, refresh);
        self.guard.refreshing(now + refresh.duration());
        let Some(book) = self.book.as_mut() else {
            return Ok(());
        };
        book.reader.prepare()?;
        book.paginate_in_background();
        Ok(())
    }

    fn tick(&mut self, now: Instant, device: &mut dyn Device) -> Result<(), AppError> {
        let paginated = self.book.as_mut().is_some_and(|book| book.collect(device));
        if paginated && matches!(self.mode, Mode::Reading) {
            // Now with its number.
            self.compose_page()?;
            device.show(&self.screen, Refresh::Partial);
        }
        self.sync_news(device, now);
        self.refresh_status(device, now);
        if matches!(self.mode, Mode::Asleep { .. }) {
            // In case SSH came on after the sleep screen went up.
            self.ssh_off();
        }
        let syncing = self.syncing();
        match &mut self.mode {
            Mode::Asleep {
                suspend_at,
                sending_until,
                ..
            } => {
                if now >= *suspend_at {
                    if syncing && sending_until.is_some_and(|until| now < until) {
                        // The places from before sleeping: a little longer.
                        *suspend_at = now + SYNC_CHECK;
                    } else if syncing {
                        // Not with the network up: it finishes first.
                        if let Some(syncer) = &self.syncer {
                            syncer.stop();
                        }
                        *suspend_at = now + SLEEP_RETRY;
                    } else if !device.battery().is_some_and(|b| b.charging) && device.suspend() {
                        self.wake(device, Instant::now().max(now))?;
                    } else {
                        *suspend_at = now + SLEEP_RETRY;
                    }
                }
            }
            // Leaves for Nickel before it would sleep.
            Mode::Power { .. } => {}
            Mode::Home(_)
            | Mode::Library(_)
            | Mode::Search(_)
            | Mode::Settings(_)
            | Mode::Kobo(_)
            | Mode::Sync(_)
            | Mode::Usb { .. }
            | Mode::Reading
            | Mode::Lookup { .. }
            | Mode::Menu(_) => {
                let idle = now.duration_since(self.last_touch) >= AUTO_SLEEP;
                if idle && !device.battery().is_some_and(|b| b.charging) {
                    self.sleep(device, now)?;
                }
            }
        }
        Ok(())
    }

    /// The screen that's up, to come back to.
    fn here(&self) -> Back {
        match &self.mode {
            Mode::Home(_) => Back::Home,
            Mode::Library(layout) => Back::Library(layout.page()),
            Mode::Search(layout) => Back::Search(layout.query().to_owned(), layout.library_page()),
            Mode::Settings(_) | Mode::Kobo(_) => Back::Settings,
            Mode::Sync(_) => Back::Sync,
            Mode::Usb { back, .. } | Mode::Power { back, .. } | Mode::Asleep { back, .. } => {
                back.clone()
            }
            Mode::Reading | Mode::Lookup { .. } | Mode::Menu(_) => Back::Reading,
        }
    }

    /// Shows `back` again.
    fn go_back(
        &mut self,
        back: Back,
        device: &mut dyn Device,
        now: Instant,
    ) -> Result<(), AppError> {
        match back {
            Back::Reading if self.book.is_some() => {
                // Settings may have changed how pages look since.
                let settings = self.preferences.settings();
                if let Some(book) = self.book.as_mut()
                    && *book.reader.settings() != settings
                {
                    book.relayout(&settings, device)?;
                }
                self.mode = Mode::Reading;
                return self.show_page(device, Refresh::Full, now);
            }
            Back::Library(page) => self.show_library(page, device, Refresh::Full, now),
            Back::Search(query, page) => {
                self.show_search(query, page, device, Refresh::Full, now);
            }
            Back::Settings => self.show_settings(device, Refresh::Full, now),
            Back::Sync => self.show_sync(device, Refresh::Full, now),
            Back::Home | Back::Reading => self.go_home(device, Refresh::Full, now),
        }
        Ok(())
    }

    /// Plugged into a computer, or unplugged: offers to hand over to Nickel
    /// for USB, unless that was turned down already, or it's asleep, in
    /// which case it asks on waking.
    fn computer(
        &mut self,
        plugged: bool,
        device: &mut dyn Device,
        now: Instant,
    ) -> Result<(), AppError> {
        let was = std::mem::replace(&mut self.computer, plugged);
        if !plugged {
            self.usb_declined = false;
            if let Mode::Usb { back, .. } = &self.mode {
                let back = back.clone();
                return self.go_back(back, device, now);
            }
            return Ok(());
        }
        if !was && !matches!(self.mode, Mode::Asleep { .. }) {
            self.offer_usb(device, now);
        }
        Ok(())
    }

    fn offer_usb(&mut self, device: &mut dyn Device, now: Instant) {
        if self.usb_declined || matches!(self.mode, Mode::Usb { .. }) {
            return;
        }
        self.save(device);
        let back = self.here();
        let settings = Settings::default();
        let layout = usb_page::Layout::new(settings.width, settings.height);
        let status = self.status_text(device, now);
        layout.draw(&mut self.screen, &mut self.ui, &status);
        device.show(&self.screen, Refresh::Full);
        self.guard.refreshing(now + Refresh::Full.duration());
        self.mode = Mode::Usb { layout, back };
    }

    fn usb_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<Flow, AppError> {
        let Mode::Usb { layout, back } = &self.mode else {
            return Ok(Flow::Continue);
        };
        match layout.hit(x, y) {
            Some(usb_page::Action::Connect) => {
                self.save(device);
                Ok(Flow::Usb)
            }
            Some(usb_page::Action::NotNow) => {
                self.usb_declined = true;
                let back = back.clone();
                self.go_back(back, device, now)?;
                Ok(Flow::Continue)
            }
            None => Ok(Flow::Continue),
        }
    }

    /// The power button was held: asks what to do, going back to the
    /// screen that was up on Cancel.
    fn offer_power(&mut self, device: &mut dyn Device, now: Instant) {
        if matches!(self.mode, Mode::Power { .. }) {
            return;
        }
        self.save(device);
        let back = self.here();
        let layout = power_page::Layout::new(self.screen.width());
        let status = self.status_text(device, now);
        layout.draw(&mut self.screen, &mut self.ui, &status);
        device.show(&self.screen, Refresh::Full);
        self.guard.refreshing(now + Refresh::Full.duration());
        self.mode = Mode::Power {
            layout,
            back,
            nickel_at: now + POWER_WAIT,
        };
    }

    fn power_tap(
        &mut self,
        x: f32,
        y: f32,
        now: Instant,
        device: &mut dyn Device,
    ) -> Result<Flow, AppError> {
        let Mode::Power { layout, back, .. } = &self.mode else {
            return Ok(Flow::Continue);
        };
        let (flow, said, below) = match layout.hit(x, y) {
            Some(power_page::Action::PowerOff) => (
                Flow::PowerOff,
                "Switched off",
                "Hold the power button to switch it on.",
            ),
            Some(power_page::Action::Restart) => (Flow::Restart, "Restarting", ""),
            Some(power_page::Action::Nickel) => {
                self.save(device);
                return Ok(Flow::Exit);
            }
            Some(power_page::Action::Cancel) => {
                let back = back.clone();
                self.go_back(back, device, now)?;
                return Ok(Flow::Continue);
            }
            None => return Ok(Flow::Continue),
        };
        self.save(device);
        power_page::draw_final(&mut self.screen, &mut self.ui, said, below);
        device.show(&self.screen, Refresh::Full);
        Ok(flow)
    }

    fn sleep(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        self.save(device);
        let back = self.here();
        if let Some(cover) = self.sleep_cover() {
            draw_cover(&mut self.screen, &cover);
        } else {
            self.asleep_notice(&back)?;
        }
        device.show(&self.screen, Refresh::Full);
        self.ssh_off();
        // What was read goes to the server, so another reader or the
        // website has it while this one sleeps.
        let local = self.local_places(device);
        let sending = self
            .syncer
            .as_mut()
            .is_some_and(|syncer| syncer.send_places(local));
        self.mode = Mode::Asleep {
            suspend_at: now + SUSPEND_DELAY,
            sending_until: sending.then(|| now + SLEEP_SEND_LONGEST),
            back,
        };
        Ok(())
    }

    /// The cover of the book being read, to sleep on: the open book's, or
    /// else the one home shows first.
    fn sleep_cover(&mut self) -> Option<Picture> {
        let (file, key) = if let Some(book) = &self.book {
            (book.file.clone(), book.key)
        } else {
            let entry = self.library.current()?;
            (entry.file.clone(), entry.key)
        };
        if let Some((kept, cover)) = &self.sleep_cover
            && *kept == key
        {
            return cover.clone();
        }
        let has_cover = self.library.get(&file).is_some_and(|entry| entry.cover);
        let cover = has_cover
            .then(|| self.books.open(&file).ok())
            .flatten()
            .and_then(|mut book| library::cover_picture(&mut book))
            .map(|cover| cover.fitted(self.screen.width(), self.screen.height()));
        self.sleep_cover = Some((key, cover.clone()));
        cover
    }

    /// Without a cover: the screen that was up, with a band along the
    /// bottom saying it's asleep.
    fn asleep_notice(&mut self, back: &Back) -> Result<(), AppError> {
        if *back == Back::Reading {
            self.compose_page()?;
        }
        // Otherwise `screen` still holds the screen that's up.
        let (width, height) = (self.screen.width(), self.screen.height());
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
        Ok(())
    }

    /// Turns SSH off, if it's on, as before sleeping.
    fn ssh_off(&mut self) {
        if let Some(syncer) = self.syncer.as_mut() {
            syncer.ssh_stop();
        }
    }

    fn wake(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        let back = match &self.mode {
            Mode::Asleep { back, .. } => back.clone(),
            _ => Back::Home,
        };
        self.woke = Some(now);
        self.last_touch = now;
        let due = self.syncer.as_ref().is_some_and(|s| {
            s.started
                .is_none_or(|at| now.duration_since(at) >= SYNC_EVERY)
        });
        if due {
            self.start_sync(device, now);
        }
        if self.computer && !self.usb_declined {
            // Plugged in while it slept: that first, going back to `back`.
            self.offer_usb(device, now);
            return Ok(());
        }
        self.go_back(back, device, now)
    }

    /// Copies the open book's page into `screen`, with its number under it.
    fn compose_page(&mut self) -> Result<(), AppError> {
        let mut number = None;
        let page = match self.book.as_mut() {
            Some(book) => {
                number = book.reader.page_number();
                book.reader.page()?
            }
            None => None,
        };
        match page {
            Some(page) => self.screen.clone_from(page),
            None => self.screen.clear(),
        }
        if let Some((page, pages)) = number {
            page_number(&mut self.screen, &mut self.ui, page, pages);
        }
        Ok(())
    }

    fn open_menu(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        let Some(book) = &self.book else {
            return Ok(());
        };
        let layout = menu::Layout::new(book.reader.settings().width, book.reader.can_go_back());
        self.mode = Mode::Menu(layout);
        self.draw_menu(device, now)
    }

    fn draw_menu(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        let values = self.menu_values(device, now);
        self.compose_page()?;
        if let Mode::Menu(layout) = &self.mode {
            layout.draw(&mut self.screen, &mut self.ui, &values);
        }
        device.show(&self.screen, Refresh::Partial);
        self.guard.refreshing(now + Refresh::Partial.duration());
        if let Some(book) = self.book.as_mut() {
            book.paginate_in_background();
        }
        Ok(())
    }

    fn menu_values(&mut self, device: &mut dyn Device, now: Instant) -> menu::Values {
        let status = self.status_text(device, now);
        let p = &self.preferences;
        let reader = self.book.as_ref().map(|book| &book.reader);
        menu::Values {
            title: reader
                .map(|r| r.manifest().title.clone())
                .unwrap_or_default(),
            status,
            light: settings::Setting::Light.value(p, self.light),
            warmth: settings::Setting::Warmth.value(p, self.light),
            text_size: settings::Setting::TextSize.value(p, self.light),
            darkness: settings::Setting::Darkness.value(p, self.light),
            contrast: settings::Setting::Contrast.value(p, self.light),
            can_return: reader.is_some_and(Reader::can_go_back),
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
            Some(menu::Action::Home) => self.go_home(device, Refresh::Full, now),
            Some(menu::Action::Settings) => {
                self.save(device);
                self.settings_from = Back::Reading;
                self.show_settings(device, Refresh::Full, now);
            }
            Some(menu::Action::Return) => {
                if let Some(book) = self.book.as_mut() {
                    book.reader.back()?;
                }
                self.close_menu(device, now)?;
            }
            Some(menu::Action::Light(step)) => {
                self.menu_step(settings::Setting::Light, step, device, now)?;
            }
            Some(menu::Action::Warmth(step)) => {
                self.menu_step(settings::Setting::Warmth, step, device, now)?;
            }
            Some(menu::Action::TextSize(step)) => {
                self.menu_step(settings::Setting::TextSize, step, device, now)?;
            }
            Some(menu::Action::Darkness(step)) => {
                self.menu_step(settings::Setting::Darkness, step, device, now)?;
            }
            Some(menu::Action::Contrast(step)) => {
                self.menu_step(settings::Setting::Contrast, step, device, now)?;
            }
        }
        Ok(Flow::Continue)
    }

    fn close_menu(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        self.mode = Mode::Reading;
        self.show_page(device, Refresh::Full, now)
    }

    /// A step from the menu: the light changes at once, and the page is
    /// laid out again for the rest.
    fn menu_step(
        &mut self,
        setting: settings::Setting,
        step: menu::Step,
        device: &mut dyn Device,
        now: Instant,
    ) -> Result<(), AppError> {
        match setting.step(step, &mut self.preferences, &mut self.light) {
            settings::Changed::Nothing => Ok(()),
            changed @ (settings::Changed::Light | settings::Changed::Guard) => {
                self.setting_changed(changed, device);
                self.draw_menu(device, now)
            }
            settings::Changed::Layout | settings::Changed::Drawing => self.relayout(device, now),
        }
    }

    /// Saves the changed preferences, and lays the open book out again with
    /// them at the same place.
    fn relayout(&mut self, device: &mut dyn Device, now: Instant) -> Result<(), AppError> {
        save_json(device, PREFERENCES, &self.preferences);
        let settings = self.preferences.settings();
        if let Some(book) = self.book.as_mut() {
            book.relayout(&settings, device)?;
        }
        if matches!(self.mode, Mode::Menu(_)) {
            self.draw_menu(device, now)?;
        }
        Ok(())
    }
}

/// "84%", "84%, charging", or nothing if the device doesn't say.
/// The page number's text size: small, in the bottom margin.
/// `cover` as large as fits on `screen`, in the middle, on white.
fn draw_cover(screen: &mut Canvas, cover: &Picture) {
    let (width, height) = (screen.width(), screen.height());
    screen.fill(0, 0, width, height, 255);
    let scale = (width as f32 / cover.width() as f32).min(height as f32 / cover.height() as f32);
    let fitted = |side: u32, most: u32| ((side as f32 * scale).round() as u32).clamp(1, most);
    let (to_width, to_height) = (fitted(cover.width(), width), fitted(cover.height(), height));
    cover.draw(
        screen,
        ((width - to_width) / 2) as i32,
        ((height - to_height) / 2) as i32,
        to_width,
        to_height,
    );
}

/// The English–Swedish dictionary's file name, made from Folkets lexikon
/// by the `dictionary` crate.
pub const DICTIONARY: &str = "dict-en-sv";

/// Reads the dictionary file, or says why it can't.
fn load_dictionary(device: &mut dyn Device) -> Result<Dictionary, String> {
    let Some(bytes) = device.dictionary() else {
        return Err(format!(
            "To look words up, the reader's folder needs the dictionary file, {DICTIONARY}."
        ));
    };
    let damaged = |error: &dyn std::fmt::Display| {
        format!("The dictionary file, {DICTIONARY}, can't be read: {error}.")
    };
    let text = String::from_utf8(bytes).map_err(|error| damaged(&error))?;
    Dictionary::parse(text).map_err(|error| damaged(&error))
}

const PAGE_NUMBER_SIZE: f32 = 28.0;

/// "12 of 340", centred in the bottom margin, which no line of the book
/// reaches.
fn page_number(screen: &mut Canvas, ui: &mut Typesetter, page: usize, pages: usize) {
    let text = format!("{page} of {pages}");
    let width = ui.label_width(&text, PAGE_NUMBER_SIZE, 400.0);
    let x = (screen.width() as f32 - width) / 2.0;
    let baseline = screen.height() as f32 - 26.0;
    ui.draw_label(screen, &text, (x, baseline), PAGE_NUMBER_SIZE, 400.0);
}

/// Such as "Wed 11 Nov 11:09", `offset` seconds ahead of UTC.
fn local_time(clock: u64, offset: i32) -> Option<String> {
    use jiff::{
        Timestamp,
        tz::{Offset, TimeZone},
    };
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let at = Timestamp::from_second(i64::try_from(clock).ok()?).ok()?;
    let zone = TimeZone::fixed(Offset::from_seconds(offset).ok()?);
    let local = at.to_zoned(zone);
    let day = DAYS[usize::from(local.weekday().to_monday_zero_offset().unsigned_abs())];
    let month = MONTHS[usize::from(local.month().unsigned_abs()) - 1];
    Some(format!(
        "{day} {} {month} {:02}:{:02}",
        local.day(),
        local.hour(),
        local.minute()
    ))
}

fn load_json<T: serde::de::DeserializeOwned>(device: &mut dyn Device, name: &str) -> Option<T> {
    serde_json::from_slice(&device.load(name)?).ok()
}

fn save_json<T: Serialize>(device: &mut dyn Device, name: &str, value: &T) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        device.save(name, &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_time_reads_like_a_clock() {
        // 09:32 UTC, an hour behind Stockholm in November.
        assert_eq!(
            local_time(1_794_389_549, 3600).as_deref(),
            Some("Wed 11 Nov 10:32")
        );
        assert_eq!(local_time(0, 0).as_deref(), Some("Thu 1 Jan 00:00"));
    }
}
