//! `reader-kobo --in-nickel <pid>`: what runs beside Nickel, as root, once
//! our plugin (`crates/kobo-plugin`) has been loaded into it.
//!
//! It does what's been asked for with a file in the app folder, each removed
//! before it's done, and then exits:
//!
//! - `uninstall`: Simple Books is to go. Puts `/etc/rc.local` back with
//!   `boot-install.sh off` and removes the plugin. The app folder is left,
//!   for deleting over USB.
//! - `install-boot`: start the reader when the Kobo is switched on, with
//!   `boot-install.sh on`. The website's installer asks for this.
//! - `start-reader`: start the reader once Nickel is up. The installer asks
//!   for this too, so the reader is the first thing seen after ejecting.
//! - `usb-return`: the reader handed over to Nickel for USB. Once the
//!   computer is unplugged, the reader starts again.
//!
//! With nothing asked for it exits at once. The plugin starts it in every
//! process that loads image plugins, so it checks that `<pid>` is Nickel's
//! first.

use std::{
    fs::{self, File},
    os::fd::AsFd,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crate::{drain, log, online, plugged_into_computer, sys};

pub(crate) const UNINSTALL: &str = "uninstall";
const INSTALL_BOOT: &str = "install-boot";
const START_READER: &str = "start-reader";
pub(crate) const USB_RETURN: &str = "usb-return";
/// Where the Kobo keeps our plugin.
const PLUGIN: &str = "/usr/local/Kobo/imageformats/libsimplebooks.so";
/// Held while one of these runs: a Nickel restarted quickly could otherwise
/// get two.
const LOCK: &str = "/tmp/simple-books-in-nickel.lock";
/// How long to let Nickel settle before stopping it for the reader.
const SETTLE: Duration = Duration::from_secs(5);
/// How long after unplugging before the reader starts, so Nickel has
/// finished with the drive.
const AFTER_UNPLUG: Duration = Duration::from_secs(3);
/// How often to check Nickel is still there, while waiting.
const CHECK: Duration = Duration::from_secs(10);

pub(crate) fn run(dir: &Path, nickel: &str) {
    let comm = fs::read_to_string(format!("/proc/{nickel}/comm")).unwrap_or_default();
    if comm.trim() != "nickel" {
        log!(
            "in-nickel: {nickel} is {:?}, not Nickel: nothing to do",
            comm.trim()
        );
        return;
    }
    let Some(_lock) = lock() else {
        log!("in-nickel: another one is running");
        return;
    };
    if take(dir, UNINSTALL) {
        uninstall(dir);
        return;
    }
    if take(dir, INSTALL_BOOT) {
        match online::run(&dir.join("boot-install.sh"), "on") {
            Ok(said) => log!("in-nickel: boot-install.sh on: {}", one_line(&said)),
            Err(said) => log!("in-nickel: boot-install.sh on failed: {}", one_line(&said)),
        }
    }
    if take(dir, START_READER) {
        std::thread::sleep(SETTLE);
        start_reader(dir);
        return;
    }
    if dir.join(USB_RETURN).exists() {
        if plugged_into_computer() {
            back_after_usb(dir, nickel);
        } else {
            // Left from a hand-over that ended some other way, such as a
            // restart.
            take(dir, USB_RETURN);
        }
    }
}

/// Removes `name` from the app folder, saying whether it was there.
fn take(dir: &Path, name: &str) -> bool {
    fs::remove_file(dir.join(name)).is_ok()
}

fn one_line(said: &str) -> String {
    said.lines().collect::<Vec<_>>().join(" ")
}

/// An exclusive lock on [`LOCK`], waiting up to 15 s for one left by the
/// last Nickel to go.
fn lock() -> Option<File> {
    let file = File::create(LOCK).ok()?;
    let started = Instant::now();
    loop {
        if sys::try_lock(&file) {
            return Some(file);
        }
        if started.elapsed() > Duration::from_secs(15) {
            return None;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn uninstall(dir: &Path) {
    match online::run(&dir.join("boot-install.sh"), "off") {
        Ok(said) => log!("in-nickel: boot-install.sh off: {}", one_line(&said)),
        Err(said) => log!("in-nickel: boot-install.sh off failed: {}", one_line(&said)),
    }
    match fs::remove_file(PLUGIN) {
        Ok(()) => log!("in-nickel: removed the plugin; Simple Books is uninstalled"),
        Err(error) => log!("in-nickel: removing the plugin: {error}"),
    }
    sys::sync();
}

/// Starts `reader.sh`, which stops Nickel, runs the reader and brings
/// Nickel back after. In its own session, so it outlives this.
fn start_reader(dir: &Path) {
    let script = dir.join("reader.sh");
    let mut command = Command::new(&script);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    sys::own_session(&mut command);
    match command.spawn() {
        Ok(_) => log!("in-nickel: starting the reader"),
        Err(error) => log!("in-nickel: starting {}: {error}", script.display()),
    }
}

/// Waits for the computer to be unplugged, then starts the reader. Gives up
/// if Nickel goes away.
fn back_after_usb(dir: &Path, nickel: &str) {
    log!("in-nickel: back to the reader once the computer is unplugged");
    let events = match sys::uevents() {
        Ok(events) => Some(events),
        Err(error) => {
            log!("in-nickel: no device events, so checking every 10 s: {error}");
            None
        }
    };
    let alive = || Path::new(&format!("/proc/{nickel}")).exists();
    while alive() {
        match &events {
            Some(events) => {
                if sys::poll(&[events.as_fd()], CHECK).is_err() {
                    std::thread::sleep(CHECK);
                }
                drain(events);
            }
            None => std::thread::sleep(CHECK),
        }
        if !plugged_into_computer() {
            take(dir, USB_RETURN);
            std::thread::sleep(AFTER_UNPLUG);
            start_reader(dir);
            return;
        }
    }
    log!("in-nickel: Nickel stopped");
}
