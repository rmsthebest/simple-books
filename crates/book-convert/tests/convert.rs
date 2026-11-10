//! Whole books in, whole books out: ordinary books, and books built to hurt.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    fmt::Write as _,
    io::{Cursor, Write},
};

use book_convert::{ConvertError, Limits, Options, Report, convert};
use book_format::{BlockKind, BookReader, Chapter, Manifest, Position, SOFT_HYPHEN};
use proptest::prelude::*;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// A book's parts, as (name inside `OEBPS/`, manifest id, media type, body).
struct Epub {
    language: &'static str,
    items: Vec<(String, String, &'static str, Vec<u8>)>,
    spine: Vec<String>,
    nav: Option<String>,
    ncx: Option<String>,
    /// More of the OPF's `<metadata>`, as XML.
    metadata: String,
}

impl Epub {
    fn new() -> Self {
        Self {
            language: "en",
            items: Vec::new(),
            spine: Vec::new(),
            nav: None,
            ncx: None,
            metadata: String::new(),
        }
    }

    fn metadata(mut self, xml: &str) -> Self {
        self.metadata.push_str(xml);
        self
    }

    /// The id the next item added will get.
    fn next_id(&self) -> String {
        format!("id{}", self.items.len())
    }

    fn chapter(mut self, name: &str, body: &str) -> Self {
        let html = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>x</title><link rel="stylesheet" type="text/css" href="style.css"/></head>
<body>{body}</body></html>"#
        );
        let id = format!("id{}", self.items.len());
        self.items.push((
            name.to_owned(),
            id.clone(),
            "application/xhtml+xml",
            html.into_bytes(),
        ));
        self.spine.push(id);
        self
    }

    fn file(mut self, name: &str, media_type: &'static str, body: &[u8]) -> Self {
        let id = format!("id{}", self.items.len());
        self.items
            .push((name.to_owned(), id, media_type, body.to_vec()));
        self
    }

    fn nav(mut self, body: &str) -> Self {
        let html = format!(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>{body}</ol></nav></body></html>"#
        );
        self = self.file("nav.xhtml", "application/xhtml+xml", html.as_bytes());
        self.nav = Some(self.items.last().unwrap().1.clone());
        self
    }

    fn ncx(mut self, points: &str) -> Self {
        let ncx = format!(
            r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>{points}</navMap></ncx>"#
        );
        self = self.file("toc.ncx", "application/x-dtbncx+xml", ncx.as_bytes());
        self.ncx = Some(self.items.last().unwrap().1.clone());
        self
    }

    fn build(&self) -> Vec<u8> {
        let mut manifest = String::new();
        for (name, id, media_type, _) in &self.items {
            let nav = if self.nav.as_ref() == Some(id) {
                r#" properties="nav""#
            } else {
                ""
            };
            write!(
                manifest,
                r#"<item id="{id}" href="{name}" media-type="{media_type}"{nav}/>"#
            )
            .unwrap();
        }
        let mut spine = String::new();
        for id in &self.spine {
            write!(spine, r#"<itemref idref="{id}"/>"#).unwrap();
        }
        let toc = self
            .ncx
            .as_ref()
            .map(|id| format!(r#" toc="{id}""#))
            .unwrap_or_default();
        let opf = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Test Book</dc:title><dc:creator>Ann Author</dc:creator><dc:language>{}</dc:language>
{}</metadata><manifest>{manifest}</manifest><spine{toc}>{spine}</spine></package>"#,
            self.language, self.metadata
        );

        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let mut put = |name: &str, body: &[u8]| {
            zip.start_file(name, options).unwrap();
            zip.write_all(body).unwrap();
        };
        put("mimetype", b"application/epub+zip");
        put("META-INF/container.xml", CONTAINER.as_bytes());
        put("OEBPS/content.opf", opf.as_bytes());
        if !self.items.iter().any(|(name, ..)| name == "style.css") {
            put("OEBPS/style.css", b".it { font-style: italic }");
        }
        for (name, _, _, body) in &self.items {
            put(&format!("OEBPS/{name}"), body);
        }
        zip.finish().unwrap().into_inner()
    }
}

struct Converted {
    report: Report,
    manifest: Manifest,
    chapters: Vec<Chapter>,
    images: Vec<Vec<u8>>,
}

fn converted_with(epub: &Epub, options: &Options) -> Result<Converted, ConvertError> {
    let mut out = Cursor::new(Vec::new());
    let report = convert(Cursor::new(epub.build()), &mut out, options)?;
    let mut book = BookReader::open(
        Cursor::new(out.into_inner()),
        book_format::Limits::default(),
    )
    .unwrap();
    let manifest = book.manifest().clone();
    let chapters = (0..manifest.chapters.len())
        .map(|index| book.chapter(index).unwrap())
        .collect();
    let images = (0..manifest.images.len())
        .map(|index| book.image(index).unwrap())
        .collect();
    Ok(Converted {
        report,
        manifest,
        chapters,
        images,
    })
}

fn converted(epub: &Epub) -> Converted {
    converted_with(epub, &Options::default()).unwrap()
}

fn text(chapter: &Chapter, block: usize) -> String {
    chapter.blocks[block]
        .spans
        .iter()
        .map(|span| span.text.replace(SOFT_HYPHEN, ""))
        .collect()
}

fn at(chapter: u32, block: u32, offset: u32) -> Position {
    Position {
        chapter,
        block,
        offset,
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[test]
fn an_ordinary_book() {
    let book = Epub::new()
        .chapter("cover.xhtml", r#"<div><img src="images/cover.png" alt="Cover"/></div>"#)
        .chapter("blank.xhtml", "<p> </p>")
        .chapter(
            "one.xhtml",
            r#"<h1 id="c1">One</h1><p>It was a <span class="it">dark</span> and extraordinary night.<a href="two.xhtml#n1" id="r1"><sup>1</sup></a></p>"#,
        )
        .chapter(
            "two.xhtml",
            r#"<h1>Notes</h1><p id="n1"><a href="one.xhtml#r1">1</a> A note. See <a href="blank.xhtml">this</a> and <a href="https://example.com">that</a>.</p>"#,
        )
        .file("images/cover.png", "image/png", &png(1200, 1800))
        .nav(r#"<li><a href="one.xhtml#c1">Chapter One</a></li><li><a href="two.xhtml">Notes</a></li><li><a href="gone.xhtml">Gone</a></li>"#);
    let found = converted(&book);

    assert_eq!(found.manifest.title, "Test Book");
    assert_eq!(found.manifest.authors, ["Ann Author"]);
    assert_eq!(found.manifest.language.as_deref(), Some("en"));
    assert_eq!(found.chapters.len(), 3, "the blank document is left out");
    assert!(found.report.hyphenated);

    // The cover, scaled to the screen.
    let info = found.manifest.images[0];
    assert_eq!((info.width, info.height), (965, 1448));
    assert_eq!(
        found.chapters[0].blocks[0].kind,
        BlockKind::Image {
            image: 0,
            alt: "Cover".to_owned()
        }
    );
    assert_eq!(
        image::load_from_memory(&found.images[0]).unwrap().width(),
        965
    );

    let one = &found.chapters[1];
    assert_eq!(one.blocks[0].kind, BlockKind::Heading { level: 1 });
    assert_eq!(text(one, 1), "It was a dark and extraordinary night.1");
    assert!(
        one.blocks[1].spans[1].style.emphasis,
        "italics from a class"
    );
    assert!(
        one.blocks[1].spans[2].text.contains(SOFT_HYPHEN),
        "hyphenated"
    );
    let note_ref = one.blocks[1].spans.last().unwrap();
    assert_eq!(note_ref.link, Some(at(2, 1, 0)));

    let two = &found.chapters[2];
    let links: Vec<Option<Position>> = two.blocks[1].spans.iter().map(|s| s.link).collect();
    assert!(
        links.contains(&Some(at(1, 1, 38))),
        "back to the note's mark"
    );
    assert!(
        links.contains(&Some(at(1, 0, 0))),
        "a link to a left-out document goes to the next chapter"
    );
    assert_eq!(links.iter().flatten().count(), 2, "no link out of the book");

    assert_eq!(found.manifest.toc.len(), 2);
    assert_eq!(found.manifest.toc[0].title, "Chapter One");
    assert_eq!(found.manifest.toc[0].target, at(1, 0, 0));
    assert_eq!(found.manifest.toc[1].target, at(2, 0, 0));
}

#[test]
fn the_series_and_a_cover_off_the_spine_are_kept() {
    let book = Epub::new().chapter("one.xhtml", "<p>Text.</p>");
    let cover = book.next_id();
    let book = book
        .file("images/cover.png", "image/png", &png(600, 900))
        .metadata(&format!(
            r#"<meta name="cover" content="{cover}"/>
            <meta name="calibre:series" content="The Saga"/>
            <meta name="calibre:series_index" content="2"/>"#
        ));
    let found = converted(&book);
    let series = found.manifest.series.as_ref().unwrap();
    assert_eq!(
        (series.name.as_str(), series.index),
        ("The Saga", Some(2.0))
    );
    // Stored though no page shows it.
    assert_eq!(found.manifest.cover, Some(0));
    assert_eq!(found.images.len(), 1);
}

#[test]
fn a_cover_shown_on_a_page_is_stored_once() {
    let book = Epub::new().chapter(
        "cover.xhtml",
        r#"<div><img src="images/cover.png" alt="Cover"/></div>"#,
    );
    let cover = book.next_id();
    let book = book
        .file("images/cover.png", "image/png", &png(600, 900))
        .chapter("one.xhtml", "<p>Text.</p>")
        .metadata(&format!(r#"<meta name="cover" content="{cover}"/>"#));
    let found = converted(&book);
    assert_eq!(found.manifest.cover, Some(0));
    assert_eq!(found.images.len(), 1);
    assert_eq!(found.manifest.series, None);
}

#[test]
fn the_fuller_table_of_contents_wins() {
    let book = Epub::new()
        .chapter("a.xhtml", "<p>A</p>")
        .chapter("b.xhtml", "<p>B</p>")
        .nav(r#"<li><a href="a.xhtml">Only A</a></li>"#)
        .ncx(
            r#"<navPoint><navLabel><text>A</text></navLabel><content src="a.xhtml"/></navPoint>
               <navPoint><navLabel><text>B</text></navLabel><content src="b.xhtml"/></navPoint>"#,
        );
    let titles: Vec<String> = converted(&book)
        .manifest
        .toc
        .into_iter()
        .map(|e| e.title)
        .collect();
    assert_eq!(titles, ["A", "B"]);
}

#[test]
fn headings_make_a_table_of_contents_when_the_book_has_none() {
    let book = Epub::new()
        .chapter(
            "a.xhtml",
            "<h2>Part</h2><h3>First</h3><p>x</p><h4>Too deep</h4>",
        )
        .chapter("b.xhtml", "<h3>Second</h3><p>y</p>");
    let toc: Vec<(String, u8, Position)> = converted(&book)
        .manifest
        .toc
        .into_iter()
        .map(|e| (e.title, e.depth, e.target))
        .collect();
    assert_eq!(
        toc,
        [
            ("Part".to_owned(), 0, at(0, 0, 0)),
            ("First".to_owned(), 1, at(0, 1, 0)),
            ("Second".to_owned(), 1, at(1, 0, 0)),
        ]
    );
}

#[test]
fn a_language_without_patterns_keeps_the_books_soft_hyphens() {
    let mut book = Epub::new().chapter("a.xhtml", "<p>ex&#173;tra&shy;ordinary</p>");
    book.language = "und";
    let found = converted(&book);
    assert!(!found.report.hyphenated);
    assert_eq!(
        found.chapters[0].blocks[0].spans[0].text,
        "ex\u{AD}tra\u{AD}ordinary"
    );
}

#[test]
fn a_book_without_text_is_refused() {
    let book = Epub::new()
        .chapter("a.xhtml", "<p> </p><div><img src='missing.png'/></div>")
        .chapter("b.xhtml", "<script>alert(1)</script>");
    assert!(matches!(
        converted_with(&book, &Options::default()),
        Err(ConvertError::NoText)
    ));
}

#[test]
fn limits_hold() {
    let big = format!("<p>{}</p>", "word ".repeat(10_000));
    let book = Epub::new().chapter("a.xhtml", &big);
    let small = Options {
        limits: Limits {
            max_document_bytes: 1000,
            ..Limits::default()
        },
        ..Options::default()
    };
    assert!(matches!(
        converted_with(&book, &small),
        Err(ConvertError::Epub(epub::EpubError::TooLarge))
    ));

    let total = Options {
        limits: Limits {
            max_total_document_bytes: 100_000,
            ..Limits::default()
        },
        ..Options::default()
    };
    let mut many = Epub::new();
    for index in 0..10 {
        many = many.chapter(&format!("{index}.xhtml"), &big);
    }
    assert!(matches!(
        converted_with(&many, &total),
        Err(ConvertError::TooLarge)
    ));

    let mut pictures = Epub::new();
    let mut body = String::from("<p>Pictures</p>");
    for index in 0..5 {
        write!(body, r#"<img src="{index}.png"/>"#).unwrap();
        pictures = pictures.file(&format!("{index}.png"), "image/png", &png(100, 100));
    }
    pictures = pictures.chapter("a.xhtml", &body);
    let few = Options {
        limits: Limits {
            max_images: 2,
            ..Limits::default()
        },
        ..Options::default()
    };
    let found = converted_with(&pictures, &few).unwrap();
    assert_eq!(found.manifest.images.len(), 2);
    assert_eq!(found.report.images_dropped, 3);
}

#[test]
fn style_work_runs_out_without_failing() {
    let css = ".a .a .a .a .a .a .a .b { font-weight: bold }\n".repeat(5000);
    let body = format!(
        "{}<p class=\"b\">deep</p>{}",
        "<div class=\"a\">".repeat(100),
        "</div>".repeat(100)
    )
    .repeat(20);
    let book = Epub::new()
        .file("style.css", "text/css", css.as_bytes())
        .chapter("a.xhtml", &body);
    let tight = Options {
        limits: Limits {
            max_style_work: 100_000,
            ..Limits::default()
        },
        ..Options::default()
    };
    let found = converted_with(&book, &tight).unwrap();
    assert!(found.report.styles_cut_short);
    assert_eq!(found.chapters[0].blocks.len(), 20);
}

#[test]
fn entities_declared_by_the_book_are_never_expanded() {
    let laughs = r#"<?xml version="1.0"?>
<!DOCTYPE html [
 <!ENTITY a "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa">
 <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
 <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
]>
<html><body><p>&c;&c;&c;&c;</p></body></html>"#;
    let book = Epub::new().file("a.xhtml", "application/xhtml+xml", laughs.as_bytes());
    let mut book = book;
    book.spine.push(book.items[0].1.clone());
    let found = converted(&book);
    assert_eq!(text(&found.chapters[0], 0), "&c;&c;&c;&c;");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn any_chapter_converts_or_fails_cleanly(body in r##"([<>/a-z ="'&;#.\n]|<p>|</p>|<i>|<br/>|<img src="x.png"/>|<a href="#x">|<h1>|<li>|<ol>|<pre>|&nbsp;){0,200}"##) {
        let book = Epub::new()
            .file("x.png", "image/png", &png(100, 100))
            .chapter("a.xhtml", &body);
        match converted_with(&book, &Options::default()) {
            Ok(_) | Err(ConvertError::NoText) => {}
            Err(other) => panic!("{other}"),
        }
    }

    #[test]
    fn any_stylesheet_is_harmless(css in r#"([{}:;.#>*@ a-z"'/\n-]|font-style|italic|display|none|@media){0,200}"#) {
        let book = Epub::new()
            .file("style.css", "text/css", css.as_bytes())
            .chapter("a.xhtml", "<p class='a b'>x <span id='c'>y</span></p>");
        converted_with(&book, &Options::default()).unwrap();
    }
}
