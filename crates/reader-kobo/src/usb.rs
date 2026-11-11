//! `reader-kobo --usb`: shares the Kobo's storage with the computer it's
//! plugged into, as Nickel does, with the firmware's own
//! `/etc/init.d/usb-gadget`. `reader.sh` and `boot.sh` run it when the
//! reader exits with [`EXIT_USB`], then start the reader again.
//!
//! `usb-gadget start storage` turns swap off, unmounts `/mnt/onboard` and
//! gives its partition to the USB gadget; `stop storage` takes it back,
//! checks it with `dosfsck` and mounts it again. Unmounting fails while
//! anything uses `/mnt/onboard`, so this runs from a copy in `/tmp`, made
//! by the scripts, from `/`, and keeps what it would log until the drive
//! is back.
//!
//! The computer has the drive until it ejects it (the gadget's backing
//! file goes empty) or the cable comes out.

use std::{
    fs,
    os::fd::AsFd,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use reader_app::{Device, Refresh, UsbScreen, usb_screen};

use crate::{device::Kobo, drain, log, open_log, plugged_into_computer, sys, sys::Framebuffer};

/// The reader's exit status asking for USB.
pub(crate) const EXIT_USB: i32 = 90;
const GADGET: &str = "/etc/init.d/usb-gadget";
/// The partition the computer gets, while it has it; empty once ejected.
const BACKING_FILE: &str = "/sys/kernel/config/usb_gadget/kobo/functions/mass_storage.0/lun.0/file";
const ONBOARD: &str = "/mnt/onboard";
/// How often to look at the backing file, for an eject.
const CHECK: Duration = Duration::from_secs(1);
/// How long the failure stays up before the reader comes back.
const FAILED_FOR: Duration = Duration::from_secs(4);

pub(crate) fn run(dir: &Path) {
    let mut said: Vec<String> = Vec::new();
    let exe = std::env::current_exe().unwrap_or_default();
    if exe.starts_with(ONBOARD) {
        open_log(dir);
        log!(
            "usb: running from {}, which would keep the drive busy",
            exe.display()
        );
        return;
    }
    let mut kobo = match Framebuffer::open() {
        Ok(framebuffer) => Some(Kobo::new(framebuffer, dir.to_owned())),
        Err(error) => {
            said.push(format!("usb: no screen: {error}"));
            None
        }
    };
    let mut show = |screen: UsbScreen| {
        if let Some(kobo) = kobo.as_mut() {
            kobo.show(&usb_screen(screen), Refresh::Full);
        }
    };
    show(UsbScreen::Connecting);
    sys::sync();
    let started = Instant::now();
    let gadget_started = gadget("start", &mut said);
    if !gadget_started || mounted() {
        said.push("usb: the drive couldn't be handed over; see usb-gadget in syslog".into());
        // Clears what the start set up; it leaves a mounted drive alone.
        gadget("stop", &mut said);
        show(UsbScreen::Failed);
        std::thread::sleep(FAILED_FOR);
        finish(dir, &said);
        return;
    }
    said.push(format!("usb: connected in {:?}", started.elapsed()));
    show(UsbScreen::Connected);
    let why = wait_for_the_end();
    said.push(format!("usb: {why}"));
    show(UsbScreen::Checking);
    let checking = Instant::now();
    gadget("stop", &mut said);
    said.push(format!("usb: drive back in {:?}", checking.elapsed()));
    finish(dir, &said);
}

/// Logs what was kept, now the drive's mounted again.
fn finish(dir: &Path, said: &[String]) {
    if !mounted() {
        // usb-gadget reboots if it can't mount it again; nowhere to log.
        return;
    }
    open_log(dir);
    for line in said {
        log!("{line}");
    }
}

/// `usb-gadget <action> storage`. Whether it worked.
fn gadget(action: &str, said: &mut Vec<String>) -> bool {
    let status = Command::new(GADGET)
        .args([action, "storage"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => true,
        Ok(status) => {
            said.push(format!("usb: usb-gadget {action} storage: {status}"));
            false
        }
        Err(error) => {
            said.push(format!("usb: usb-gadget {action} storage: {error}"));
            false
        }
    }
}

fn mounted() -> bool {
    fs::read_to_string("/proc/mounts").is_ok_and(|mounts| {
        mounts
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(ONBOARD))
    })
}

/// Waits until the computer ejects the drive or the cable comes out, and
/// says which.
fn wait_for_the_end() -> &'static str {
    let events = sys::uevents().ok();
    loop {
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
            return "unplugged";
        }
        if ejected(&fs::read_to_string(BACKING_FILE).unwrap_or_default()) {
            return "ejected on the computer";
        }
    }
}

/// Whether the gadget's backing file, as read, says the computer ejected
/// the drive: it's emptied then.
fn ejected(backing_file: &str) -> bool {
    backing_file.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_backing_file_is_an_eject() {
        assert!(ejected("\n"));
        assert!(ejected(""));
        assert!(!ejected("/dev/disk/by-partlabel/userdata\n"));
    }
}
