//! The engine against made-up books: every line ends up on exactly one page,
//! turning walks the whole book both ways, positions find their pages, and
//! nothing is drawn in the side margins.

use std::io::Cursor;

use book_format::{
    Align, Block, BlockKind, BookReader, Chapter, ImageInfo, ImageKind, Limits, NewImage, Position,
    SOFT_HYPHEN, Span, Style,
};
use reader_core::{PageMap, PageRef, Reader, Settings, Turn, Typesetter, paginate, paginate_book};

const WORDS: &[&str] = &[
    "the", "road", "ran", "south", "through", "a", "gray", "country", "of", "ash", "and", "rain",
    "where", "nothing", "moved", "but", "wind",
];

/// Prose with a soft hyphen in every long word, as the server makes it.
fn prose(seed: usize, words: usize) -> String {
    let mut text = String::new();
    for i in 0..words {
        if i > 0 {
            text.push(' ');
        }
        let word = WORDS[(seed * 7 + i * 13) % WORDS.len()];
        if word.len() > 5 {
            let (a, b) = word.split_at(3);
            text.push_str(a);
            text.push(SOFT_HYPHEN);
            text.push_str(b);
        } else {
            text.push_str(word);
        }
    }
    text
}

fn paragraph(seed: usize, words: usize) -> Block {
    Block {
        kind: BlockKind::Paragraph {
            indent_first: seed.is_multiple_of(2),
        },
        indent: 0,
        align: Align::Default,
        spans: vec![
            Span::plain(prose(seed, words)),
            Span {
                text: format!(" {}", prose(seed + 1, 6)),
                style: Style {
                    emphasis: true,
                    ..Style::default()
                },
                link: None,
            },
        ],
    }
}

fn heading(text: &str) -> Block {
    Block {
        kind: BlockKind::Heading { level: 1 },
        indent: 0,
        align: Align::Center,
        spans: vec![Span::plain(text)],
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::GrayImage::from_pixel(width, height, image::Luma([90]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

/// Three chapters with an empty one between, a list, a separator, an image
/// and a run with nowhere to break.
fn book() -> Vec<u8> {
    let mut first = vec![heading("One")];
    for seed in 0..40 {
        first.push(paragraph(seed, 20 + seed * 3 % 70));
    }
    first.push(Block {
        kind: BlockKind::ListItem {
            marker: "•".into()
        },
        indent: 1,
        align: Align::Default,
        spans: vec![Span::plain(prose(3, 30))],
    });
    first.push(Block {
        kind: BlockKind::Separator,
        indent: 0,
        align: Align::Default,
        spans: Vec::new(),
    });
    first.push(Block {
        kind: BlockKind::Image {
            image: 0,
            alt: String::new(),
        },
        indent: 0,
        align: Align::Default,
        spans: Vec::new(),
    });
    first.push(Block {
        kind: BlockKind::Paragraph {
            indent_first: false,
        },
        indent: 0,
        align: Align::Default,
        spans: vec![Span::plain("x\u{a0}".repeat(200))],
    });
    let mut third = vec![heading("Three")];
    for seed in 0..25 {
        third.push(paragraph(seed + 100, 40));
    }
    let chapters = [
        Chapter { blocks: first },
        Chapter { blocks: Vec::new() },
        Chapter { blocks: third },
    ];
    let images = [NewImage {
        info: ImageInfo {
            width: 2000,
            height: 600,
            kind: ImageKind::Png,
        },
        bytes: png(2000, 600),
    }];
    let mut out = Cursor::new(Vec::new());
    book_format::write(
        &mut out,
        book_format::About {
            title: "Test".into(),
            language: Some("en".into()),
            ..book_format::About::default()
        },
        Vec::new(),
        &chapters,
        &images,
    )
    .unwrap();
    out.into_inner()
}

fn open(bytes: &[u8]) -> BookReader<Cursor<Vec<u8>>> {
    BookReader::open(Cursor::new(bytes.to_vec()), Limits::default()).unwrap()
}

const START: Position = Position {
    chapter: 0,
    block: 0,
    offset: 0,
};

#[test]
fn turning_walks_every_page_both_ways() {
    let bytes = book();
    let mut reader = Reader::open(open(&bytes), Settings::default(), 1, START, None).unwrap();
    let mut forward = vec![reader.current().unwrap()];
    while reader.turn(Turn::Next).unwrap() {
        forward.push(reader.current().unwrap());
    }
    let mut back = vec![reader.current().unwrap()];
    while reader.turn(Turn::Previous).unwrap() {
        back.push(reader.current().unwrap());
    }
    back.reverse();
    assert_eq!(forward, back);
    assert!(forward.windows(2).all(|w| w[0] < w[1]));
    // The empty chapter has no pages, and the book has more than a few.
    assert!(forward.iter().all(|page| page.chapter != 1));
    assert!(forward.len() > 10, "{} pages", forward.len());
    assert_eq!(reader.page_map().total(), Some(forward.len()));
}

#[test]
fn pages_start_in_order_and_cover_each_block() {
    let bytes = book();
    let mut book = open(&bytes);
    let chapter = book.chapter(0).unwrap();
    let images = book.manifest().images.clone();
    let mut typesetter = Typesetter::new(Settings::default(), Some("en"));
    let pages = paginate(&mut typesetter, 0, &chapter, &images);
    assert_eq!((pages[0].block, pages[0].line), (0, 0));
    assert!(
        pages
            .windows(2)
            .all(|w| (w[0].block, w[0].line) < (w[1].block, w[1].line))
    );
    let empty = paginate(&mut typesetter, 1, &Chapter::default(), &images);
    assert_eq!(empty, []);
}

#[test]
fn positions_find_their_pages() {
    let bytes = book();
    let mut reader = Reader::open(open(&bytes), Settings::default(), 1, START, None).unwrap();
    let mut pages = vec![reader.current().unwrap()];
    while reader.turn(Turn::Next).unwrap() {
        pages.push(reader.current().unwrap());
    }
    for page in pages.iter().copied().step_by(3) {
        let position = reader.page_map().position(page).unwrap();
        reader.go_to(position).unwrap();
        assert_eq!(reader.current(), Some(page));
        // A little further into the same page is still that page.
        reader
            .go_to(Position {
                offset: position.offset + 1,
                ..position
            })
            .unwrap();
        assert_eq!(reader.current(), Some(page));
    }
    // The empty chapter opens at the next chapter's first page.
    reader
        .go_to(Position {
            chapter: 1,
            block: 0,
            offset: 0,
        })
        .unwrap();
    assert_eq!(
        reader.current(),
        Some(PageRef {
            chapter: 2,
            page: 0
        })
    );
    // Past the end of the book is its last page.
    reader
        .go_to(Position {
            chapter: 9,
            block: 99,
            offset: 0,
        })
        .unwrap();
    assert_eq!(reader.current(), pages.last().copied());
}

#[test]
fn prepared_pages_are_ready_both_ways() {
    let bytes = book();
    let mut reader = Reader::open(open(&bytes), Settings::default(), 1, START, None).unwrap();
    for _ in 0..3 {
        reader.turn(Turn::Next).unwrap();
    }
    let here = reader.current().unwrap();
    let drawn = reader.page().unwrap().unwrap().clone();
    reader.prepare().unwrap();
    reader.turn(Turn::Previous).unwrap();
    assert!(reader.is_drawn(reader.current().unwrap()));
    reader.turn(Turn::Next).unwrap();
    reader.turn(Turn::Next).unwrap();
    assert!(reader.is_drawn(reader.current().unwrap()));
    // Drawing a page again gives the same pixels.
    reader.turn(Turn::Previous).unwrap();
    assert_eq!(reader.current(), Some(here));
    assert_eq!(reader.page().unwrap().unwrap(), &drawn);
}

#[test]
fn nothing_is_drawn_in_the_side_margins() {
    let bytes = book();
    let settings = Settings::default();
    let mut reader = Reader::open(open(&bytes), settings.clone(), 1, START, None).unwrap();
    let mut inked = 0;
    loop {
        let canvas = reader.page().unwrap().unwrap();
        let width = canvas.width() as usize;
        // A glyph's ink can reach a little past its advance, as an italic
        // f's tail does.
        let margin = settings.margin_side as usize - 8;
        for row in canvas.pixels().chunks(width) {
            assert!(
                row[..margin].iter().all(|p| *p == 255),
                "ink in the left margin"
            );
            assert!(
                row[width - margin..].iter().all(|p| *p == 255),
                "ink in the right margin"
            );
        }
        inked += usize::from(canvas.pixels().iter().any(|p| *p < 128));
        if !reader.turn(Turn::Next).unwrap() {
            break;
        }
    }
    assert!(inked > 10);
}

#[test]
fn a_background_map_matches_paginating_on_demand() {
    let bytes = book();
    let settings = Settings::default();
    let key = PageMap::key(1, &settings);
    let mut map = PageMap::new(key, 3);
    paginate_book(&mut open(&bytes), settings.clone(), &mut map, None, || {
        false
    })
    .unwrap();
    assert!(map.is_complete());

    let mut reader = Reader::open(open(&bytes), settings.clone(), 1, START, None).unwrap();
    while reader.turn(Turn::Next).unwrap() {}
    assert_eq!(reader.page_map(), &map);

    // A saved map is used as is, and one for other settings is ignored.
    let reader = Reader::open(open(&bytes), settings.clone(), 1, START, Some(map.clone())).unwrap();
    assert!(reader.page_map().is_complete());
    let bigger = Settings {
        font_size: 48.0,
        ..settings
    };
    let reader = Reader::open(open(&bytes), bigger, 1, START, Some(map)).unwrap();
    assert!(!reader.page_map().is_complete());
}

#[test]
fn bigger_text_makes_more_pages() {
    let bytes = book();
    let count = |settings: Settings| {
        let mut map = PageMap::new(PageMap::key(1, &settings), 3);
        paginate_book(&mut open(&bytes), settings, &mut map, None, || false).unwrap();
        map.total().unwrap()
    };
    let normal = count(Settings::default());
    let big = count(Settings {
        font_size: 56.0,
        ..Settings::default()
    });
    assert!(big > normal, "{big} vs {normal}");
}

#[test]
fn a_book_with_nothing_in_it_opens() {
    let mut out = Cursor::new(Vec::new());
    book_format::write(
        &mut out,
        book_format::About {
            title: "Blank".into(),
            ..book_format::About::default()
        },
        Vec::new(),
        &[Chapter::default(), Chapter::default()],
        &[],
    )
    .unwrap();
    let bytes = out.into_inner();
    let mut reader = Reader::open(open(&bytes), Settings::default(), 1, START, None).unwrap();
    assert!(reader.page().unwrap().is_none());
    assert!(!reader.turn(Turn::Previous).unwrap());
}

/// A chapter whose second paragraph has a footnote reference, and a chapter
/// of notes.
fn footnoted_book(target: Position) -> Vec<u8> {
    let mut first = vec![heading("One"), paragraph(1, 40)];
    let mut note = Span::plain("1");
    note.style.vertical = book_format::Vertical::Super;
    note.link = Some(target);
    first.push(Block {
        kind: BlockKind::Paragraph { indent_first: true },
        indent: 0,
        align: Align::Default,
        spans: vec![Span::plain(prose(2, 12)), note, Span::plain(prose(3, 30))],
    });
    let mut notes = vec![heading("Notes")];
    for seed in 0..40 {
        notes.push(paragraph(seed + 200, 30));
    }
    let chapters = [Chapter { blocks: first }, Chapter { blocks: notes }];
    let mut out = Cursor::new(Vec::new());
    book_format::write(
        &mut out,
        book_format::About {
            title: "Notes".into(),
            language: Some("en".into()),
            ..book_format::About::default()
        },
        Vec::new(),
        &chapters,
        &[],
    )
    .unwrap();
    out.into_inner()
}

#[test]
fn a_tapped_footnote_goes_there_and_back() {
    let target = Position {
        chapter: 1,
        block: 30,
        offset: 0,
    };
    let bytes = footnoted_book(target);
    let settings = Settings::default();
    let mut reader = Reader::open(open(&bytes), settings.clone(), 1, START, None).unwrap();
    reader.page().unwrap();
    let here = reader.current().unwrap();

    // Somewhere on the page, a tap finds the superscript; most places don't.
    let mut hits = 0;
    let mut misses = 0;
    let (width, height) = (
        u16::try_from(settings.width).unwrap(),
        u16::try_from(settings.height).unwrap(),
    );
    for y in (0..height).step_by(8) {
        for x in (0..width).step_by(8) {
            match reader.link_at(f32::from(x), f32::from(y)) {
                Some(found) => {
                    assert_eq!(found, target);
                    hits += 1;
                }
                None => misses += 1,
            }
        }
    }
    assert!(hits > 0, "the link can't be tapped");
    assert!(misses > hits * 20, "{hits} hits, {misses} misses");

    assert!(!reader.can_go_back());
    reader.follow(target).unwrap();
    let there = reader.current().unwrap();
    assert_eq!(Some(there), reader.page_map().locate(target));
    assert_ne!(there, here);
    assert!(reader.can_go_back());

    reader.turn(Turn::Next).unwrap();
    assert!(reader.back().unwrap());
    assert_eq!(reader.current(), Some(here));
    assert!(!reader.back().unwrap());
    assert_eq!(reader.current(), Some(here));
}

/// Every page from wherever `reader` is to the end of the book, then back
/// to the start: where each one starts.
fn walk(reader: &mut Reader<Cursor<Vec<u8>>>) -> Vec<Position> {
    let mut seen = vec![reader.position().unwrap()];
    while reader.turn(Turn::Next).unwrap() {
        seen.push(reader.position().unwrap());
    }
    while reader.turn(Turn::Previous).unwrap() {
        seen.push(reader.position().unwrap());
    }
    seen
}

#[test]
fn a_relayout_shows_its_page_first_and_lays_out_the_same_pages() {
    let bytes = book();
    let settings = Settings::default();
    let mut map = PageMap::new(PageMap::key(1, &settings), 3);
    paginate_book(&mut open(&bytes), settings.clone(), &mut map, None, || {
        false
    })
    .unwrap();
    let bigger = Settings {
        font_size: settings.font_size * 1.3,
        ..settings
    };
    let total = map.total().unwrap();
    let tops: Vec<Position> = (0..total)
        .step_by(total / 3)
        .filter_map(|n| {
            let (chapter, page) = (0u32..3).find_map(|chapter| {
                let before: usize = (0..chapter).map(|c| map.pages(c).unwrap().len()).sum();
                let page = n.checked_sub(before)?;
                (page < map.pages(chapter)?.len()).then_some((chapter, page as u32))
            })?;
            map.position(PageRef { chapter, page })
        })
        .collect();
    assert!(tops.len() >= 3);
    for top in tops {
        // The whole book laid out as the background thread would, keeping
        // a page at `top`.
        let mut full = PageMap::new(PageMap::key(1, &bigger), 3);
        paginate_book(
            &mut open(&bytes),
            bigger.clone(),
            &mut full,
            Some(top),
            || false,
        )
        .unwrap();

        let mut reader = Reader::relayout(open(&bytes), bigger.clone(), 1, top, None).unwrap();
        assert_eq!(reader.keeping(), Some(top));
        assert_eq!(
            reader.position().map(|p| (p.chapter, p.block)),
            Some((top.chapter, top.block))
        );
        reader.page().unwrap();
        reader.prepare().unwrap();
        assert!(
            reader.page_map().pages(top.chapter).is_none(),
            "only a few pages laid out, even once prepared"
        );
        let through = reader.through_chapter().unwrap();
        assert!((0.0..=1.0).contains(&through));

        // Taking the background's map: the same page, now numbered.
        let mut adopted = Reader::relayout(open(&bytes), bigger.clone(), 1, top, None).unwrap();
        let at = adopted.position();
        adopted.adopt(&full);
        assert_eq!(adopted.keeping(), None);
        assert_eq!(adopted.position(), at);
        assert_eq!(adopted.current(), full.locate(top), "{top:?}");
        assert!(
            (adopted.through_chapter().unwrap() - through).abs() < 0.2,
            "by characters it's about where it is by pages"
        );

        // Turning past the few pages lays the chapter out there and then,
        // the same as the background does.
        let mut opened =
            Reader::open(open(&bytes), bigger.clone(), 1, top, Some(full.clone())).unwrap();
        assert_eq!(walk(&mut reader), walk(&mut opened), "{top:?}");
        assert_eq!(
            reader.page_map().pages(top.chapter),
            full.pages(top.chapter)
        );
    }
}

#[test]
fn background_pagination_stops_within_a_long_chapter() {
    let bytes = book();
    let settings = Settings::default();
    let mut map = PageMap::new(PageMap::key(1, &settings), 3);
    let mut asked = 0;
    // Yes the second time: before the first chapter, then partway into it.
    paginate_book(&mut open(&bytes), settings, &mut map, None, || {
        asked += 1;
        asked > 1
    })
    .unwrap();
    assert_eq!(asked, 2);
    assert_eq!(map.chapters[0], None, "half a chapter isn't kept");
}
