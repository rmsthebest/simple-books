//! Going online to sync: Wi-Fi and SSH through the scripts beside the
//! binary (`wifi.sh`, `ssh.sh`), and which server to sync with.

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use reader_app::{Network, Ssh};

use crate::log;

/// Nickel's settings, which say which server the Kobo syncs with.
const NICKEL_CONF: &str = "/mnt/onboard/.kobo/Kobo/Kobo eReader.conf";
/// The server's address, put in the app folder by hand. Used first.
const SERVER_FILE: &str = "server.txt";
/// The longest a script may take. `wifi.sh up` gives up after about a
/// minute.
const RUN_LIMIT: Duration = Duration::from_secs(180);

/// Wi-Fi, on while anything needs it: a sync, a pairing, or SSH.
pub(crate) struct Wifi {
    script: PathBuf,
    /// How many [`Network::up`]s haven't been matched by a
    /// [`Network::down`]. Held while the script runs, so a second job
    /// waits for the first to connect rather than starting the script
    /// again.
    users: Mutex<u32>,
}

impl Wifi {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            script: dir.join("wifi.sh"),
            users: Mutex::new(0),
        }
    }
}

impl Network for Wifi {
    fn up(&self) -> Result<(), String> {
        let mut users = self.users.lock().unwrap_or_else(PoisonError::into_inner);
        if *users == 0 {
            let started = Instant::now();
            if let Err(why) = run(&self.script, "up") {
                log!("Wi-Fi didn't come up: {why}");
                return Err(why);
            }
            log!("Wi-Fi up in {:?}", started.elapsed());
        }
        *users += 1;
        Ok(())
    }

    fn down(&self) {
        let mut users = self.users.lock().unwrap_or_else(PoisonError::into_inner);
        *users = users.saturating_sub(1);
        if *users == 0 {
            if let Err(why) = run(&self.script, "down") {
                log!("Wi-Fi didn't go down: {why}");
            }
            log!("Wi-Fi down");
        }
    }
}

/// KOReader's dropbear, as the Nickelite SSH entries run it.
pub(crate) struct KoboSsh {
    script: PathBuf,
    wifi: PathBuf,
}

impl KoboSsh {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            script: dir.join("ssh.sh"),
            wifi: dir.join("wifi.sh"),
        }
    }
}

impl Ssh for KoboSsh {
    fn start(&self) -> Result<String, String> {
        // dropbear keeps whatever it's given as output, so it gets nothing
        // and only the exit status is read.
        let started = Command::new(&self.script)
            .arg("start")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !started {
            log!("SSH didn't start");
            return Err("It needs KOReader installed, for its SSH server.".to_owned());
        }
        let address = run(&self.wifi, "address").unwrap_or_default();
        let address = address.trim();
        log!("SSH on at {address}");
        if address.is_empty() {
            Ok("ssh -p 2222 root@<the Kobo's address>".to_owned())
        } else {
            Ok(format!("ssh -p 2222 root@{address}"))
        }
    }

    fn stop(&self) {
        let _ = Command::new(&self.script)
            .arg("stop")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        log!("SSH off");
    }
}

/// Runs `script action`. Returns what it printed, or the line it printed
/// to say why it failed.
///
/// What it prints goes to a file, not a pipe: a daemon it starts in the
/// background could hold a pipe open, and reading to the end would wait
/// for that daemon to exit. It's stopped after [`RUN_LIMIT`].
fn run(script: &Path, action: &str) -> Result<String, String> {
    static RUNS: AtomicU32 = AtomicU32::new(0);
    let n = RUNS.fetch_add(1, Ordering::Relaxed);
    let out_path =
        std::env::temp_dir().join(format!("simple-books-{}-{n}.out", std::process::id()));
    let couldnt = |error: std::io::Error| format!("Couldn't run {}: {error}", script.display());
    let out = File::create(&out_path).map_err(couldnt)?;
    let mut child = Command::new(script)
        .arg(action)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(Stdio::null())
        .spawn()
        .map_err(couldnt)?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < RUN_LIMIT => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("{} {action} took too long.", script.display()));
            }
            Err(error) => break Err(couldnt(error)),
        }
    };
    let said = fs::read_to_string(&out_path).unwrap_or_default();
    let _ = fs::remove_file(&out_path);
    let said = said.trim().to_owned();
    match status? {
        status if status.success() => Ok(said),
        status if said.is_empty() => Err(format!("Wi-Fi failed ({status}).")),
        _ => Err(said),
    }
}

/// The server to sync with: from `server.txt` in the app folder, or else
/// the one Nickel syncs with. Only the address is kept, never the token
/// in Nickel's.
pub(crate) fn server(dir: &Path) -> Option<String> {
    let found = std::fs::read_to_string(dir.join(SERVER_FILE))
        .ok()
        .and_then(|text| from_server_file(&text))
        .or_else(|| {
            std::fs::read(NICKEL_CONF)
                .ok()
                .and_then(|bytes| from_nickel_conf(&String::from_utf8_lossy(&bytes)))
        });
    match &found {
        Some(server) => log!("syncs with {server}"),
        None => log!("no server known: not connected in Nickel, and no {SERVER_FILE}"),
    }
    found
}

fn from_server_file(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    web_address(line.trim_end_matches('/'))
}

/// The server in Nickel's `api_endpoint`, as the website's Kobo page sets
/// it: `https://books.example.com/kobo/<token>`. Kobo's own store doesn't
/// count.
fn from_nickel_conf(text: &str) -> Option<String> {
    let endpoint = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("api_endpoint="))?;
    let (server, _token) = endpoint.trim().split_once("/kobo/")?;
    web_address(server)
}

/// `address` if it's just a scheme and a host.
fn web_address(address: &str) -> Option<String> {
    let host = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))?;
    (!host.is_empty() && !host.contains(['/', ' ', '?', '#'])).then(|| address.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_comes_from_nickels_endpoint_without_the_token() {
        let conf = "[OneStoreServices]\nx=1\napi_endpoint=https://books.example.com/kobo/abc123\n";
        assert_eq!(
            from_nickel_conf(conf).as_deref(),
            Some("https://books.example.com")
        );
        assert_eq!(
            from_nickel_conf("api_endpoint=https://storeapi.kobo.com\n"),
            None,
            "Kobo's own store"
        );
        assert_eq!(from_nickel_conf("nothing=here\n"), None);
    }

    #[test]
    fn a_server_file_holds_just_the_address() {
        assert_eq!(
            from_server_file("\n  http://192.168.1.10:3000/ \n").as_deref(),
            Some("http://192.168.1.10:3000")
        );
        assert_eq!(from_server_file("books.example.com"), None);
        assert_eq!(from_server_file("https://b.example.com/kobo/x"), None);
    }
}
