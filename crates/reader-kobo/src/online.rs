//! Going online to sync: Wi-Fi through `wifi.sh` and SSH through
//! `simple-books-ssh`, both beside the binary, and which server to sync
//! with.

use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use reader_app::{Network, Ssh, SshLogin};

use crate::log;

/// Nickel's settings, which say which server the Kobo syncs with.
const NICKEL_CONF: &str = "/mnt/onboard/.kobo/Kobo/Kobo eReader.conf";
/// The server's address, put in the app folder by hand. Used first.
/// No extension: Nickel would list a `.txt` as a book.
const SERVER_FILE: &str = "server";
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
    /// Up and usable, for the status, which mustn't wait on `users` while
    /// the script runs.
    up: AtomicBool,
}

impl Wifi {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            script: dir.join("wifi.sh"),
            users: Mutex::new(0),
            up: AtomicBool::new(false),
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
            self.up.store(true, Ordering::Relaxed);
        }
        *users += 1;
        Ok(())
    }

    fn down(&self) {
        let mut users = self.users.lock().unwrap_or_else(PoisonError::into_inner);
        *users = users.saturating_sub(1);
        if *users == 0 {
            self.up.store(false, Ordering::Relaxed);
            if let Err(why) = run(&self.script, "down") {
                log!("Wi-Fi didn't go down: {why}");
            }
            log!("Wi-Fi down");
        }
    }

    fn is_up(&self) -> bool {
        self.up.load(Ordering::Relaxed)
    }
}

/// Our own SSH server, `simple-books-ssh`, beside the reader. It gets a
/// fresh password on its stdin and serves until stdin closes, so it stops
/// with the reader, however the reader stops.
pub(crate) struct KoboSsh {
    server: PathBuf,
    wifi: PathBuf,
    log: PathBuf,
    running: Mutex<Option<Child>>,
}

/// Where SSH listens.
const SSH_PORT: u16 = 2222;
/// Letters for passwords: none that look alike on the screen.
const PASSWORD_LETTERS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

impl KoboSsh {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            server: dir.join("simple-books-ssh"),
            wifi: dir.join("wifi.sh"),
            log: dir.join("reader.log"),
            running: Mutex::new(None),
        }
    }
}

/// Eight letters from `/dev/urandom`, as `abcd-efgh`.
fn password() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0; 8];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let letters: String = bytes
        .iter()
        .map(|b| char::from(PASSWORD_LETTERS[usize::from(*b) % PASSWORD_LETTERS.len()]))
        .collect();
    Ok(format!("{}-{}", &letters[..4], &letters[4..]))
}

impl Ssh for KoboSsh {
    fn start(&self) -> Result<SshLogin, String> {
        use std::io::Write;
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.is_some() {
            return Err("It's already on.".to_owned());
        }
        if !self.server.exists() {
            return Err("simple-books-ssh is missing. Install Simple Books again.".to_owned());
        }
        let password = password().map_err(|error| format!("No password: {error}"))?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_or_else(|_| Stdio::null(), Stdio::from);
        let dir = self.server.parent().unwrap_or(Path::new("/"));
        let mut child = Command::new(&self.server)
            .arg("--dir")
            .arg(dir)
            .arg("--port")
            .arg(SSH_PORT.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|error| format!("Couldn't start it: {error}"))?;
        let told = child
            .stdin
            .as_mut()
            .is_some_and(|stdin| writeln!(stdin, "{password}").is_ok());
        // It stops at once if it can't listen, such as with the port taken.
        std::thread::sleep(Duration::from_millis(300));
        if !told || !matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
            log!("SSH didn't start");
            return Err("It stopped at once; reader.log says why.".to_owned());
        }
        *running = Some(child);
        let address = run(&self.wifi, "address").unwrap_or_default();
        let address = address.trim();
        log!("SSH on at {address}");
        let host = if address.is_empty() {
            "<the Kobo's address>"
        } else {
            address
        };
        Ok(SshLogin {
            command: format!("ssh -p {SSH_PORT} root@{host}"),
            password: Some(password),
        })
    }

    fn stop(&self) {
        let Some(mut child) = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        // Its stdin closing is what stops it.
        drop(child.stdin.take());
        let asked = Instant::now();
        while matches!(child.try_wait(), Ok(None)) {
            if asked.elapsed() > Duration::from_secs(3) {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.wait();
        log!("SSH off");
    }
}

/// Runs `script action`. Returns what it printed, or the line it printed
/// to say why it failed.
///
/// What it prints goes to a file, not a pipe: a daemon it starts in the
/// background could hold a pipe open, and reading to the end would wait
/// for that daemon to exit. It's stopped after [`RUN_LIMIT`].
pub(crate) fn run(script: &Path, action: &str) -> Result<String, String> {
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

/// The server to sync with: from `server` in the app folder, or else
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

    #[test]
    fn a_password_is_eight_unambiguous_letters_and_fresh_each_time() {
        let (a, b) = (password().unwrap(), password().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 9);
        assert_eq!(&a[4..5], "-");
        assert!(
            a.bytes()
                .filter(|b| *b != b'-')
                .all(|b| PASSWORD_LETTERS.contains(&b))
        );
    }
}
