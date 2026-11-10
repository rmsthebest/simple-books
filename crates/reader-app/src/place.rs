//! Where the reader is in a book, as saved between openings and synced.
//!
//! A place says which file it was taken in, so it's only used as it stands
//! in that file. In any other, such as the same book made again by a newer
//! converter, block numbers may have moved, and it goes by how far through
//! the book it was instead.

use std::io::{Read, Seek};

use book_format::{BookReader, Position};
use serde::{Deserialize, Serialize};

/// The start of a book.
pub(crate) const START: Position = Position {
    chapter: 0,
    block: 0,
    offset: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedPlace {
    #[serde(flatten)]
    pub position: Position,
    /// The key of the file it was taken in. 0 for a place from another
    /// reader, which says its [`SavedPlace::revision`] instead, and for
    /// places saved before this was kept, which are taken as they are.
    #[serde(default)]
    pub file: u64,
    /// The server's revision of the file it was taken in, if it's a synced
    /// book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    /// How far through the book, from 0 to 1.
    #[serde(default)]
    pub progress: f32,
    /// When the reader was there, in seconds since 1970; 0 if unknown.
    #[serde(default)]
    pub at: u64,
}

impl SavedPlace {
    /// Whether the position points at the same words in the file with key
    /// `file` and server revision `revision`.
    pub(crate) fn fits(&self, file: u64, revision: Option<i64>) -> bool {
        if self.file != 0 {
            return self.file == file;
        }
        match self.revision {
            Some(taken) => Some(taken) == revision,
            None => true,
        }
    }

    /// Where to open `book`, whose file has key `file` and revision
    /// `revision`.
    pub(crate) fn resolve<R: Read + Seek>(
        &self,
        book: &mut BookReader<R>,
        file: u64,
        revision: Option<i64>,
    ) -> Position {
        if self.fits(file, revision) {
            self.position
        } else {
            position_at(book, self.progress)
        }
    }
}

/// The start of the block `fraction` of the way through the book, by
/// length.
pub(crate) fn position_at<R: Read + Seek>(book: &mut BookReader<R>, fraction: f32) -> Position {
    let chapters: Vec<u64> = book.manifest().chapters.iter().map(|c| c.chars).collect();
    let total: u64 = chapters.iter().sum();
    // Not a number, too.
    if total == 0 || fraction.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return START;
    }
    let mut want = (f64::from(fraction.min(1.0)) * total as f64) as u64;
    let Some(chapter) = chapters.iter().position(|&chars| {
        if want < chars {
            true
        } else {
            want -= chars;
            false
        }
    }) else {
        // The very end: the last chapter's start is as good as any.
        return Position {
            chapter: chapters.len().saturating_sub(1) as u32,
            ..START
        };
    };
    let block = book.chapter(chapter).ok().and_then(|blocks| {
        let mut left = want;
        blocks.blocks.iter().position(|block| {
            let chars = block.chars();
            if left < chars {
                true
            } else {
                left -= chars;
                false
            }
        })
    });
    Position {
        chapter: chapter as u32,
        block: block.unwrap_or(0) as u32,
        offset: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use book_format::{About, Align, Block, BlockKind, Chapter, Limits, Span};

    use super::*;

    fn paragraph(text: &str) -> Block {
        Block {
            kind: BlockKind::Paragraph {
                indent_first: false,
            },
            indent: 0,
            align: Align::Default,
            spans: vec![Span::plain(text.to_owned())],
        }
    }

    /// Two chapters of 100 characters each, in blocks of 25.
    fn book() -> BookReader<Cursor<Vec<u8>>> {
        let chapter = || Chapter {
            blocks: (0..4).map(|_| paragraph(&"x".repeat(25))).collect(),
        };
        let mut bytes = Cursor::new(Vec::new());
        book_format::write(
            &mut bytes,
            About {
                title: "Book".into(),
                ..About::default()
            },
            Vec::new(),
            &[chapter(), chapter()],
            &[],
        )
        .unwrap();
        bytes.set_position(0);
        BookReader::open(bytes, Limits::default()).unwrap()
    }

    fn at(chapter: u32, block: u32) -> Position {
        Position {
            chapter,
            block,
            offset: 0,
        }
    }

    #[test]
    fn a_fraction_finds_its_block() {
        let mut book = book();
        assert_eq!(position_at(&mut book, 0.0), START);
        assert_eq!(position_at(&mut book, 0.3), at(0, 2));
        assert_eq!(position_at(&mut book, 0.5), at(1, 0));
        assert_eq!(position_at(&mut book, 0.9), at(1, 3));
        assert_eq!(position_at(&mut book, 1.0), at(1, 0));
        assert_eq!(position_at(&mut book, f32::NAN), START);
    }

    #[test]
    fn a_place_is_used_as_it_stands_only_in_its_own_file() {
        let mut book = book();
        let here = SavedPlace {
            position: at(0, 3),
            file: 7,
            revision: Some(1),
            progress: 0.75,
            at: 100,
        };
        assert_eq!(here.resolve(&mut book, 7, Some(1)), at(0, 3));
        assert_eq!(
            here.resolve(&mut book, 8, Some(1)),
            at(1, 2),
            "another file"
        );

        let synced = SavedPlace { file: 0, ..here };
        assert_eq!(
            synced.resolve(&mut book, 8, Some(1)),
            at(0, 3),
            "same revision"
        );
        assert_eq!(
            synced.resolve(&mut book, 8, Some(2)),
            at(1, 2),
            "another revision"
        );
    }

    #[test]
    fn places_saved_before_still_read() {
        let old: SavedPlace = serde_json::from_str(r#"{"chapter":1,"block":2}"#).unwrap();
        assert_eq!(old.position, at(1, 2));
        assert!(old.fits(12345, None), "taken as it is");
        let json = serde_json::to_string(&old).unwrap();
        assert_eq!(serde_json::from_str::<SavedPlace>(&json).unwrap(), old);
    }
}
