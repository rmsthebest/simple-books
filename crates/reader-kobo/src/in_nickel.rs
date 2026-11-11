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
//!   computer is unplugged, the reader starts again. Waiting for that is a
//!   copy of this program in `/tmp` (`--usb-wait`), with nothing open on
//!   `/mnt/onboard`: Nickel unmounts it to hand it to the computer, and
//!   can't while anything uses it.
//!
//! With nothing asked for it exits at once. The plugin starts it in every
//! process that loads image plugins, so it checks that `<pid>` is Nickel's
//! first.

use std::{
    fs::{self, File},
    os::{
        fd::AsFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crate::{close_log, drain, log, online, open_log, plugged_into_computer, sys};

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
/// The copy of this program that waits out USB, in memory rather than on
/// the drive the computer gets.
const WAITER: &str = "/tmp/simple-books-usb-wait";
/// How long after unplugging to wait for Nickel to mount the drive again.
const REMOUNT_LONGEST: Duration = Duration::from_secs(60);

pub(crate) fn run(dir: &Path, nickel: &str) {
    let comm = fs::read_to_string(format!("/proc/{nickel}/comm")).unwrap_or_default();
    if comm.trim() != "nickel" {
        log!(
            "in-nickel: {nickel} is {:?}, not Nickel: nothing to do",
            comm.trim()
        );
        return;
    }
    let Some(lock) = lock() else {
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
            back_after_usb(dir, nickel, &lock);
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

/// Becomes [`usb_wait`], run from a copy in `/tmp`, keeping `lock`. If
/// that can't be done, the reader won't come back on its own after USB, but
/// USB still works.
fn back_after_usb(dir: &Path, nickel: &str, lock: &File) {
    let copied = std::env::current_exe()
        .and_then(|exe| fs::copy(exe, WAITER))
        .and_then(|_| fs::set_permissions(WAITER, fs::Permissions::from_mode(0o755)))
        .and_then(|()| sys::keep_across_exec(lock));
    if let Err(error) = copied {
        log!("in-nickel: can't wait for USB to end from {WAITER}: {error}");
        take(dir, USB_RETURN);
        return;
    }
    log!("in-nickel: back to the reader once the computer is unplugged");
    close_log();
    let error = Command::new(WAITER)
        .arg("--usb-wait")
        .arg(nickel)
        .arg("--dir")
        .arg(dir)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .exec();
    open_log(dir);
    log!("in-nickel: starting {WAITER}: {error}");
    take(dir, USB_RETURN);
}

/// `reader-kobo --usb-wait <pid>`, from [`WAITER`]: waits for the computer
/// to be unplugged, touching nothing on `/mnt/onboard` meanwhile, not even
/// the log, then starts the reader once Nickel has the drive back. Gives up
/// if Nickel goes away. The lock from `--in-nickel` is still held.
pub(crate) fn usb_wait(dir: &Path, nickel: &str) {
    // It runs on without the file.
    let _ = fs::remove_file(WAITER);
    let unplugged = wait_for_unplug(nickel);
    let started = Instant::now();
    while !dir.join("reader.sh").exists() && started.elapsed() < REMOUNT_LONGEST {
        std::thread::sleep(Duration::from_secs(1));
    }
    open_log(dir);
    if !unplugged {
        log!("in-nickel: Nickel stopped while plugged into the computer");
        return;
    }
    log!("in-nickel: unplugged from the computer");
    take(dir, USB_RETURN);
    std::thread::sleep(AFTER_UNPLUG);
    start_reader(dir);
}

/// Waits until the computer's unplugged (true), or Nickel stops (false).
fn wait_for_unplug(nickel: &str) -> bool {
    // Without device events, it checks every 10 s.
    let events = sys::uevents().ok();
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
            return true;
        }
    }
    false
}
