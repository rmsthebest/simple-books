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
    log,
    sys::{self, Framebuffer, Waveform},
};

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
}

impl std::fmt::Debug for Kobo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kobo")
            .field("framebuffer", &self.framebuffer)
            .field("dir", &self.dir)
            .finish()
    }
}

impl Kobo {
    pub(crate) fn new(framebuffer: Framebuffer, dir: PathBuf) -> Self {
        Self { framebuffer, dir }
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
