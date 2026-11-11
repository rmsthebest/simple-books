//! Setting the clock from the network when it's plainly wrong. A battery
//! that ran flat leaves the Kobo's RTC starting again from 2000, and HTTPS
//! then fails, since every certificate looks not yet valid. Nickel sets the
//! clock itself; in its place, once Wi-Fi is up, busybox's `ntpd` does,
//! and the RTC (kept in UTC, as `/etc/adjtime` says) is set from it.

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

use crate::log;

/// A clock earlier than this is wrong: 2026-11-01, before this reader ran
/// on any Kobo.
const EARLIEST: u64 = 1_793_491_200;
const NTPD: &str = "/usr/sbin/ntpd";
const HWCLOCK: &str = "/sbin/hwclock";
const TIME_SERVER: &str = "pool.ntp.org";
/// `ntpd -q` keeps trying until it hears back, so it's stopped after this.
const NTPD_LIMIT: Duration = Duration::from_secs(20);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn is_wrong(clock: u64) -> bool {
    clock < EARLIEST
}

/// With Wi-Fi up: sets the clock, and the RTC, if the clock is wrong.
/// Otherwise does nothing. Whatever happens is logged; a sync that then
/// fails says why itself.
pub(crate) fn fix_if_wrong() {
    let before = now();
    if !is_wrong(before) {
        return;
    }
    log!("the clock says {before}, which is wrong: asking {TIME_SERVER}");
    let started = Instant::now();
    if let Err(why) = run_for(Command::new(NTPD).args(["-q", "-n", "-p", TIME_SERVER])) {
        log!("{NTPD}: {why}");
    }
    let after = now();
    if is_wrong(after) {
        log!("the clock is still wrong, after {:?}", started.elapsed());
        return;
    }
    log!("the clock is set, in {:?}", started.elapsed());
    if let Err(why) = run_for(Command::new(HWCLOCK).args(["--systohc", "--utc"])) {
        log!("{HWCLOCK}: {why}");
    }
}

/// Runs `command` with nothing for its input or output, stopping it after
/// [`NTPD_LIMIT`].
fn run_for(command: &mut Command) -> Result<(), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(status.to_string()),
            Ok(None) if started.elapsed() < NTPD_LIMIT => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer in {NTPD_LIMIT:?}"));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_from_before_this_reader_is_wrong() {
        // What the RTC starts from with a flat battery.
        assert!(is_wrong(946_684_800));
        assert!(!is_wrong(EARLIEST));
        assert!(!is_wrong(now()), "this machine's clock is right");
    }

    #[test]
    fn a_command_says_whether_it_worked() {
        assert!(run_for(&mut Command::new("true")).is_ok());
        assert!(run_for(&mut Command::new("false")).is_err());
        assert!(run_for(&mut Command::new("no-such-command-here")).is_err());
    }
}
