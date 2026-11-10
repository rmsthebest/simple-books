//! Writes what the session shows (the home screen, a page, the menu and the
//! sleep screen) as PGM images, to look at them without the device.
//!
//! ```sh
//! cargo run -p reader-app --example screens -- books-dir out-dir
//! ```
//!
//! The first book by name is opened.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use reader_app::{Battery, Books, Device, Event, Folder, Light, Refresh, Session};
use reader_core::Canvas;

struct Recorder {
    out: PathBuf,
    /// What to call the next screen shown.
    name: &'static str,
    files: HashMap<String, Vec<u8>>,
    clock: u64,
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
    run.send("sleep", Event::Power);
}
