//! The reader in a desktop window, shaped and timed like the Clara BW.
//!
//! ```sh
//! cargo run --release -p reader-emu -- book.epub [--full]
//! ```
//!
//! Takes an EPUB (converted here, as the server would) or a converted book.
//! The window is half size unless `--full` is given, since the page is
//! 1448 pixels tall.
//!
//! - Click the left third of the page, or press ←, Page Up or Backspace, to
//!   go back. Click elsewhere, or press →, Page Down or Space, to go forward.
//! - `+` and `-` change the text size, `H` hyphenation and `J` justification.
//! - `Q` or Esc quits.
//!
//! A turn shows the new page only after the panel's refresh time from
//! `docs/clara-bw-hardware.md`, with a black-and-white flash every
//! [`FULL_EVERY`] turns, and the [`TurnGuard`] ignores taps meanwhile, as on
//! the device. The page map and the last position are kept in the user's
//! cache directory, so a book reopens where it was left.
#![forbid(unsafe_code)]
#![allow(clippy::cast_precision_loss, reason = "timings and pixel sizes")]

use std::{
    io::Cursor,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use book_format::{BookReader, Limits, Position};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use reader_core::{Canvas, PageMap, Reader, Settings, Turn, TurnGuard, paginate_book};

/// GL16, the grey partial refresh for page turns: 281 ms on the device.
const PARTIAL: Duration = Duration::from_millis(281);
/// GC16 with a full update: 352–412 ms. It's shown as a flash.
const FULL: Duration = Duration::from_millis(400);
/// A full refresh clears ghosting every this many turns.
const FULL_EVERY: u32 = 6;

type Book = BookReader<Cursor<Vec<u8>>>;
type Source = Cursor<Vec<u8>>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
    else {
        eprintln!("usage: reader-emu <book.epub | book.book> [--full]");
        std::process::exit(2);
    };
    let divisor = if args.iter().any(|a| a == "--full") {
        1
    } else {
        2
    };

    let bytes = match load(&path) {
        Ok(bytes) => Arc::new(bytes),
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            std::process::exit(1);
        }
    };
    let mut hash = Fnv::default();
    hash.write(&bytes);
    let book_key = hash.0;
    let cache = Cache::new(book_key);

    let mut settings = Settings::default();
    let mut app = App::open(&bytes, book_key, &cache, settings.clone(), cache.position());
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
    while window.is_open() && !window.is_key_down(Key::Escape) && !window.is_key_down(Key::Q) {
        let now = Instant::now();
        let mut turn = None;
        let mut changed = None;
        for key in window.get_keys_pressed(KeyRepeat::Yes) {
            match key {
                Key::Right | Key::PageDown | Key::Space => turn = Some(Turn::Next),
                Key::Left | Key::PageUp | Key::Backspace => turn = Some(Turn::Previous),
                Key::Equal | Key::NumPadPlus => changed = Some(settings.font_size + 4.0),
                Key::Minus | Key::NumPadMinus => changed = Some(settings.font_size - 4.0),
                Key::H => {
                    settings.hyphenate = !settings.hyphenate;
                    changed = Some(settings.font_size);
                }
                Key::J => {
                    settings.justify = !settings.justify;
                    changed = Some(settings.font_size);
                }
                _ => {}
            }
        }
        let down = window.get_mouse_down(MouseButton::Left);
        if down
            && !was_down
            && let Some((x, _)) = window.get_mouse_pos(MouseMode::Discard)
        {
            turn = Some(if x * (divisor as f32) < settings.width as f32 / 3.0 {
                Turn::Previous
            } else {
                Turn::Next
            });
        }
        was_down = down;

        if let Some(size) = changed {
            settings.font_size = size.clamp(24.0, 80.0);
            let at = app.reader.position().unwrap_or(START);
            cache.save(&app.reader);
            app = App::open(&bytes, book_key, &cache, settings.clone(), Some(at));
            println!(
                "text {} px, hyphenation {}, justified {}",
                settings.font_size, settings.hyphenate, settings.justify
            );
        } else if let Some(turn) = turn {
            app.turn(turn, now);
        }
        app.tick(now);

        app.shown(now).show(&mut screen, divisor);
        window.set_title(&app.title());
        if window.update_with_buffer(&screen, width, height).is_err() {
            break;
        }
    }
    cache.save(&app.reader);
    if let Some(position) = app.reader.position() {
        println!("left at {position:?}");
    }
}

const START: Position = Position {
    chapter: 0,
    block: 0,
    offset: 0,
};

fn load(path: &PathBuf) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if path
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
        Ok(out.into_inner())
    } else {
        Ok(bytes)
    }
}

fn open_book(bytes: &[u8]) -> Book {
    BookReader::open(Cursor::new(bytes.to_vec()), Limits::default()).unwrap_or_else(|error| {
        eprintln!("can't open the book: {error}");
        std::process::exit(1);
    })
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

struct Refresh {
    started: Instant,
    full: bool,
}

struct App {
    reader: Reader<Source>,
    guard: TurnGuard,
    /// The page on the panel, which stays up until a refresh finishes.
    on_panel: Canvas,
    refresh: Option<Refresh>,
    turns: u32,
    ignored: u32,
    last_draw: Option<(Duration, bool)>,
    background: mpsc::Receiver<PageMap>,
    stop: Arc<AtomicBool>,
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl App {
    fn open(
        bytes: &Arc<Vec<u8>>,
        book_key: u64,
        cache: &Cache,
        settings: Settings,
        at: Option<Position>,
    ) -> Self {
        let key = PageMap::key(book_key, &settings);
        let started = Instant::now();
        let mut reader = Reader::open(
            open_book(bytes),
            settings.clone(),
            book_key,
            at.unwrap_or(START),
            cache.map(key),
        )
        .unwrap_or_else(|error| {
            eprintln!("can't open the book: {error}");
            std::process::exit(1);
        });
        let on_panel = reader
            .page()
            .ok()
            .flatten()
            .cloned()
            .unwrap_or_else(|| Canvas::new(settings.width, settings.height));
        println!("opened in {:?}", started.elapsed());

        // The rest of the book is paginated on another thread, as on the device.
        let (send, background) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        if !reader.page_map().is_complete() {
            let bytes = Arc::clone(bytes);
            let stop = Arc::clone(&stop);
            let mut map = reader.page_map().clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                let mut book = open_book(&bytes);
                let result = paginate_book(&mut book, settings, &mut map, || {
                    stop.load(Ordering::Relaxed)
                });
                if result.is_ok() && map.is_complete() {
                    println!(
                        "paginated {} pages in the background in {:?}",
                        map.total().unwrap_or(0),
                        started.elapsed()
                    );
                    let _ = send.send(map);
                }
            });
        }
        Self {
            reader,
            guard: TurnGuard::default(),
            on_panel,
            refresh: None,
            turns: 0,
            ignored: 0,
            last_draw: None,
            background,
            stop,
        }
    }

    fn turn(&mut self, turn: Turn, now: Instant) {
        if !self.guard.allow(now) {
            self.ignored += 1;
            return;
        }
        if !matches!(self.reader.turn(turn), Ok(true)) {
            return;
        }
        let cached = self
            .reader
            .current()
            .is_some_and(|p| self.reader.is_drawn(p));
        let started = Instant::now();
        let Ok(Some(canvas)) = self.reader.page() else {
            return;
        };
        self.on_panel.clone_from(canvas);
        self.last_draw = Some((started.elapsed(), cached));

        self.turns += 1;
        let full = self.turns.is_multiple_of(FULL_EVERY);
        self.guard
            .refreshing(now + if full { FULL } else { PARTIAL });
        self.refresh = Some(Refresh { started: now, full });
    }

    fn tick(&mut self, now: Instant) {
        if let Ok(map) = self.background.try_recv() {
            self.reader.adopt(&map);
        }
        if let Some(refresh) = &self.refresh {
            let length = if refresh.full { FULL } else { PARTIAL };
            if now.duration_since(refresh.started) >= length {
                self.refresh = None;
                self.guard.refreshed();
                // Idle: get both neighbours ready for the next turn.
                let started = Instant::now();
                if self.reader.prepare().is_ok() {
                    let elapsed = started.elapsed();
                    if elapsed > Duration::from_millis(20) {
                        println!("preparing the next pages took {elapsed:?}");
                    }
                }
            }
        } else if self.turns == 0 {
            let _ = self.reader.prepare();
        }
    }

    /// What the panel shows at `now`: the old page until the refresh ends,
    /// or for a full refresh, black then white then the new page.
    fn shown(&self, now: Instant) -> Shown<'_> {
        if let Some(refresh) = &self.refresh
            && refresh.full
        {
            let elapsed = now.duration_since(refresh.started);
            if elapsed < FULL / 3 {
                return Shown::Flat(0);
            } else if elapsed < FULL * 2 / 3 {
                return Shown::Flat(255);
            }
        }
        Shown::Page(&self.on_panel)
    }

    fn title(&self) -> String {
        use std::fmt::Write;
        let map = self.reader.page_map();
        let page = self.reader.current();
        let number = page.and_then(|p| map.number(p)).map(|n| n + 1);
        let mut title = format!("{} — ", self.reader.manifest().title);
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
        match self.last_draw {
            Some((_, true)) => title += " — from the cache",
            Some((elapsed, false)) => {
                let _ = write!(
                    title,
                    " — drawn in {:.1} ms",
                    elapsed.as_secs_f64() * 1000.0
                );
            }
            None => {}
        }
        if self.ignored > 0 {
            let _ = write!(title, " — {} turns ignored", self.ignored);
        }
        title
    }
}

/// Page maps and last positions, in `$XDG_CACHE_HOME/simple-books-emu`.
struct Cache {
    dir: Option<PathBuf>,
    book: u64,
}

impl Cache {
    fn new(book: u64) -> Self {
        let dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|dir| dir.join("simple-books-emu"));
        Self { dir, book }
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(name))
    }

    fn position(&self) -> Option<Position> {
        let bytes = std::fs::read(self.file(&format!("{:016x}.position.json", self.book))?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn map(&self, key: u64) -> Option<PageMap> {
        let bytes = std::fs::read(self.file(&format!("{key:016x}.pages.json"))?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, reader: &Reader<Source>) {
        let Some(dir) = &self.dir else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Some(position) = reader.position()
            && let Ok(json) = serde_json::to_vec(&position)
        {
            let _ = std::fs::write(dir.join(format!("{:016x}.position.json", self.book)), json);
        }
        let map = reader.page_map();
        if map.is_complete()
            && let Ok(json) = serde_json::to_vec(map)
        {
            let _ = std::fs::write(dir.join(format!("{:016x}.pages.json", map.key)), json);
        }
    }
}

/// 64-bit FNV-1a, to tell books apart.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}
