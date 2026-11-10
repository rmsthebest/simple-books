//! A book on disk is a ZIP: a `mimetype` entry, `manifest.json`, then one entry
//! per chapter and per image. Chapters and images are read only when needed,
//! so opening a book costs one small JSON read.
//!
//! Everything read back is checked against [`Limits`] and for consistency, so
//! the reader can index into a book without checking again.

use std::io::{self, Read, Seek, Write};

use serde::de::DeserializeOwned;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    BlockKind, Chapter, ChapterInfo, FORMAT_VERSION, ImageInfo, ImageKind, MAX_INDENT, Manifest,
    Position, TocEntry,
};

const MIMETYPE: &str = "mimetype";
const MIMETYPE_VALUE: &[u8] = b"application/x-simple-books-book";
const MANIFEST: &str = "manifest.json";

/// What a book may cost to read.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_manifest_bytes: u64,
    pub max_chapter_bytes: u64,
    pub max_image_bytes: u64,
    pub max_chapters: usize,
    pub max_images: usize,
    pub max_image_side: u32,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_manifest_bytes: 4 * MIB,
            max_chapter_bytes: 16 * MIB,
            max_image_bytes: 8 * MIB,
            max_chapters: 5000,
            max_images: 5000,
            max_image_side: 4096,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("not a simple-books book")]
    NotABook,
    #[error("made for format {0}, and this reads format {FORMAT_VERSION}")]
    Version(u32),
    #[error("the book is damaged: {0}")]
    Corrupt(&'static str),
    #[error("the book is too large to read safely")]
    TooLarge,
    #[error("no chapter {0}")]
    NoChapter(usize),
    #[error("no image {0}")]
    NoImage(usize),
    #[error("couldn't read or write the book: {0}")]
    Io(#[from] io::Error),
}

/// An image to store, already scaled and encoded.
#[derive(Debug, Clone)]
pub struct NewImage {
    pub info: ImageInfo,
    pub bytes: Vec<u8>,
}

/// Writes a book. The chapter lengths in the manifest are worked out here, so
/// they can't disagree with the chapters.
///
/// # Errors
///
/// [`FormatError::Corrupt`] if the parts don't make a valid book, such as a
/// link to a chapter that doesn't exist, [`FormatError::TooLarge`] if a part
/// is over the default [`Limits`], or [`FormatError::Io`].
pub fn write<W: Write + Seek>(
    out: W,
    title: String,
    authors: Vec<String>,
    language: Option<String>,
    toc: Vec<TocEntry>,
    chapters: &[Chapter],
    images: &[NewImage],
) -> Result<(), FormatError> {
    let manifest = Manifest {
        format: FORMAT_VERSION,
        title,
        authors,
        language,
        chapters: chapters
            .iter()
            .map(|chapter| ChapterInfo {
                chars: chapter.blocks.iter().map(crate::Block::chars).sum(),
            })
            .collect(),
        toc,
        images: images.iter().map(|image| image.info).collect(),
    };
    let limits = Limits::default();
    check_manifest(&manifest, &limits)?;
    for chapter in chapters {
        check_chapter(chapter, &manifest)?;
    }

    // A fixed timestamp makes the same book produce the same bytes.
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default());
    let deflated = stored.compression_method(CompressionMethod::Deflated);

    let mut zip = ZipWriter::new(out);
    zip.start_file(MIMETYPE, stored).map_err(zip_error)?;
    zip.write_all(MIMETYPE_VALUE)?;

    zip.start_file(MANIFEST, deflated).map_err(zip_error)?;
    zip.write_all(&json_within(&manifest, limits.max_manifest_bytes)?)?;

    for (index, chapter) in chapters.iter().enumerate() {
        let json = json_within(chapter, limits.max_chapter_bytes)?;
        zip.start_file(chapter_name(index), deflated)
            .map_err(zip_error)?;
        zip.write_all(&json)?;
    }
    // Images are compressed already.
    for (index, image) in images.iter().enumerate() {
        if image.bytes.len() as u64 > limits.max_image_bytes {
            return Err(FormatError::TooLarge);
        }
        zip.start_file(image_name(index, image.info.kind), stored)
            .map_err(zip_error)?;
        zip.write_all(&image.bytes)?;
    }
    zip.finish().map_err(zip_error)?;
    Ok(())
}

/// Nothing is written that [`BookReader`] would refuse to read back.
fn json_within<T: serde::Serialize>(value: &T, limit: u64) -> Result<Vec<u8>, FormatError> {
    let json = serde_json::to_vec(value).map_err(io::Error::from)?;
    if json.len() as u64 > limit {
        return Err(FormatError::TooLarge);
    }
    Ok(json)
}

/// An open book.
#[derive(Debug)]
pub struct BookReader<R> {
    zip: ZipArchive<R>,
    manifest: Manifest,
    limits: Limits,
}

impl<R: Read + Seek> BookReader<R> {
    /// Opens a book and reads its manifest.
    ///
    /// # Errors
    ///
    /// Any [`FormatError`], if it isn't a book this can read.
    pub fn open(reader: R, limits: Limits) -> Result<Self, FormatError> {
        let mut zip = ZipArchive::new(reader).map_err(|_| FormatError::NotABook)?;
        let mimetype = read_entry(&mut zip, MIMETYPE, MIMETYPE_VALUE.len() as u64)
            .map_err(|_| FormatError::NotABook)?;
        if mimetype != MIMETYPE_VALUE {
            return Err(FormatError::NotABook);
        }

        let manifest: Manifest = read_json(&mut zip, MANIFEST, limits.max_manifest_bytes)?;
        if manifest.format != FORMAT_VERSION {
            return Err(FormatError::Version(manifest.format));
        }
        check_manifest(&manifest, &limits)?;
        Ok(Self {
            zip,
            manifest,
            limits,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Reads one chapter.
    ///
    /// # Errors
    ///
    /// [`FormatError::NoChapter`] if there's no such chapter, or another
    /// [`FormatError`] if it can't be read.
    pub fn chapter(&mut self, index: usize) -> Result<Chapter, FormatError> {
        if index >= self.manifest.chapters.len() {
            return Err(FormatError::NoChapter(index));
        }
        let chapter: Chapter = read_json(
            &mut self.zip,
            &chapter_name(index),
            self.limits.max_chapter_bytes,
        )?;
        check_chapter(&chapter, &self.manifest)?;
        let chars: u64 = chapter.blocks.iter().map(crate::Block::chars).sum();
        if chars != self.manifest.chapters[index].chars {
            return Err(FormatError::Corrupt("a chapter's length doesn't match"));
        }
        Ok(chapter)
    }

    /// Reads one image's encoded bytes.
    ///
    /// # Errors
    ///
    /// [`FormatError::NoImage`] if there's no such image, or another
    /// [`FormatError`] if it can't be read.
    pub fn image(&mut self, index: usize) -> Result<Vec<u8>, FormatError> {
        let info = self
            .manifest
            .images
            .get(index)
            .ok_or(FormatError::NoImage(index))?;
        read_entry(
            &mut self.zip,
            &image_name(index, info.kind),
            self.limits.max_image_bytes,
        )
    }
}

fn chapter_name(index: usize) -> String {
    format!("chapters/{index}.json")
}

fn image_name(index: usize, kind: ImageKind) -> String {
    let extension = match kind {
        ImageKind::Png => "png",
        ImageKind::Jpeg => "jpg",
    };
    format!("images/{index}.{extension}")
}

fn zip_error(error: zip::result::ZipError) -> FormatError {
    match error {
        zip::result::ZipError::Io(error) => FormatError::Io(error),
        _ => FormatError::Corrupt("the archive"),
    }
}

fn read_entry<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, FormatError> {
    let entry = zip
        .by_name(name)
        .map_err(|_| FormatError::Corrupt("a part is missing"))?;
    if entry.size() > limit {
        return Err(FormatError::TooLarge);
    }
    let mut bytes = Vec::new();
    entry
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FormatError::Corrupt("a part can't be decompressed"))?;
    if bytes.len() as u64 > limit {
        return Err(FormatError::TooLarge);
    }
    Ok(bytes)
}

fn read_json<R: Read + Seek, T: DeserializeOwned>(
    zip: &mut ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<T, FormatError> {
    let bytes = read_entry(zip, name, limit)?;
    serde_json::from_slice(&bytes).map_err(|_| FormatError::Corrupt("a part isn't valid"))
}

fn check_manifest(manifest: &Manifest, limits: &Limits) -> Result<(), FormatError> {
    if manifest.chapters.is_empty() {
        return Err(FormatError::Corrupt("no chapters"));
    }
    if manifest.chapters.len() > limits.max_chapters || manifest.images.len() > limits.max_images {
        return Err(FormatError::TooLarge);
    }
    for image in &manifest.images {
        let sides = [image.width, image.height];
        if sides.contains(&0) {
            return Err(FormatError::Corrupt("an image has no size"));
        }
        if sides.iter().any(|side| *side > limits.max_image_side) {
            return Err(FormatError::TooLarge);
        }
    }
    for entry in &manifest.toc {
        check_position(entry.target, manifest)?;
    }
    Ok(())
}

fn check_chapter(chapter: &Chapter, manifest: &Manifest) -> Result<(), FormatError> {
    for block in &chapter.blocks {
        if block.indent > MAX_INDENT {
            return Err(FormatError::Corrupt("a block is indented too deeply"));
        }
        match &block.kind {
            BlockKind::Heading { level } if !(1..=6).contains(level) => {
                return Err(FormatError::Corrupt("a heading level is out of range"));
            }
            BlockKind::Image { image, .. } if *image as usize >= manifest.images.len() => {
                return Err(FormatError::Corrupt("an image is missing"));
            }
            BlockKind::Image { .. } | BlockKind::Separator if !block.spans.is_empty() => {
                return Err(FormatError::Corrupt("an image or separator has text"));
            }
            _ => {}
        }
        for span in &block.spans {
            if let Some(link) = span.link {
                check_position(link, manifest)?;
            }
        }
    }
    Ok(())
}

/// Checks the chapter only. A block or offset past the end of its chapter is
/// read as the chapter's end, since checking those would mean reading every
/// chapter up front.
fn check_position(position: Position, manifest: &Manifest) -> Result<(), FormatError> {
    if position.chapter as usize >= manifest.chapters.len() {
        return Err(FormatError::Corrupt(
            "a link goes to a chapter that isn't there",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{Align, Block, SOFT_HYPHEN, Span, Style};

    fn paragraph(text: &str) -> Block {
        Block {
            kind: BlockKind::Paragraph { indent_first: true },
            indent: 0,
            align: Align::Default,
            spans: vec![Span::plain(text)],
        }
    }

    fn sample() -> (Vec<Chapter>, Vec<NewImage>, Vec<TocEntry>) {
        let one = Chapter {
            blocks: vec![
                Block {
                    kind: BlockKind::Heading { level: 1 },
                    indent: 0,
                    align: Align::Center,
                    spans: vec![Span::plain("One")],
                },
                Block {
                    kind: BlockKind::Paragraph {
                        indent_first: false,
                    },
                    indent: 0,
                    align: Align::Default,
                    spans: vec![
                        Span::plain(format!("Hy{SOFT_HYPHEN}phen ")),
                        Span {
                            text: "note".to_owned(),
                            style: Style {
                                vertical: crate::Vertical::Super,
                                ..Style::default()
                            },
                            link: Some(Position {
                                chapter: 1,
                                block: 0,
                                offset: 0,
                            }),
                        },
                    ],
                },
                Block {
                    kind: BlockKind::Image {
                        image: 0,
                        alt: "A map".to_owned(),
                    },
                    indent: 0,
                    align: Align::Default,
                    spans: Vec::new(),
                },
            ],
        };
        let two = Chapter {
            blocks: vec![paragraph("The note.")],
        };
        let image = NewImage {
            info: ImageInfo {
                width: 2,
                height: 3,
                kind: ImageKind::Png,
            },
            bytes: vec![1, 2, 3],
        };
        let toc = vec![TocEntry {
            title: "One".to_owned(),
            depth: 0,
            target: Position {
                chapter: 0,
                block: 0,
                offset: 0,
            },
        }];
        (vec![one, two], vec![image], toc)
    }

    fn written(chapters: &[Chapter], images: &[NewImage], toc: Vec<TocEntry>) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        write(
            &mut out,
            "Title".to_owned(),
            vec!["Ann Author".to_owned()],
            Some("en".to_owned()),
            toc,
            chapters,
            images,
        )
        .unwrap();
        out.into_inner()
    }

    #[test]
    fn round_trips() {
        let (chapters, images, toc) = sample();
        let bytes = written(&chapters, &images, toc.clone());

        let mut book = BookReader::open(Cursor::new(bytes), Limits::default()).unwrap();
        let manifest = book.manifest().clone();
        assert_eq!(manifest.title, "Title");
        assert_eq!(manifest.toc, toc);
        // "One", then "Hyphen note" without its soft hyphen.
        assert_eq!(manifest.chapters[0].chars, 3 + 11);
        assert_eq!(book.chapter(0).unwrap(), chapters[0]);
        assert_eq!(book.chapter(1).unwrap(), chapters[1]);
        assert_eq!(book.image(0).unwrap(), images[0].bytes);
        assert!(matches!(book.chapter(2), Err(FormatError::NoChapter(2))));
        assert!(matches!(book.image(1), Err(FormatError::NoImage(1))));
    }

    #[test]
    fn the_same_book_makes_the_same_bytes() {
        let (chapters, images, toc) = sample();
        assert_eq!(
            written(&chapters, &images, toc.clone()),
            written(&chapters, &images, toc)
        );
    }

    #[test]
    fn refuses_to_write_broken_books() {
        let (mut chapters, images, toc) = sample();
        chapters[1].blocks[0].spans[0].link = Some(Position {
            chapter: 9,
            block: 0,
            offset: 0,
        });
        let result = write(
            Cursor::new(Vec::new()),
            String::new(),
            Vec::new(),
            None,
            toc,
            &chapters,
            &images,
        );
        assert!(matches!(result, Err(FormatError::Corrupt(_))));

        let result = write(
            Cursor::new(Vec::new()),
            String::new(),
            Vec::new(),
            None,
            Vec::new(),
            &[],
            &[],
        );
        assert!(
            matches!(result, Err(FormatError::Corrupt(_))),
            "no chapters"
        );
    }

    /// Rewrites one entry of a written book, keeping the others.
    fn replace_entry(book: &[u8], name: &str, contents: &[u8]) -> Vec<u8> {
        let mut source = ZipArchive::new(Cursor::new(book)).unwrap();
        let mut out = ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..source.len() {
            let mut entry = source.by_index(index).unwrap();
            let entry_name = entry.name().unwrap().into_owned();
            out.start_file(entry_name.as_str(), SimpleFileOptions::default())
                .unwrap();
            if entry_name == name {
                out.write_all(contents).unwrap();
            } else {
                io::copy(&mut entry, &mut out).unwrap();
            }
        }
        out.finish().unwrap().into_inner()
    }

    #[test]
    fn refuses_what_it_cant_trust() {
        let (chapters, images, toc) = sample();
        let good = written(&chapters, &images, toc);
        let open = |bytes: Vec<u8>| BookReader::open(Cursor::new(bytes), Limits::default());

        assert!(matches!(
            open(b"not a zip".to_vec()),
            Err(FormatError::NotABook)
        ));
        assert!(matches!(
            open(replace_entry(&good, MIMETYPE, b"application/epub+zip")),
            Err(FormatError::NotABook)
        ));

        let mut book = BookReader::open(Cursor::new(good.clone()), Limits::default()).unwrap();
        let mut manifest = book.manifest().clone();

        manifest.format = FORMAT_VERSION + 1;
        let newer = serde_json::to_vec(&manifest).unwrap();
        assert!(matches!(
            open(replace_entry(&good, MANIFEST, &newer)),
            Err(FormatError::Version(_))
        ));

        // A chapter that doesn't match the manifest's length.
        let mut changed = book.chapter(1).unwrap();
        changed.blocks.push(paragraph("More."));
        let changed = serde_json::to_vec(&changed).unwrap();
        let mut book = open(replace_entry(&good, &chapter_name(1), &changed)).unwrap();
        assert!(matches!(book.chapter(1), Err(FormatError::Corrupt(_))));

        let small = Limits {
            max_manifest_bytes: 10,
            ..Limits::default()
        };
        assert!(matches!(
            BookReader::open(Cursor::new(good), small),
            Err(FormatError::TooLarge)
        ));
    }
}
