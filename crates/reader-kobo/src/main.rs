//! The simple-books reader on the Kobo Clara BW.
//!
//! ```sh
//! reader-kobo [--boot] [--dir /mnt/onboard/.adds/simple-books]
//! reader-kobo --restore-screen
//! ```
//!
//! The reader sets the screen to 8-bit gray, as KOReader does, and puts
//! Nickel's settings back when it exits. `--restore-screen` does that after a
//! reader that crashed or was killed, and nothing otherwise; `reader.sh` runs
//! it every time.
//!
//! Started by `kobo/reader.sh` once Nickel is stopped, which restarts Nickel
//! when this exits, however it exits. Or, with `--boot`, by `kobo/boot.sh`
//! from `/etc/rc.local` before Nickel has started, which starts it when this
//! exits. Then it leaves for Nickel straight away if a finger is on the
//! screen, and once home is up it does what Nickel would: resets the boot
//! counter (see `docs/clara-bw-hardware.md`) and turns the blinking boot
//! light off. It also removes `boot-started`, so the next boot starts it
//! again. It starts on the home screen, with the `.book` files in the app
//! folder's `books/`. The app folder also holds the library index, covers,
//! saved places, preferences and page maps, and `reader.log`.
//!
//! It runs the same [`Session`] as the emulator:
//!
//! - **Screen:** `/dev/fb0` through hwtcon, GL16 for turns and GC16 for full
//!   refreshes.
//! - **Input:** the touch panel (`event1`, grabbed so nothing else sees it)
//!   and the power button (`event2`). A press sleeps; holding it for
//!   [`HOLD`] brings up the power screen: power off, restart or Nickel. It
//!   goes to Nickel if nothing is tapped, so holding the button is still
//!   the way out if touch isn't working. Powering off and restarting are
//!   left to `reader.sh` and `boot.sh`, asked for with [`EXIT_POWER_OFF`]
//!   and [`EXIT_RESTART`]. The sleep cover's events (`event0`) are only
//!   logged for now, since what they mean isn't confirmed.
//! - **Light, battery and sleep:** sysfs, as recorded in
//!   `docs/clara-bw-hardware.md`.
//! - **Sync:** with the server Nickel syncs with, or the one in `server`
//!   in the app folder. Wi-Fi is on only while a sync, a pairing or SSH
//!   needs it, through `wifi.sh` (see [`online`]). It syncs at start, on
//!   Sync now, and on waking every half hour.
//! - **USB:** plugged into a computer, as the kernel's device events and
//!   `extcon` tell it, it offers to leave for Nickel, which does USB.
#![deny(unsafe_code)]
#![allow(clippy::cast_precision_loss, reason = "pixel sizes")]

mod device;
mod in_nickel;
mod input;
mod online;
mod sys;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use reader_app::{Event, Flow, Folder, LONG_PRESS, Session, SyncSetup};

use crate::{
    device::Kobo,
    input::{BTN_TOUCH, EV_KEY, KEY_POWER, RawEvent, Touch},
    sys::Framebuffer,
};

const APP_DIR: &str = "/mnt/onboard/.adds/simple-books";
const TOUCH: &str = "/dev/input/event1";
const POWER: &str = "/dev/input/event2";
const COVER: &str = "/dev/input/event0";
/// Which USB cable is in: `SDP=1` or `CDP=1` is a computer, `DCP=1` a
/// charger. Nickel offers to connect for the first two.
const EXTCON: &str = "/sys/class/extcon/extcon0/state";
/// Holding the power button this long brings up the power screen, which
/// goes to Nickel if nothing is tapped. Shorter presses sleep.
const HOLD: Duration = Duration::from_secs(3);
/// The exit status that asks `reader.sh` or `boot.sh` to power off, as
/// KOReader's does.
const EXIT_POWER_OFF: i32 = 88;
/// The exit status that asks `reader.sh` or `boot.sh` to restart.
const EXIT_RESTART: i32 = 89;
/// Handling an event that takes this long is logged, such as opening a
/// book.
const SLOW: Duration = Duration::from_millis(300);
/// The log is cut back to its end when it grows past this.
const LOG_LIMIT: u64 = 1 << 20;
/// Made by `boot.sh` as it starts the reader at boot, and removed once home
/// is up. Still there at the next boot, it means this one never got that
/// far, so that boot is Nickel's.
const BOOT_STARTED: &str = "boot-started";
/// The LEDs Nickel turns off once it's up: on the Clara BW the white one,
/// `LED`, blinks from early in the boot. The green one is other models'.
const BOOT_LIGHTS: [&str; 2] = [
    "/sys/class/leds/LED/brightness",
    "/sys/class/leds/bd71828-green-led/brightness",
];

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
    if args.iter().any(|a| a == "--restore-screen") {
        if let Err(error) = sys::restore_screen() {
            eprintln!("restoring the screen: {error}");
            std::process::exit(1);
        }
        return;
    }
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|at| args.get(at + 1))
        .map_or_else(|| PathBuf::from(APP_DIR), PathBuf::from);
    open_log(&dir);
    std::panic::set_hook(Box::new(|info| log!("panic: {info}")));
    if let Some(nickel) = args
        .iter()
        .position(|a| a == "--in-nickel")
        .and_then(|at| args.get(at + 1))
    {
        in_nickel::run(&dir, nickel);
        return;
    }
    let boot = args.iter().any(|a| a == "--boot");
    log!("starting{}", if boot { " at boot" } else { "" });
    match run(&dir, boot) {
        Ok(Flow::PowerOff) => {
            log!("powering off");
            std::process::exit(EXIT_POWER_OFF);
        }
        Ok(Flow::Restart) => {
            log!("restarting");
            std::process::exit(EXIT_RESTART);
        }
        Ok(Flow::Usb) => {
            // in-nickel brings the reader back once it's unplugged.
            request(&dir, in_nickel::USB_RETURN);
            log!("back to Nickel for USB");
        }
        Ok(Flow::Uninstall) => {
            // in-nickel does it, once Nickel has started.
            request(&dir, in_nickel::UNINSTALL);
            log!("back to Nickel, to uninstall");
        }
        Ok(Flow::Continue | Flow::Exit) => log!("back to Nickel"),
        Err(error) => {
            log!("stopping: {error}");
            std::process::exit(1);
        }
    }
}

/// Leaves a request for `reader-kobo --in-nickel`, in the app folder.
fn request(dir: &Path, name: &str) {
    if let Err(error) = fs::write(dir.join(name), "") {
        log!("asking for {name}: {error}");
    }
    sys::sync();
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

/// Runs until it's time to leave, and says why: [`Flow::Exit`] for Nickel,
/// or to power off or restart.
fn run(dir: &Path, boot: bool) -> io::Result<Flow> {
    if boot && finger_on_screen() {
        log!("a finger on the screen at boot: Nickel instead");
        return Ok(Flow::Exit);
    }
    let framebuffer = Framebuffer::open()?;
    log!("{framebuffer:?}");
    let width = framebuffer.width;
    let mut kobo = Kobo::new(framebuffer, dir.to_owned());
    let mut inputs = Inputs::open(width)?;

    let started = Instant::now();
    let books = Arc::new(Folder::new(dir.join("books")));
    let mut session = Session::start(books, &mut kobo, started).map_err(io::Error::other)?;
    log!(
        "home in {:?}, {} books",
        started.elapsed(),
        session.library().books().len()
    );
    let setup = SyncSetup {
        dir: dir.to_owned(),
        server: online::server(dir),
        name: "Kobo Clara BW".to_owned(),
        network: Arc::new(online::Wifi::new(dir)),
        ssh: Some(Arc::new(online::KoboSsh::new(dir))),
    };
    session.enable_sync(setup, &mut kobo, Instant::now());
    if boot {
        booted(dir);
    }
    if inputs.computer {
        session
            .handle(Event::Computer(true), Instant::now(), &mut kobo)
            .map_err(io::Error::other)?;
    }

    let mut tick_at = session.deadline(Instant::now());
    // For logging each sync as it ends: when the job started, and the last
    // result logged.
    let mut job_started = session.syncing().then(Instant::now);
    let mut last_logged = session.last_sync_at();
    loop {
        let timeout = tick_at.saturating_duration_since(Instant::now());
        let mut events = inputs.wait(timeout)?;
        let now = Instant::now();
        if now >= tick_at {
            events.push(Event::Tick);
        }
        for event in events {
            let handling = Instant::now();
            let flow = session.handle(event, now, &mut kobo);
            if handling.elapsed() >= SLOW {
                let open = session.reader().map(|r| r.manifest().title.clone());
                log!(
                    "{event:?} took {:?}; at home: {}, book: {open:?}",
                    handling.elapsed(),
                    session.at_home()
                );
            }
            match flow {
                Ok(Flow::Continue) => {}
                Ok(flow) => return Ok(flow),
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        log_sync(&session, &mut job_started, &mut last_logged);
        tick_at = session.deadline(Instant::now());
    }
}

/// Everything the reader waits on: the touch panel, the power button, the
/// sleep cover, and the kernel's device events for USB.
struct Inputs {
    touch: File,
    power: File,
    cover: Option<File>,
    uevents: Option<File>,
    taps: Touch,
    width: u32,
    /// When the power button went down, if it's down. A release with no
    /// press, such as after the press that woke the device, does nothing.
    power_down: Option<Instant>,
    /// The button has been down for [`HOLD`] and the power screen asked
    /// for, so letting go does nothing.
    held: bool,
    /// When the finger on the panel came down, if one is.
    touch_down: Option<Instant>,
    /// It rested there for [`LONG_PRESS`], and a hold was sent, so lifting
    /// it is no tap.
    touch_held: bool,
    /// Plugged into a computer, as last seen.
    computer: bool,
}

impl Inputs {
    fn open(width: u32) -> io::Result<Self> {
        let touch = open_input(TOUCH)?;
        sys::grab(&touch)?;
        let uevents = match sys::uevents() {
            Ok(socket) => Some(socket),
            Err(error) => {
                log!("no device events, so USB isn't noticed: {error}");
                None
            }
        };
        Ok(Self {
            touch,
            power: open_input(POWER)?,
            cover: open_input(COVER).ok(),
            uevents,
            taps: Touch::default(),
            width,
            power_down: None,
            held: false,
            touch_down: None,
            touch_held: false,
            computer: plugged_into_computer(),
        })
    }

    /// Waits up to `timeout` for something to happen, and says what did.
    fn wait(&mut self, timeout: Duration) -> io::Result<Vec<Event>> {
        let mut fds = vec![self.touch.as_fd(), self.power.as_fd()];
        let cover_at = self.cover.as_ref().map(|cover| {
            fds.push(cover.as_fd());
            fds.len() - 1
        });
        let uevents_at = self.uevents.as_ref().map(|uevents| {
            fds.push(uevents.as_fd());
            fds.len() - 1
        });
        // Held down, the power screen comes up without waiting to be let go.
        let mut timeout = match self.power_down {
            Some(down) if !self.held => {
                timeout.min((down + HOLD).saturating_duration_since(Instant::now()))
            }
            _ => timeout,
        };
        // A finger resting is a hold without waiting for it to lift.
        if let Some(down) = self.touch_down
            && !self.touch_held
        {
            timeout = timeout.min((down + LONG_PRESS).saturating_duration_since(Instant::now()));
        }
        let ready = sys::poll(&fds, timeout)?;
        let now = Instant::now();
        let mut events = Vec::new();
        if ready[0] {
            for raw in read_events(&self.touch) {
                if let Some((x, y)) = self.taps.feed(raw, self.width) {
                    self.touch_down = None;
                    if !std::mem::take(&mut self.touch_held) {
                        events.push(Event::Tap { x, y });
                    }
                } else if self.touch_down.is_none() && self.taps.down_at().is_some() {
                    self.touch_down = Some(now);
                }
            }
            // A touch the panel dropped without a lift: nothing to wait
            // for, or the timeout above would stay at nothing.
            if self.taps.down_at().is_none() {
                self.touch_down = None;
                self.touch_held = false;
            }
        }
        if let (Some(down), Some((x, y))) = (self.touch_down, self.taps.down_at())
            && !self.touch_held
            && now.duration_since(down) >= LONG_PRESS
        {
            self.touch_held = true;
            events.push(Event::Hold { x, y });
        }
        if ready[1] {
            for raw in read_events(&self.power) {
                if raw.kind != EV_KEY || raw.code != KEY_POWER {
                    continue;
                }
                match (raw.value, self.power_down.take()) {
                    (1, _) => {
                        self.power_down = Some(now);
                        self.held = false;
                    }
                    (0, Some(_)) if std::mem::take(&mut self.held) => {}
                    (0, Some(down)) if now.duration_since(down) >= HOLD => {
                        log!("power button held");
                        events.push(Event::PowerHeld);
                    }
                    (0, Some(_)) => events.push(Event::Power),
                    _ => {}
                }
            }
        }
        if let Some(down) = self.power_down
            && !self.held
            && now.duration_since(down) >= HOLD
        {
            log!("power button held");
            self.held = true;
            events.push(Event::PowerHeld);
        }
        if let Some(cover) = &self.cover
            && cover_at.is_some_and(|at| ready[at])
        {
            for raw in read_events(cover) {
                if raw.kind == EV_KEY {
                    log!("cover: key {} = {}", raw.code, raw.value);
                }
            }
        }
        if let Some(uevents) = &self.uevents
            && uevents_at.is_some_and(|at| ready[at])
        {
            drain(uevents);
            let plugged = plugged_into_computer();
            if plugged != self.computer {
                self.computer = plugged;
                log!("USB: {}", if plugged { "a computer" } else { "unplugged" });
                events.push(Event::Computer(plugged));
            }
        }
        Ok(events)
    }
}

/// Whether a finger is on the touch panel. If it can't tell, no.
fn finger_on_screen() -> bool {
    File::open(TOUCH)
        .and_then(|touch| sys::key_down(&touch, BTN_TOUCH))
        .unwrap_or(false)
}

/// Home is up after starting at boot: what Nickel does once it's up, and
/// the sign that this boot got this far.
fn booted(dir: &Path) {
    match Command::new("bootcount-ctl").arg("reset").status() {
        Ok(status) if status.success() => {}
        Ok(status) => log!("bootcount-ctl reset: {status}"),
        Err(error) => log!("bootcount-ctl reset: {error}"),
    }
    // The boot light blinks until Nickel turns it off; writing 0 also stops
    // its blink timer.
    for light in BOOT_LIGHTS {
        if Path::new(light).exists()
            && let Err(error) = fs::write(light, "0")
        {
            log!("turning {light} off: {error}");
        }
    }
    if let Err(error) = fs::remove_file(dir.join(BOOT_STARTED))
        && error.kind() != io::ErrorKind::NotFound
    {
        log!("removing {BOOT_STARTED}: {error}");
    }
    sys::sync();
}

/// Logs how a sync or pairing went, once it ends. Never the pairing code.
fn log_sync(
    session: &Session<File>,
    job_started: &mut Option<Instant>,
    last_logged: &mut Option<u64>,
) {
    if session.syncing() {
        job_started.get_or_insert_with(Instant::now);
        return;
    }
    let took = job_started.take().map(|started| started.elapsed());
    if session.last_sync_at() != *last_logged {
        *last_logged = session.last_sync_at();
        let said = session.last_sync().unwrap_or_default();
        match took {
            Some(took) => log!("sync ended after {took:?}: {said}"),
            None => log!("sync ended: {said}"),
        }
    }
}

/// Whether the USB cable goes to a computer, rather than a charger or
/// nothing.
fn plugged_into_computer() -> bool {
    fs::read_to_string(EXTCON).is_ok_and(|state| to_computer(&state))
}

fn to_computer(state: &str) -> bool {
    state
        .lines()
        .any(|line| matches!(line.trim(), "SDP=1" | "CDP=1"))
}

/// Reads what's waiting on a non-blocking socket, to throw it away.
fn drain(mut socket: &File) {
    let mut buffer = [0u8; 4096];
    while socket.read(&mut buffer).is_ok_and(|n| n > 0) {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_computer_is_sdp_or_cdp_and_a_charger_isnt() {
        let state = |sdp, dcp, cdp| format!("USB=1\nUSB_HOST=0\nSDP={sdp}\nDCP={dcp}\nCDP={cdp}\n");
        assert!(to_computer(&state(1, 0, 0)));
        assert!(to_computer(&state(0, 0, 1)));
        assert!(!to_computer(&state(0, 1, 0)), "a charger");
        assert!(!to_computer(&state(0, 0, 0)));
    }
}
