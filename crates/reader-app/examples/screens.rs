//! Writes what the session shows for a book (the page, the menu and the
//! sleep screen) as PGM images, to look at them without the device.
//!
//! ```sh
//! cargo run -p reader-app --example screens -- book.book out-dir
//! ```

use std::{collections::HashMap, fs, io::Cursor, path::PathBuf, sync::Arc, time::Instant};

use book_format::{BookReader, Limits};
use reader_app::{Battery, Device, Event, Light, Refresh, Session};
use reader_core::Canvas;

struct Recorder {
    out: PathBuf,
    names: Vec<&'static str>,
    files: HashMap<String, Vec<u8>>,
}

impl Device for Recorder {
    fn show(&mut self, screen: &Canvas, refresh: Refresh) {
        let name = self.names.first().copied().unwrap_or("extra");
        if !self.names.is_empty() {
            self.names.remove(0);
        }
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
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = Arc::new(fs::read(&args[0]).unwrap());
    let out = PathBuf::from(&args[1]);
    fs::create_dir_all(&out).unwrap();
    let mut device = Recorder {
        out,
        names: vec!["page", "menu", "sleep"],
        files: HashMap::new(),
    };
    let open: reader_app::Opener<Cursor<Vec<u8>>> =
        Arc::new(move || BookReader::open(Cursor::new(bytes.as_ref().clone()), Limits::default()));
    let now = Instant::now();
    let mut session = Session::start(open, 1, &mut device, now).unwrap();
    session
        .handle(Event::Tap { x: 536.0, y: 60.0 }, now, &mut device)
        .unwrap();
    // Close the menu by tapping below it, which shows the page again.
    device.names.insert(0, "closed");
    session
        .handle(
            Event::Tap {
                x: 536.0,
                y: 1300.0,
            },
            now,
            &mut device,
        )
        .unwrap();
    session.handle(Event::Power, now, &mut device).unwrap();
}
