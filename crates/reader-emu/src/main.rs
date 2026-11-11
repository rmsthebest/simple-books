//! The reader in a desktop window, shaped and timed like the Clara BW.
//!
//! ```sh
//! cargo run --release -p reader-emu -- [books-folder | book.epub | book.book]... [--full] [--server URL] [--dictionary FILE] [--fonts FOLDER]...
//! ```
//!
//! Starts on the home screen, with the books in the given folder. EPUBs
//! (converted here, as the server would) and converted books given instead
//! are added to the emulator's own folder, `books/` in its cache directory,
//! which is used when no folder is given. The window is half size unless
//! `--full` is given, since the page is 1448 pixels tall.
//!
//! `--server URL` syncs with a simple-books server, such as one run locally
//! at `http://localhost:3000`, as the Kobo would: Settings, then Sync, then
//! Pair. Synced books go in the emulator's own folder, so no folder can be
//! given with it. The pairing stays in the cache directory, and the network
//! is always up. To try it against a server of its own:
//!
//! ```sh
//! cargo leptos build
//! PUBLIC_URL=http://localhost:3000 DATA_DIR=/tmp/simple-books-data target/debug/simple-books
//! ```
//!
//! Open the setup link it prints, make an account and add a book. Use
//! `localhost` rather than an IP address, since the sign-in cookie needs a
//! secure origin. Then run the emulator with `--server http://localhost:3000`,
//! tap Pair and type the code at `localhost:3000/device`.
//!
//! `--dictionary FILE` looks words up in that dictionary file, as made by
//! `scripts/build-kobo-files.sh` in
//! `target/kobo-files/.adds/simple-books/dict-en-sv`. Without it, the file
//! is looked for in the cache directory.
//!
//! `--fonts FOLDER` offers the fonts in that folder, and the folders in it,
//! in Settings, as the Kobo offers its own and those in `fonts/` on its
//! storage. It can be given more than once.
//!
//! It runs the same [`Session`] as the Kobo, with the mouse as the finger:
//!
//! - Click the left third of the page to go back a page, the right third to
//!   go forward, and the middle third for the menu. Click a link to follow
//!   it. Hold the button down on a word to look it up in the dictionary,
//!   and click anywhere to close that. On the home screen, click a cover to
//!   open its book.
//! - ← and Page Up go back, → Page Down and Space forward, and Backspace
//!   returns from a link. `P` is the power button, and `O` holds it down.
//! - `+` and `-` change the text size, `H` toggles hyphenation, `J`
//!   justification and `K` Knuth–Plass line breaking. `[` and `]` change the
//!   text weight, and `,` and `.` its gamma.
//! - `Q` or Esc quits.
//!
//! A new page appears only after the panel's refresh time from
//! `docs/clara-bw-hardware.md`, and a full refresh flashes black and white.
//! The emulator doesn't suspend: the sleep screen stays up until a click.
//! Positions, preferences and page maps are kept in the user's cache
//! directory, so a book reopens where it was left.
#![forbid(unsafe_code)]
#![allow(clippy::cast_precision_loss, reason = "pixel sizes")]

use std::{
    fs::File,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime},
};

use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use reader_app::{
    AlwaysOnline, Battery, Device, Event, Flow, Folder, Light, Preferences, Refresh, Session,
    SyncSetup, Turn,
};
use reader_core::{Canvas, Settings};

const USAGE: &str = "usage: reader-emu [books-folder | book.epub | book.book]... [--full] \
                     [--server URL] [--dictionary FILE] [--fonts FOLDER]...";

/// What the command line asks for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    paths: Vec<PathBuf>,
    full: bool,
    server: Option<String>,
    dictionary: Option<PathBuf>,
    fonts: Vec<PathBuf>,
}

impl Args {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => return Err(USAGE.to_owned()),
                "--full" => parsed.full = true,
                "--server" => {
                    let url = args.next().ok_or("--server needs the server's address")?;
                    parsed.server = Some(server_url(&url)?);
                }
                _ if arg.starts_with("--server=") => {
                    parsed.server = Some(server_url(&arg["--server=".len()..])?);
                }
                "--dictionary" => {
                    let file = args
                        .next()
                        .ok_or("--dictionary needs the dictionary file")?;
                    parsed.dictionary = Some(PathBuf::from(file));
                }
                _ if arg.starts_with("--dictionary=") => {
                    parsed.dictionary = Some(PathBuf::from(&arg["--dictionary=".len()..]));
                }
                "--fonts" => {
                    let folder = args.next().ok_or("--fonts needs a folder of fonts")?;
                    parsed.fonts.push(PathBuf::from(folder));
                }
                _ if arg.starts_with("--fonts=") => {
                    parsed.fonts.push(PathBuf::from(&arg["--fonts=".len()..]));
                }
                _ if arg.starts_with("--") => return Err(format!("unknown option {arg}\n{USAGE}")),
                _ => parsed.paths.push(PathBuf::from(arg)),
            }
        }
        Ok(parsed)
    }
}

/// The server's address, without a trailing slash.
fn server_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let host = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("{url}: expected http:// or https://"))?;
    if host.is_empty() || host.contains('/') {
        return Err(format!(
            "{url}: expected just the server, like http://localhost:3000"
        ));
    }
    Ok(url.to_owned())
}

/// The mouse button as the finger, as on the Kobo: a tap where it went
/// down, once let go, or a hold once it's rested there long enough.
#[derive(Debug, Default)]
struct Mouse {
    /// Where and when it went down, and whether that's been a hold.
    pressed: Option<((f32, f32), Instant, bool)>,
}

impl Mouse {
    fn feed(&mut self, down: bool, at: Option<(f32, f32)>, now: Instant) -> Option<Event> {
        match (&mut self.pressed, down) {
            (None, true) => {
                self.pressed = at.map(|at| (at, now, false));
                None
            }
            (Some(((x, y), since, held)), true) => {
                if !*held && now.duration_since(*since) >= reader_app::LONG_PRESS {
                    *held = true;
                    return Some(Event::Hold { x: *x, y: *y });
                }
                None
            }
            (Some(((x, y), _, held)), false) => {
                let tap = (!*held).then_some(Event::Tap { x: *x, y: *y });
                self.pressed = None;
                tap
            }
            (None, false) => None,
        }
    }
}

fn main() {
    let args = Args::parse(std::env::args().skip(1)).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let divisor = if args.full { 1 } else { 2 };

    let settings = Settings::default();
    let mut panel = Panel::new(settings.width, settings.height);
    panel.dictionary.clone_from(&args.dictionary);
    let Some(own) = panel.dir.as_ref().map(|dir| dir.join("books")) else {
        eprintln!("no cache directory: set XDG_CACHE_HOME or HOME");
        std::process::exit(1);
    };
    let folder = books_folder(&args.paths, &own, args.server.is_some()).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    println!("books in {}", folder.display());

    if !args.fonts.is_empty() {
        let found = reader_core::find_fonts(&args.fonts);
        println!("fonts: {}", found.join(", "));
    }
    let started = Instant::now();
    let books = Arc::new(Folder::new(folder));
    let mut session = Session::start(books, &mut panel, started).unwrap_or_else(|error| {
        eprintln!("can't start: {error}");
        std::process::exit(1);
    });
    println!("home in {:?}", started.elapsed());
    if let (Some(server), Some(dir)) = (args.server, panel.dir.clone()) {
        println!("syncing with {server}: Settings, then Sync");
        let setup = SyncSetup {
            dir,
            server: Some(server),
            name: "Emulator".to_owned(),
            network: Arc::new(AlwaysOnline),
            ssh: None,
            updates: false,
        };
        session.enable_sync(setup, &mut panel, Instant::now());
    }

    let (width, height) = (
        settings.width as usize / divisor,
        settings.height as usize / divisor,
    );
    let mut window = Window::new("reader-emu", width, height, WindowOptions::default())
        .unwrap_or_else(|error| {
            eprintln!("can't open a window: {error}");
            std::process::exit(1);
        });
    window.set_target_fps(60);

    let mut screen = vec![0u32; width * height];
    let mut mouse = Mouse::default();
    let mut tick_at = session.deadline(Instant::now());
    while window.is_open() && !window.is_key_down(Key::Escape) && !window.is_key_down(Key::Q) {
        let now = Instant::now();
        let mut events = Vec::new();
        for key in window.get_keys_pressed(KeyRepeat::Yes) {
            if let Some(event) = key_event(key) {
                events.push(event);
            } else if let Some(change) = preference_key(key) {
                if let Err(error) = session.update_preferences(&mut panel, now, change) {
                    eprintln!("{error}");
                }
                print_preferences(session.preferences());
            }
        }
        let down = window.get_mouse_down(MouseButton::Left);
        let at = window
            .get_mouse_pos(MouseMode::Discard)
            .map(|(x, y)| (x * divisor as f32, y * divisor as f32));
        events.extend(mouse.feed(down, at, now));
        if now >= tick_at {
            events.push(Event::Tick);
        }

        // Exit to Nickel, or the power off or restart the Kobo would do.
        let mut exit = None;
        for event in events {
            match session.handle(event, now, &mut panel) {
                Ok(Flow::Continue) => {}
                Ok(flow) => exit = Some(flow),
                Err(error) => eprintln!("{error}"),
            }
            tick_at = session.deadline(now);
        }
        if let Some(flow) = exit {
            println!("{flow:?}");
            break;
        }

        panel.shown(now).show(&mut screen, divisor);
        window.set_title(&title(&session));
        if window.update_with_buffer(&screen, width, height).is_err() {
            break;
        }
    }
    session.save(&mut panel);
    if let Some(position) = session.reader().and_then(reader_core::Reader::position) {
        println!("left at {position:?}");
    }
}

fn key_event(key: Key) -> Option<Event> {
    Some(match key {
        Key::Right | Key::PageDown | Key::Space => Event::Turn(Turn::Next),
        Key::Left | Key::PageUp => Event::Turn(Turn::Previous),
        Key::Backspace => Event::Back,
        Key::P => Event::Power,
        Key::O => Event::PowerHeld,
        _ => return None,
    })
}

/// A change to the preferences.
type Change = Box<dyn FnOnce(&mut Preferences)>;

/// A key that changes a preference, as the change it makes.
fn preference_key(key: Key) -> Option<Change> {
    Some(match key {
        Key::Equal | Key::NumPadPlus => Box::new(|p| p.font_size = (p.font_size + 4.0).min(80.0)),
        Key::Minus | Key::NumPadMinus => Box::new(|p| p.font_size = (p.font_size - 4.0).max(24.0)),
        Key::H => Box::new(|p| p.hyphenate = !p.hyphenate),
        Key::J => Box::new(|p| p.justify = !p.justify),
        Key::K => Box::new(|p| p.optimal_breaks = !p.optimal_breaks),
        Key::LeftBracket => Box::new(|p| p.weight = (p.weight - 50.0).max(300.0)),
        Key::RightBracket => Box::new(|p| p.weight = (p.weight + 50.0).min(700.0)),
        // Rounded, so repeated steps land on tenths.
        Key::Comma => Box::new(|p| p.gamma = ((p.gamma - 0.2).max(0.6) * 10.0).round() / 10.0),
        Key::Period => Box::new(|p| p.gamma = ((p.gamma + 0.2).min(3.0) * 10.0).round() / 10.0),
        _ => return None,
    })
}

fn print_preferences(p: &Preferences) {
    println!(
        "text {} px, weight {}, gamma {}, hyphenation {}, justified {}, Knuth–Plass {}",
        p.font_size, p.weight, p.gamma, p.hyphenate, p.justify, p.optimal_breaks
    );
}

fn title(session: &Session<File>) -> String {
    use std::fmt::Write;
    let reader = match session.reader() {
        Some(reader) if !session.at_home() => reader,
        _ => return format!("reader-emu — {} books", session.library().books().len()),
    };
    let map = reader.page_map();
    let page = reader.current();
    let number = page.and_then(|p| map.number(p)).map(|n| n + 1);
    let mut title = format!("{} — ", reader.manifest().title);
    match (number, map.total(), page) {
        (Some(n), Some(total), _) => {
            let _ = write!(title, "page {n} of {total}");
        }
        (_, _, Some(page)) => {
            let _ = write!(
                title,
                "chapter {}, page {}",
                page.chapter + 1,
                page.page + 1
            );
        }
        _ => {}
    }
    if reader.can_go_back() {
        title += " — Backspace returns";
    }
    let ignored = session.ignored_turns();
    if ignored > 0 {
        let _ = write!(title, " — {ignored} turns ignored");
    }
    title
}

/// The folder of books to show: the one given, or else `own`, after adding
/// the books given to `own`. A folder can't be given when syncing, which keeps
/// books in `own`.
fn books_folder(paths: &[PathBuf], own: &Path, syncing: bool) -> Result<PathBuf, String> {
    let mut folder = own.to_owned();
    for path in paths {
        if !path.is_dir() {
            add(path, own).map_err(|error| format!("{}: {error}", path.display()))?;
        } else if syncing {
            return Err(format!(
                "{}: with --server, books are kept in {}, so leave out the folder",
                path.display(),
                own.display()
            ));
        } else {
            folder.clone_from(path);
        }
    }
    Ok(folder)
}

/// Adds an EPUB, converted, or a converted book to the folder `books`, as
/// `<name>.book`.
fn add(path: &Path, books: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let bytes = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
    {
        let mut out = Cursor::new(Vec::new());
        book_convert::convert(
            Cursor::new(bytes),
            &mut out,
            &book_convert::Options::default(),
        )
        .map_err(|e| e.to_string())?;
        out.into_inner()
    } else {
        bytes
    };
    let name = path.file_stem().ok_or("no file name")?;
    let to = books.join(name).with_extension("book");
    // Rewriting the same book would make it look newly added.
    if std::fs::read(&to).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    std::fs::create_dir_all(books).map_err(|e| e.to_string())?;
    std::fs::write(&to, bytes).map_err(|e| e.to_string())
}

/// The emulated panel and the rest of the device.
struct Panel {
    /// What the panel shows once the refresh under way ends.
    next: Canvas,
    /// What it showed before, still up during a partial refresh.
    before: Canvas,
    refresh: Option<(Instant, Refresh)>,
    light: Light,
    /// Positions, preferences and page maps, in
    /// `$XDG_CACHE_HOME/simple-books-emu`.
    dir: Option<PathBuf>,
    /// This computer's time zone, for the time in the status.
    zone: jiff::tz::TimeZone,
    /// From `--dictionary`.
    dictionary: Option<PathBuf>,
}

impl Panel {
    fn new(width: u32, height: u32) -> Self {
        let dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|dir| dir.join("simple-books-emu"));
        Self {
            next: Canvas::new(width, height),
            before: Canvas::new(width, height),
            refresh: None,
            light: Light {
                brightness: 30,
                warmth: 4,
            },
            dir,
            zone: jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC),
            dictionary: None,
        }
    }

    /// What the panel shows at `now`: the old page until a partial refresh
    /// ends, and black then white for the first two thirds of a full one.
    fn shown(&self, now: Instant) -> Shown<'_> {
        let Some((started, refresh)) = self.refresh else {
            return Shown::Page(&self.next);
        };
        let elapsed = now.duration_since(started);
        let length = refresh.duration();
        match refresh {
            _ if elapsed >= length => Shown::Page(&self.next),
            Refresh::Partial => Shown::Page(&self.before),
            Refresh::Full if elapsed < length / 3 => Shown::Flat(0),
            Refresh::Full if elapsed < length * 2 / 3 => Shown::Flat(255),
            Refresh::Full => Shown::Page(&self.next),
        }
    }
}

impl Device for Panel {
    fn show(&mut self, screen: &Canvas, refresh: Refresh) {
        std::mem::swap(&mut self.before, &mut self.next);
        self.next.clone_from(screen);
        self.refresh = Some((Instant::now(), refresh));
    }

    fn battery(&mut self) -> Option<Battery> {
        None
    }

    fn light(&mut self) -> Light {
        self.light
    }

    fn set_light(&mut self, light: Light) {
        self.light = light;
        println!("front light {}%, warmth {}", light.brightness, light.warmth);
    }

    fn suspend(&mut self) -> bool {
        println!("asleep; the emulator doesn't suspend, so click to wake");
        false
    }

    fn load(&mut self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.dir.as_ref()?.join(name)).ok()
    }

    fn dictionary(&mut self) -> Option<Vec<u8>> {
        match &self.dictionary {
            Some(file) => std::fs::read(file)
                .inspect_err(|error| eprintln!("{}: {error}", file.display()))
                .ok(),
            None => self.load(reader_app::DICTIONARY),
        }
    }

    fn save(&mut self, name: &str, bytes: &[u8]) {
        let Some(dir) = &self.dir else {
            return;
        };
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(dir.join(name), bytes);
        }
    }

    fn clock(&mut self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }

    fn utc_offset(&mut self, clock: u64) -> i32 {
        i64::try_from(clock)
            .ok()
            .and_then(|clock| jiff::Timestamp::from_second(clock).ok())
            .map_or(0, |at| self.zone.to_offset(at).seconds())
    }
}

/// What the window shows while a refresh is under way.
enum Shown<'a> {
    Page(&'a Canvas),
    Flat(u8),
}

impl Shown<'_> {
    fn show(&self, screen: &mut [u32], divisor: usize) {
        match self {
            Shown::Flat(gray) => screen.fill(rgb(*gray)),
            Shown::Page(canvas) => {
                let width = canvas.width() as usize;
                let pixels = canvas.pixels();
                let out_width = width / divisor;
                for (y, row) in screen.chunks_mut(out_width).enumerate() {
                    for (x, out) in row.iter_mut().enumerate() {
                        // The average of a divisor × divisor square.
                        let mut sum = 0u32;
                        for dy in 0..divisor {
                            let at = (y * divisor + dy) * width + x * divisor;
                            sum += pixels[at..at + divisor]
                                .iter()
                                .map(|p| u32::from(*p))
                                .sum::<u32>();
                        }
                        *out = rgb((sum / (divisor * divisor) as u32) as u8);
                    }
                }
            }
        }
    }
}

fn rgb(gray: u8) -> u32 {
    let g = u32::from(gray);
    (g << 16) | (g << 8) | g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|&a| a.to_owned()))
    }

    #[test]
    fn the_command_line_reads() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
        let args = parse(&["books", "--full", "--server", "http://localhost:3000/"]).unwrap();
        assert_eq!(args.paths, [PathBuf::from("books")]);
        assert!(args.full);
        assert_eq!(args.server.as_deref(), Some("http://localhost:3000"));
        assert_eq!(
            parse(&["--dictionary", "dict-en-sv"]).unwrap().dictionary,
            Some(PathBuf::from("dict-en-sv"))
        );
        assert_eq!(
            parse(&["--fonts", "a", "--fonts=b"]).unwrap().fonts,
            [PathBuf::from("a"), PathBuf::from("b")]
        );
        assert_eq!(
            parse(&["--server=https://books.example.com"])
                .unwrap()
                .server
                .as_deref(),
            Some("https://books.example.com")
        );
        for bad in [
            &["--server"][..],
            &["--server", "localhost:3000"],
            &["--server", "https://books.example.com/kobo/x"],
            &["--fast"],
            &["--dictionary"],
            &["--fonts"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
