//! Writes what the session shows (the home screen, a page, the menu, the
//! library, search, settings, sync and the sleep screen) as PGM images, to
//! look at them without the device. A stand-in server on a local port answers
//! the sync screen.
//!
//! ```sh
//! cargo run -p reader-app --example screens -- books-dir out-dir
//! ```
//!
//! The first book by name is opened.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use reader_app::{
    AlwaysOnline, Battery, Books, Device, Event, Folder, Light, Refresh, Session, Ssh, SshLogin,
    SyncSetup,
};
use reader_core::Canvas;

struct Recorder {
    out: PathBuf,
    /// What to call the next screen shown.
    name: &'static str,
    files: HashMap<String, Vec<u8>>,
    clock: u64,
    starts_reader: bool,
}

impl Device for Recorder {
    fn show(&mut self, screen: &Canvas, refresh: Refresh) {
        let name = self.name;
        let mut pgm = format!("P5\n{} {}\n255\n", screen.width(), screen.height()).into_bytes();
        pgm.extend_from_slice(screen.pixels());
        let path = self.out.join(format!("{name}.pgm"));
        fs::write(&path, pgm).unwrap();
        println!("{} ({refresh:?})", path.display());
    }
    fn battery(&mut self) -> Option<Battery> {
        Some(Battery {
            percent: 84,
            charging: false,
        })
    }
    fn light(&mut self) -> Light {
        Light {
            brightness: 30,
            warmth: 4,
        }
    }
    fn set_light(&mut self, _: Light) {}
    fn suspend(&mut self) -> bool {
        false
    }
    fn load(&mut self, name: &str) -> Option<Vec<u8>> {
        self.files.get(name).cloned()
    }
    fn save(&mut self, name: &str, bytes: &[u8]) {
        self.files.insert(name.to_owned(), bytes.to_vec());
    }
    fn clock(&mut self) -> u64 {
        self.clock += 60;
        self.clock
    }
    fn starts_reader(&mut self) -> Option<bool> {
        Some(self.starts_reader)
    }
    fn set_starts_reader(&mut self, on: bool) -> Result<String, String> {
        self.starts_reader = on;
        Ok("Simple Books will start when the Kobo is switched on.".to_owned())
    }
}

/// Sends events a second apart, past the turn guard, naming the screen each
/// one shows.
struct Run {
    session: Session<fs::File>,
    device: Recorder,
    now: Instant,
}

impl Run {
    fn send(&mut self, name: &'static str, event: Event) {
        self.now += Duration::from_secs(1);
        self.device.name = name;
        self.session
            .handle(event, self.now, &mut self.device)
            .unwrap();
    }

    fn tap(&mut self, name: &'static str, x: f32, y: f32) {
        self.send(name, Event::Tap { x, y });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let folder = Folder::new(&args[0]);
    let out = PathBuf::from(&args[1]);
    fs::create_dir_all(&out).unwrap();
    let first = folder
        .list()
        .first()
        .expect("a .book in the folder")
        .name
        .clone();
    let mut device = Recorder {
        out,
        name: "home-new",
        files: HashMap::new(),
        clock: 1_700_000_000,
        starts_reader: false,
    };
    let now = Instant::now();
    let session = Session::start(Arc::new(folder), &mut device, now).unwrap();
    let mut run = Run {
        session,
        device,
        now,
    };
    run.device.name = "page";
    run.session.open(&first, &mut run.device, run.now).unwrap();
    for _ in 0..3 {
        run.tap("page", 800.0, 900.0);
    }
    run.tap("menu", 536.0, 60.0);
    // The menu's Home button.
    run.tap("home", 150.0, 780.0);
    // Library, then sorted by series.
    run.tap("settings", 471.0, 1376.0);
    // Line spacing up, then Done.
    let started = Instant::now();
    run.tap("settings-spacing", 967.0, 590.0);
    println!("settings-spacing: drawn in {:?}", started.elapsed());
    sync_screens(&mut run);
    // The Kobo screen, starting here turned on, then back.
    run.tap("kobo", 536.0, 1376.0);
    run.tap("kobo-on", 536.0, 482.0);
    run.tap("kobo-uninstall", 536.0, 878.0);
    run.tap("settings-back", 536.0, 1376.0);
    run.tap("home", 776.0, 1376.0);
    run.tap("library", 194.0, 1376.0);
    run.tap("library-series", 656.0, 172.0);
    run.tap("search", 896.0, 172.0);
    // h, then j: the second row's sixth and seventh keys.
    let unit = (944.0 - 10.0 * 12.0) / 11.0;
    for (i, name) in [(5.0, "search-h"), (6.0, "search-hj")] {
        let x = 64.0 + i * (unit + 12.0) + unit / 2.0;
        let started = Instant::now();
        run.tap(name, x, 1112.0 + 48.0);
        println!("{name}: drawn in {:?}", started.elapsed());
    }
    // Plugged into a computer, then Not now.
    run.send("usb", Event::Computer(true));
    run.tap("search-hj", 296.0, 1376.0);
    // The power button held, then Cancel.
    run.send("power", Event::PowerHeld);
    run.tap("search-hj", 536.0, 968.0);
    run.send("sleep", Event::Power);
    // Held again on the sleep screen, then Power off.
    run.send("power", Event::PowerHeld);
    run.tap("switched-off", 536.0, 552.0);
}

/// From settings: the sync screen unpaired, showing a code, and paired.
/// Ends back in settings.
fn sync_screens(run: &mut Run) {
    let dir = run.device.out.join("sync");
    fs::create_dir_all(&dir).unwrap();
    let setup = SyncSetup {
        dir: dir.clone(),
        server: Some(stand_in_server()),
        name: "Kobo Clara BW".to_owned(),
        network: Arc::new(AlwaysOnline),
        ssh: Some(Arc::new(StandInSsh)),
    };
    run.session.enable_sync(setup, &mut run.device, run.now);
    run.tap("sync-unpaired", 296.0, 1376.0);
    run.tap("sync-asking", 200.0, 1376.0);
    while run.session.pairing_code().is_none() {
        wait_and_tick(run, "sync-code");
    }
    run.tap("sync-stopping", 200.0, 1376.0);
    while run.session.syncing() {
        wait_and_tick(run, "sync-stopped");
    }
    // As if the code had been typed on the website.
    fs::write(dir.join("sync-secret"), "secret").unwrap();
    run.tap("settings-again", 776.0, 1376.0);
    run.tap("sync-paired", 296.0, 1376.0);
    run.tap("sync-syncing", 200.0, 1376.0);
    while run.session.syncing() {
        wait_and_tick(run, "sync-synced");
    }
    // SSH on, then off again.
    run.tap("sync-ssh-starting", 536.0, 1256.0);
    while run.session.ssh_login().is_none() {
        wait_and_tick(run, "sync-ssh-on");
    }
    run.tap("sync-ssh-off", 536.0, 1256.0);
    run.tap("settings-back", 776.0, 1376.0);
}

struct StandInSsh;

impl Ssh for StandInSsh {
    fn start(&self) -> Result<SshLogin, String> {
        Ok(SshLogin {
            command: "ssh -p 2222 root@192.168.1.50".to_owned(),
            password: Some("kq7m-x3fh".to_owned()),
        })
    }

    fn stop(&self) {}
}

fn wait_and_tick(run: &mut Run, name: &'static str) {
    std::thread::sleep(Duration::from_millis(20));
    run.send(name, Event::Tick);
}

/// Answers pairing with a code that's never typed, and changes with nothing
/// new. Returns its address.
fn stand_in_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let body = if request.starts_with("POST /device/v1/pair") {
                r#"{"secret":"s","code":"HJKM-7QRT","url":"https://books.example.com/device","expires_in":900,"interval":1}"#
            } else if request.starts_with("GET /device/v1/pair") {
                r#"{"state":"pending"}"#
            } else {
                r#"{"books":[],"removed":[],"places":[],"next":0,"more":false,"full":true}"#
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{address}")
}
