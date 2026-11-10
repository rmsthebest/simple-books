//! The reader in a desktop window, shaped and timed like the Clara BW.
//!
//! ```sh
//! cargo run --release -p reader-emu -- [books-folder | book.epub | book.book]... [--full]
//! ```
//!
//! Starts on the home screen, with the books in the given folder. EPUBs
//! (converted here, as the server would) and converted books given instead
//! are added to the emulator's own folder, `books/` in its cache directory,
//! which is used when no folder is given. The window is half size unless
//! `--full` is given, since the page is 1448 pixels tall.
//!
//! It runs the same [`Session`] as the Kobo, with the mouse as the finger:
//!
//! - Click the left third of the page to go back a page, and elsewhere to go
//!   forward. Click a link to follow it, and the top of the page for the
//!   menu. On the home screen, click a cover to open its book.
//! - ← and Page Up go back, → Page Down and Space forward, and Backspace
//!   returns from a link. `P` is the power button.
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
    Battery, Device, Event, Flow, Folder, Light, Preferences, Refresh, Session, Turn,
};
use reader_core::{Canvas, Settings};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: reader-emu [books-folder | book.epub | book.book]... [--full]");
        std::process::exit(2);
    }
    let divisor = if args.iter().any(|a| a == "--full") {
        1
    } else {
        2
    };

    let settings = Settings::default();
    let mut panel = Panel::new(settings.width, settings.height);
    let Some(own) = panel.dir.as_ref().map(|dir| dir.join("books")) else {
        eprintln!("no cache directory: set XDG_CACHE_HOME or HOME");
        std::process::exit(1);
    };
    let mut folder = own.clone();
    for arg in args.iter().filter(|a| !a.starts_with("--")) {
        let path = PathBuf::from(arg);
        if path.is_dir() {
            folder = path;
        } else if let Err(error) = add(&path, &own) {
            eprintln!("{}: {error}", path.display());
            std::process::exit(1);
        }
    }
    println!("books in {}", folder.display());

    let started = Instant::now();
    let books = Arc::new(Folder::new(folder));
    let mut session = Session::start(books, &mut panel, started).unwrap_or_else(|error| {
        eprintln!("can't start: {error}");
        std::process::exit(1);
    });
    println!("home in {:?}", started.elapsed());

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
    let mut was_down = false;
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
        if down
            && !was_down
            && let Some((x, y)) = window.get_mouse_pos(MouseMode::Discard)
        {
            events.push(Event::Tap {
                x: x * divisor as f32,
                y: y * divisor as f32,
            });
        }
        was_down = down;
        if now >= tick_at {
            events.push(Event::Tick);
        }

        let mut exit = false;
        for event in events {
            match session.handle(event, now, &mut panel) {
                Ok(Flow::Continue) => {}
                Ok(Flow::Exit) => exit = true,
                Err(error) => eprintln!("{error}"),
            }
            tick_at = session.deadline(now);
        }
        if exit {
            println!("exit to Nickel");
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
