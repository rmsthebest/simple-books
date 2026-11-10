//! Converts EPUBs and shows what came out, to check the converter against
//! real books.
//!
//! ```sh
//! cargo run --release -p book-convert --example inspect -- book.epub [chapter]
//! ```
//!
//! With several books and no chapter, prints one summary line per book.

use std::{fs::File, io::Cursor, time::Instant};

use book_convert::{Options, convert};
use book_format::{BlockKind, BookReader, Limits, SOFT_HYPHEN};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let chapter: Option<usize> = args.last().and_then(|a| a.parse().ok());
    let paths: Vec<&String> = args
        .iter()
        .filter(|a| a.parse::<usize>().is_err())
        .collect();

    for path in &paths {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) => {
                println!("{path}: {error}");
                continue;
            }
        };
        let started = Instant::now();
        let mut out = Cursor::new(Vec::new());
        let report = match convert(file, &mut out, &Options::default()) {
            Ok(report) => report,
            Err(error) => {
                println!("FAILED {path}: {error}");
                continue;
            }
        };
        let bytes = out.into_inner();
        let elapsed = started.elapsed();
        let size = bytes.len();
        let mut book = BookReader::open(Cursor::new(bytes), Limits::default()).unwrap();
        let manifest = book.manifest().clone();
        let chars: u64 = manifest.chapters.iter().map(|c| c.chars).sum();
        println!(
            "{:>6} ms {:>6} KiB {:>3} ch {:>4} img ({} dropped) {:>3} toc hy={} cut={} {:>8} chars  {} [{}]",
            elapsed.as_millis(),
            size / 1024,
            report.chapters,
            report.images,
            report.images_dropped,
            report.toc_entries,
            u8::from(report.hyphenated),
            u8::from(report.styles_cut_short),
            chars,
            manifest.title,
            manifest.language.as_deref().unwrap_or("?"),
        );

        if paths.len() == 1 && chapter.is_none() {
            for entry in &manifest.toc {
                println!(
                    "  {}{} -> {:?}",
                    "  ".repeat(entry.depth as usize),
                    entry.title,
                    entry.target
                );
            }
        }
        if let Some(index) = chapter {
            show_chapter(&mut book, index);
        }
    }
}

fn show_chapter(book: &mut BookReader<Cursor<Vec<u8>>>, index: usize) {
    let chapter = book.chapter(index).unwrap();
    for (at, block) in chapter.blocks.iter().enumerate() {
        let kind = match &block.kind {
            BlockKind::Paragraph { indent_first } => {
                if *indent_first { "p>" } else { "p" }.to_owned()
            }
            BlockKind::Heading { level } => format!("h{level}"),
            BlockKind::ListItem { marker } => format!("li {marker}"),
            BlockKind::Preformatted => "pre".to_owned(),
            BlockKind::Image { image, alt } => format!("img {image} {alt:?}"),
            BlockKind::Separator => "---".to_owned(),
        };
        let mut text = String::new();
        for span in &block.spans {
            let mut piece = span.text.replace(SOFT_HYPHEN, "·");
            if span.style.emphasis {
                piece = format!("_{piece}_");
            }
            if span.style.strong {
                piece = format!("*{piece}*");
            }
            if span.style.small_caps {
                piece = format!("~{piece}~");
            }
            if let Some(link) = span.link {
                piece = format!("[{piece}]({link:?})");
            }
            text.push_str(&piece);
        }
        println!(
            "{at:>4} {kind:<6} i{} {:?} | {}",
            block.indent,
            block.align,
            text.replace('\n', "⏎")
        );
    }
}
