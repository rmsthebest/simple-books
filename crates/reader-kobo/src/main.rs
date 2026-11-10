//! The simple-books reader on the Kobo Clara BW.
//!
//! ```sh
//! reader-kobo [book.book] [--dir /mnt/onboard/.adds/simple-books]
//! ```
//!
//! Started by `kobo/reader.sh` once Nickel is stopped, which restarts Nickel
//! when this exits, however it exits. It reads the given book, or else the
//! newest `.book` in the app folder's `books/`. The app folder holds the
//! saved places, preferences and page maps, and `reader.log`.
//!
//! It runs the same [`Session`] as the emulator:
//!
//! - **Screen:** `/dev/fb0` through hwtcon, GL16 for turns and GC16 for full
//!   refreshes.
//! - **Input:** the touch panel (`event1`, grabbed so nothing else sees it)
//!   and the power button (`event2`). A press sleeps; holding it for
//!   [`HOLD_TO_EXIT`] leaves for Nickel, the way out if touch isn't working. The sleep cover's events (`event0`)
//!   are only logged for now, since what they mean isn't confirmed.
//! - **Light, battery and sleep:** sysfs, as recorded in
//!   `docs/clara-bw-hardware.md`.
#![deny(unsafe_code)]
#![allow(clippy::cast_precision_loss, reason = "pixel sizes")]

mod device;
mod input;
mod sys;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use book_format::{BookReader, Limits};
use reader_app::{Event, Flow, Opener, Session};

use crate::{
    device::Kobo,
    input::{EV_KEY, KEY_POWER, RawEvent, Touch},
    sys::Framebuffer,
};

const APP_DIR: &str = "/mnt/onboard/.adds/simple-books";
const TOUCH: &str = "/dev/input/event1";
const POWER: &str = "/dev/input/event2";
const COVER: &str = "/dev/input/event0";
/// Holding the power button this long leaves for Nickel, even if the touch
/// panel isn't working. Shorter presses sleep.
const HOLD_TO_EXIT: Duration = Duration::from_secs(3);
/// The log is cut back to its end when it grows past this.
const LOG_LIMIT: u64 = 1 << 20;

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Appends a line to `reader.log`, and to stderr.
macro_rules! log {
    ($($arg:tt)*) => { $crate::write_log(format_args!($($arg)*)) };
}
pub(crate) use log;

pub(crate) fn write_log(message: std::fmt::Arguments<'_>) {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    eprintln!("{message}");
    if let Ok(mut log) = LOG.lock()
        && let Some(file) = log.as_mut()
    {
        let _ = writeln!(file, "{seconds} {message}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|at| args.get(at + 1))
        .map_or_else(|| PathBuf::from(APP_DIR), PathBuf::from);
    let book = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--dir"))
        .map(|(_, a)| PathBuf::from(a));
    open_log(&dir);
    std::panic::set_hook(Box::new(|info| log!("panic: {info}")));
    log!("starting");
    match run(&dir, book) {
        Ok(()) => log!("back to Nickel"),
        Err(error) => {
            log!("stopping: {error}");
            std::process::exit(1);
        }
    }
}

fn open_log(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    let path = dir.join("reader.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT)
        && let Ok(old) = fs::read(&path)
    {
        let keep = &old[old.len() - (LOG_LIMIT / 4) as usize..];
        let _ = fs::write(&path, keep);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path)
        && let Ok(mut log) = LOG.lock()
    {
        *log = Some(file);
    }
}

fn run(dir: &Path, book: Option<PathBuf>) -> io::Result<()> {
    let book = match book {
        Some(book) => book,
        None => newest_book(&dir.join("books"))?,
    };
    log!("reading {}", book.display());
    let book_key = fnv(&fs::read(&book)?);
    let path = Arc::new(book);
    let open: Opener<File> = Arc::new(move || {
        let file = File::open(path.as_ref())?;
        BookReader::open(file, Limits::default())
    });

    let framebuffer = Framebuffer::open()?;
    log!("{framebuffer:?}");
    let width = framebuffer.width;
    let mut kobo = Kobo::new(framebuffer, dir.to_owned());

    let touch = open_input(TOUCH)?;
    sys::grab(&touch)?;
    let power = open_input(POWER)?;
    let cover = open_input(COVER).ok();

    let started = Instant::now();
    let mut session =
        Session::start(open, book_key, &mut kobo, started).map_err(io::Error::other)?;
    log!("opened in {:?}", started.elapsed());

    let mut taps = Touch::default();
    // When the power button went down, if it's down. A release with no
    // press, such as after the press that woke the device, does nothing.
    let mut power_down: Option<Instant> = None;
    let mut tick_at = session.deadline(Instant::now());
    loop {
        let timeout = tick_at.saturating_duration_since(Instant::now());
        let mut fds = vec![touch.as_fd(), power.as_fd()];
        if let Some(cover) = &cover {
            fds.push(cover.as_fd());
        }
        let ready = sys::poll(&fds, timeout)?;
        let now = Instant::now();
        let mut events = Vec::new();
        if ready[0] {
            for raw in read_events(&touch) {
                if let Some((x, y)) = taps.feed(raw, width) {
                    events.push(Event::Tap { x, y });
                }
            }
        }
        if ready[1] {
            for raw in read_events(&power) {
                if raw.kind != EV_KEY || raw.code != KEY_POWER {
                    continue;
                }
                match (raw.value, power_down.take()) {
                    (1, _) => power_down = Some(now),
                    (0, Some(down)) if now.duration_since(down) >= HOLD_TO_EXIT => {
                        log!("power button held: leaving for Nickel");
                        events.push(Event::Exit);
                    }
                    (0, Some(_)) => events.push(Event::Power),
                    _ => {}
                }
            }
        }
        if let Some(cover) = &cover
            && ready.get(2) == Some(&true)
        {
            for raw in read_events(cover) {
                if raw.kind == EV_KEY {
                    log!("cover: key {} = {}", raw.code, raw.value);
                }
            }
        }
        if now >= tick_at {
            events.push(Event::Tick);
        }
        for event in events {
            match session.handle(event, now, &mut kobo) {
                Ok(Flow::Continue) => {}
                Ok(Flow::Exit) => return Ok(()),
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        tick_at = session.deadline(Instant::now());
    }
}

fn open_input(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{path}: {error}")))
}

/// The events waiting on a non-blocking input device.
fn read_events(mut device: &File) -> Vec<RawEvent> {
    let mut events = Vec::new();
    let mut buffer = [0u8; 64 * input::EVENT_SIZE];
    loop {
        match device.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => events.extend(input::parse(&buffer[..n])),
        }
    }
    events
}

/// The most recently changed `.book` in `dir`.
fn newest_book(dir: &Path) -> io::Result<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "book") {
            continue;
        }
        let modified = fs::metadata(&path)?.modified()?;
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no .book in {}", dir.display()),
        )
    })
}

/// 64-bit FNV-1a, to tell books apart.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
