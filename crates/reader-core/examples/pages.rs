//! Draws a book's pages to PNGs and times the engine, to look at the output
//! and get a per-page budget to check on the device.
//!
//! ```sh
//! cargo run --release -p reader-core --example pages -- book.epub [out-dir] [pages]
//! ```
//!
//! Takes an EPUB (converted here, as the server would) or a converted book.
#![allow(clippy::cast_precision_loss, reason = "timings")]

use std::{
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
    time::{Duration, Instant},
};

use book_format::{BookReader, Limits, Position};
use reader_core::{PageMap, Reader, Settings, Turn, paginate_book};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = PathBuf::from(&args[0]);
    let out = args.get(1).map(PathBuf::from);
    let wanted: usize = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(12);

    let mut bytes = Vec::new();
    File::open(&input).unwrap().read_to_end(&mut bytes).unwrap();
    if input.extension().is_some_and(|e| e == "epub") {
        let mut converted = Cursor::new(Vec::new());
        book_convert::convert(
            Cursor::new(bytes),
            &mut converted,
            &book_convert::Options::default(),
        )
        .unwrap();
        bytes = converted.into_inner();
    }
    let open = || BookReader::open(Cursor::new(bytes.clone()), Limits::default()).unwrap();
    let settings = Settings::default();

    // The whole book, as the background thread would do it.
    let mut book = open();
    let mut map = PageMap::new(PageMap::key(0, &settings), book.manifest().chapters.len());
    let started = Instant::now();
    paginate_book(&mut book, settings.clone(), &mut map, || false).unwrap();
    let paginated = started.elapsed();
    let total = map.total().unwrap();
    println!(
        "{}: {total} pages, paginated in {paginated:?} ({:.2} ms/page)",
        book.manifest().title,
        paginated.as_secs_f64() * 1000.0 / total.max(1) as f64
    );

    // Opening cold at the start, then drawing pages one after another.
    let started = Instant::now();
    let start = Position {
        chapter: 0,
        block: 0,
        offset: 0,
    };
    let mut reader = Reader::open(open(), settings, 0, start, None).unwrap();
    reader.page().unwrap();
    println!("opened and drew the first page in {:?}", started.elapsed());

    if let Some(out) = &out {
        std::fs::create_dir_all(out).unwrap();
    }
    let mut drawing = Duration::ZERO;
    let mut turning = Duration::ZERO;
    let mut drawn = 0;
    for n in 0..wanted {
        let started = Instant::now();
        let canvas = reader.page().unwrap().unwrap();
        let elapsed = started.elapsed();
        if n > 0 {
            drawing += elapsed;
            drawn += 1;
        }
        if let Some(out) = &out {
            image::GrayImage::from_raw(canvas.width(), canvas.height(), canvas.pixels().to_vec())
                .unwrap()
                .save(out.join(format!("page-{n:03}.png")))
                .unwrap();
        }
        if !reader.turn(Turn::Next).unwrap() {
            break;
        }
    }
    // With the neighbours prepared, a turn either way is a cache hit.
    for turn in [Turn::Previous, Turn::Next, Turn::Previous] {
        reader.prepare().unwrap();
        let started = Instant::now();
        reader.turn(turn).unwrap();
        reader.page().unwrap();
        turning += started.elapsed();
    }
    println!(
        "drawing a page: {:.2} ms on average over {drawn}; a prepared turn: {:?}",
        drawing.as_secs_f64() * 1000.0 / f64::from(drawn.max(1)),
        turning / 3
    );
}
