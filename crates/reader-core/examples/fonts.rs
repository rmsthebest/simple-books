//! Lists the font families found in folders, as the reader would offer them,
//! and with `--sample out.png` draws a line in each:
//! `cargo run -p reader-core --example fonts -- /usr/share/fonts`.

#![allow(clippy::cast_precision_loss, reason = "a few lines of text")]

use std::path::PathBuf;

use reader_core::{Canvas, Settings, Typesetter};

fn main() {
    let mut folders = Vec::new();
    let mut sample = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--sample" {
            sample = args.next().map(PathBuf::from);
        } else {
            folders.push(PathBuf::from(arg));
        }
    }
    let started = std::time::Instant::now();
    let found = reader_core::find_fonts(&folders);
    let took = started.elapsed();
    let families = reader_core::font_families();
    for family in &families {
        let weights = if reader_core::varies_in_weight(Some(family)) {
            "any weight"
        } else {
            "regular and bold"
        };
        println!("{family}  ({weights})");
    }
    println!("{} found in {took:?}", found.len());
    let Some(out) = sample else {
        return;
    };
    let line = 64.0;
    let mut canvas = Canvas::new(1072, (line * (families.len() as f32 + 1.0)) as u32);
    canvas.clear();
    for (i, family) in families.iter().enumerate() {
        let settings = Settings {
            font: Some(family.clone()),
            ..Settings::default()
        };
        let mut typesetter = Typesetter::new(settings, Some("en"));
        let text = format!("{family}: Åsa’s fox");
        let baseline = line * (i as f32 + 1.0);
        typesetter.draw_label(&mut canvas, &text, (32.0, baseline), 40.0, 400.0);
        typesetter.draw_label(&mut canvas, "and in bold", (700.0, baseline), 40.0, 700.0);
    }
    image::GrayImage::from_raw(canvas.width(), canvas.height(), canvas.pixels().to_vec())
        .expect("a whole canvas")
        .save(&out)
        .expect("the sample saved");
}
