//! The Clara BW as the session's [`Device`]: the panel through the
//! framebuffer, the battery and light through sysfs, sleep the way KOReader
//! does it on MediaTek, and files in the app's folder on the user storage.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use reader_app::{Battery, Device, Light, Refresh};
use reader_core::Canvas;

use crate::{
    log, online,
    sys::{self, Framebuffer, Waveform},
};

/// Where `boot-install.sh` adds its line.
const RC_LOCAL: &str = "/etc/rc.local";
/// How that line ends.
const BOOT_MARK: &str = "# simple-books";

const BATTERY: &str = "/sys/class/power_supply/battery/capacity";
const CHARGER: &str = "/sys/class/power_supply/ac/online";
/// 0–100.
const BRIGHTNESS: &str = "/sys/class/backlight/mxc_msp430.0/brightness";
/// 0–10, warmest at 0; the session's warmth is the other way round, as
/// KOReader shows it.
const COLOR: &str = "/sys/class/backlight/lm3630a_led/color";
const STATE_EXTENDED: &str = "/sys/power/state-extended";
const STATE: &str = "/sys/power/state";

pub(crate) struct Kobo {
    pub framebuffer: Framebuffer,
    /// Where the app keeps its files.
    dir: PathBuf,
    /// Nickel keeps `/etc/localtime` linked to the zone chosen in its
    /// settings.
    zone: jiff::tz::TimeZone,
}

impl std::fmt::Debug for Kobo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kobo")
            .field("framebuffer", &self.framebuffer)
            .field("dir", &self.dir)
            .field("zone", &self.zone.iana_name())
            .finish()
    }
}

impl Kobo {
    pub(crate) fn new(framebuffer: Framebuffer, dir: PathBuf) -> Self {
        let zone = jiff::tz::TimeZone::try_system().unwrap_or_else(|error| {
            log!("no time zone, so UTC: {error}");
            jiff::tz::TimeZone::UTC
        });
        Self {
            framebuffer,
            dir,
            zone,
        }
    }

    fn charging() -> bool {
        read_number(CHARGER) == Some(1)
    }
}

impl Device for Kobo {
    fn show(&mut self, screen: &Canvas, refresh: Refresh) {
        // Don't write over a page the panel is still drawing.
        if let Err(error) = self.framebuffer.wait() {
            log!("waiting for the panel: {error}");
        }
        let fb = &mut self.framebuffer;
        let (width, height, stride) = (fb.width as usize, fb.height as usize, fb.stride as usize);
        if screen.width() as usize != width || screen.height() as usize != height {
            log!(
                "a {}×{} screen for a {width}×{height} panel",
                screen.width(),
                screen.height()
            );
            return;
        }
        let source = screen.pixels();
        let target = fb.pixels();
        for (row, line) in source.chunks_exact(width).enumerate() {
            target[row * stride..row * stride + width].copy_from_slice(line);
        }
        let waveform = match refresh {
            Refresh::Partial => Waveform::Gl16,
            Refresh::Full => Waveform::Gc16,
        };
        if let Err(error) = fb.update(waveform) {
            log!("updating the panel: {error}");
        }
    }

    fn battery(&mut self) -> Option<Battery> {
        let percent = read_number(BATTERY)?;
        Some(Battery {
            percent: percent.min(100) as u8,
            charging: Self::charging(),
        })
    }

    fn light(&mut self) -> Light {
        Light {
            brightness: read_number(BRIGHTNESS).unwrap_or(0).min(100) as u8,
            warmth: 10 - read_number(COLOR).unwrap_or(10).min(10) as u8,
        }
    }

    fn set_light(&mut self, light: Light) {
        let color = 10 - light.warmth.min(10);
        for (path, value) in [(BRIGHTNESS, light.brightness.min(100)), (COLOR, color)] {
            if let Err(error) = fs::write(path, value.to_string()) {
                log!("{path}: {error}");
            }
        }
    }

    fn suspend(&mut self) -> bool {
        // On MediaTek, suspending while plugged in hangs the kernel.
        if Self::charging() {
            return false;
        }
        // The sleep screen has to be finished before the panel loses power.
        let _ = self.framebuffer.wait();
        if let Err(error) = fs::write(STATE_EXTENDED, "1") {
            log!("{STATE_EXTENDED}: {error}");
            return false;
        }
        std::thread::sleep(Duration::from_secs(2));
        sys::sync();
        // Blocks until the device wakes.
        let slept = fs::write(STATE, "mem");
        if let Err(error) = &slept {
            log!("{STATE}: {error}");
        }
        if let Err(error) = fs::write(STATE_EXTENDED, "0") {
            log!("{STATE_EXTENDED}: {error}");
        }
        log!("woke");
        slept.is_ok()
    }

    fn load(&mut self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.dir.join(name)).ok()
    }

    fn clock(&mut self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }

    fn utc_offset(&mut self, clock: u64) -> i32 {
        utc_offset(&self.zone, clock)
    }

    /// On when `boot` is in the app folder and `/etc/rc.local` runs
    /// `boot.sh`, as `boot-install.sh on` leaves it.
    fn starts_reader(&mut self) -> Option<bool> {
        let line = std::fs::read_to_string(RC_LOCAL)
            .is_ok_and(|rc| rc.lines().any(|l| l.ends_with(BOOT_MARK)));
        Some(line && self.dir.join("boot").exists())
    }

    fn set_starts_reader(&mut self, on: bool) -> Result<String, String> {
        let script = self.dir.join("boot-install.sh");
        let result = online::run(&script, if on { "on" } else { "off" });
        let one_line = |said: String| said.lines().collect::<Vec<_>>().join(" ");
        match result {
            Ok(said) => {
                log!("boot-install.sh: {}", one_line(said.clone()));
                Ok(one_line(said))
            }
            Err(said) => {
                log!("boot-install.sh failed: {}", one_line(said.clone()));
                Err(one_line(said))
            }
        }
    }

    fn update(&mut self) -> Result<(), String> {
        crate::update::apply(&self.dir)
    }

    fn save(&mut self, name: &str, bytes: &[u8]) {
        if let Err(error) = save_atomically(&self.dir.join(name), bytes) {
            log!("saving {name}: {error}");
        }
    }
}

/// Writes a new file beside the old one and renames it over, so a crash or
/// a flat battery leaves either the old file or the new one, never half.
fn save_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

fn read_number(path: &str) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// How far ahead of UTC `zone` is at `clock`, in seconds.
fn utc_offset(zone: &jiff::tz::TimeZone, clock: u64) -> i32 {
    i64::try_from(clock)
        .ok()
        .and_then(|clock| jiff::Timestamp::from_second(clock).ok())
        .map_or(0, |at| zone.to_offset(at).seconds())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_replaces_the_file_whole() {
        let dir = std::env::temp_dir().join(format!("reader-kobo-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        save_atomically(&path, b"first").unwrap();
        save_atomically(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert!(!path.with_extension("tmp").exists());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
}
