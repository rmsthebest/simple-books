//! Files built to hurt, and the ordinary files that must not be caught with them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Write};

use epub::{EpubError, Limits, parse};
use proptest::prelude::*;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

const OPF: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Good Book</dc:title><dc:creator>Ann</dc:creator><dc:language>en</dc:language>
</metadata>
<manifest>
<item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
<item id="img" href="cover.jpg" media-type="image/jpeg" properties="cover-image"/>
</manifest>
<spine><itemref idref="c1"/></spine>
</package>"#;

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

fn stored() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn build(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in entries {
        zip.start_file(*name, options()).unwrap();
        zip.write_all(body).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// A valid book, with some entries replaced or added.
fn with(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let base: [(&str, &[u8]); 5] = [
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("OEBPS/content.opf", OPF.as_bytes()),
        ("OEBPS/c1.xhtml", b"<html><body>Hello</body></html>"),
        ("OEBPS/cover.jpg", b"pretend jpeg"),
    ];
    let mut entries: Vec<(&str, &[u8])> = base
        .into_iter()
        .filter(|(name, _)| !extra.iter().any(|(other, _)| other == name))
        .collect();
    entries.extend_from_slice(extra);
    build(&entries)
}

fn without(name: &str) -> Vec<u8> {
    let base: [(&str, &[u8]); 5] = [
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("OEBPS/content.opf", OPF.as_bytes()),
        ("OEBPS/c1.xhtml", b"<html><body>Hello</body></html>"),
        ("OEBPS/cover.jpg", b"pretend jpeg"),
    ];
    let kept: Vec<(&str, &[u8])> = base.into_iter().filter(|(n, _)| *n != name).collect();
    build(&kept)
}

fn check(bytes: Vec<u8>) -> Result<epub::Book, EpubError> {
    parse(Cursor::new(bytes), &Limits::default())
}

fn check_with(bytes: Vec<u8>, limits: &Limits) -> Result<epub::Book, EpubError> {
    parse(Cursor::new(bytes), limits)
}

fn opf_with(metadata: &str) -> String {
    OPF.replace("<dc:title>Good Book</dc:title>", metadata)
}

/// Finds each central directory record for an entry, and each local header.
fn positions(bytes: &[u8], signature: [u8; 4]) -> Vec<usize> {
    bytes
        .windows(4)
        .enumerate()
        .filter(|(_, window)| **window == signature)
        .map(|(at, _)| at)
        .collect()
}

fn central_records(bytes: &[u8]) -> Vec<usize> {
    positions(bytes, *b"PK\x01\x02")
}

fn local_headers(bytes: &[u8]) -> Vec<usize> {
    positions(bytes, *b"PK\x03\x04")
}

fn eocd(bytes: &[u8]) -> usize {
    *positions(bytes, *b"PK\x05\x06").last().unwrap()
}

#[test]
fn an_ordinary_book_passes() {
    let book = check(with(&[])).unwrap();
    assert_eq!(book.metadata.title.as_deref(), Some("Good Book"));
    assert_eq!(book.metadata.authors, ["Ann"]);
    assert_eq!(book.cover.unwrap().0, b"pretend jpeg");
}

#[test]
fn ordinary_variations_pass() {
    // No mimetype entry: wrong by the spec, but common, and harmless.
    assert!(check(without("mimetype")).is_ok());
    // A missing cover file leaves it without a cover, not refused.
    assert!(check(without("OEBPS/cover.jpg")).unwrap().cover.is_none());

    // No title at all: the uploader's filename can stand in.
    let opf = opf_with("");
    let book = check(with(&[("OEBPS/content.opf", opf.as_bytes())])).unwrap();
    assert_eq!(book.metadata.title, None);

    // Directory entries, and a package at the top level.
    let container = CONTAINER.replace("OEBPS/content.opf", "content.opf");
    let opf = OPF
        .replace("cover.jpg", "OEBPS/cover.jpg")
        .replace("c1.xhtml", "OEBPS/c1.xhtml");
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_directory("OEBPS/", options()).unwrap();
    for (name, body) in [
        ("META-INF/container.xml", container.as_bytes()),
        ("content.opf", opf.as_bytes()),
        ("OEBPS/c1.xhtml", b"<html/>".as_slice()),
    ] {
        zip.start_file(name, options()).unwrap();
        zip.write_all(body).unwrap();
    }
    assert!(check(zip.finish().unwrap().into_inner()).is_ok());
}

#[test]
fn font_obfuscation_is_not_drm() {
    let encryption = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
        xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
        </enc:EncryptedData></encryption>"#;
    assert!(check(with(&[("META-INF/encryption.xml", encryption)])).is_ok());
}

#[test]
fn real_encryption_is_drm() {
    let encryption = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
        xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
        </enc:EncryptedData></encryption>"#;
    let result = check(with(&[("META-INF/encryption.xml", encryption)]));
    assert!(matches!(result, Err(EpubError::Drm)), "{result:?}");
}

#[test]
fn drm_marker_files_are_drm() {
    for name in ["META-INF/rights.xml", "META-INF/sinf.xml"] {
        let result = check(with(&[(name, b"<rights/>")]));
        assert!(matches!(result, Err(EpubError::Drm)), "{name}: {result:?}");
    }
}

#[test]
fn not_a_zip() {
    for bytes in [
        Vec::new(),
        b"short".to_vec(),
        b"<html>definitely not a zip</html>".repeat(10),
        vec![0u8; 100_000],
    ] {
        let result = check(bytes);
        assert!(matches!(result, Err(EpubError::NotAnArchive)), "{result:?}");
    }
}

#[test]
fn a_zip_that_isnt_a_book() {
    for (what, bytes) in [
        ("no container", without("META-INF/container.xml")),
        ("no package", without("OEBPS/content.opf")),
        ("empty", build(&[])),
    ] {
        let result = check(bytes);
        assert!(
            matches!(result, Err(EpubError::NoContent)),
            "{what}: {result:?}"
        );
    }

    let no_rootfile: &[u8] = b"<container><rootfiles/></container>";
    let result = check(with(&[("META-INF/container.xml", no_rootfile)]));
    assert!(matches!(result, Err(EpubError::NoContent)), "{result:?}");

    let empty_spine = OPF.replace(r#"<itemref idref="c1"/>"#, "");
    let result = check(with(&[("OEBPS/content.opf", empty_spine.as_bytes())]));
    assert!(matches!(result, Err(EpubError::NoContent)), "{result:?}");

    let dangling = OPF.replace(r#"idref="c1""#, r#"idref="nope""#);
    let result = check(with(&[("OEBPS/content.opf", dangling.as_bytes())]));
    assert!(matches!(result, Err(EpubError::NoContent)), "{result:?}");

    // The spine points at a file that isn't there.
    let result = check(without("OEBPS/c1.xhtml"));
    assert!(matches!(result, Err(EpubError::NoContent)), "{result:?}");
}

#[test]
fn traversal_and_absolute_names() {
    for name in [
        "../../evil",
        "OEBPS/../../evil",
        "/etc/passwd",
        "..\\evil",
        "a//b",
        "C:evil",
    ] {
        // Written by patching, since the writer may tidy names.
        let placeholder = "X".repeat(name.len());
        let mut bytes = with(&[(placeholder.as_str(), b"x")]);
        let mut at = 0;
        let mut patched = 0;
        while let Some(found) = bytes[at..]
            .windows(placeholder.len())
            .position(|window| window == placeholder.as_bytes())
        {
            bytes[at + found..at + found + name.len()].copy_from_slice(name.as_bytes());
            at += found + name.len();
            patched += 1;
        }
        assert_eq!(patched, 2, "a local header and a central record");
        let result = check(bytes);
        assert!(
            matches!(result, Err(EpubError::Unsafe(_))),
            "{name}: {result:?}"
        );
    }
}

#[test]
fn duplicate_and_case_colliding_names() {
    // Same-length names, patched into place after writing.
    let mut same = with(&[("OEBPS/aaaa.xhtml", b"a"), ("OEBPS/bbbb.xhtml", b"b")]);
    let from = b"OEBPS/bbbb.xhtml";
    let mut at = 0;
    while let Some(found) = same[at..].windows(from.len()).position(|w| w == from) {
        same[at + found..at + found + from.len()].copy_from_slice(b"OEBPS/aaaa.xhtml");
        at += found + from.len();
    }
    let result = check(same);
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");

    let folded = with(&[("OEBPS/Chapter.xhtml", b"a"), ("OEBPS/chapter.xhtml", b"b")]);
    let result = check(folded);
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");
}

#[test]
fn symbolic_links() {
    let mut zip = ZipWriter::new(Cursor::new(with(&[])));
    zip = ZipWriter::new_append(zip.finish().unwrap()).unwrap();
    zip.add_symlink("OEBPS/link", "/etc/passwd", options())
        .unwrap();
    let result = check(zip.finish().unwrap().into_inner());
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");
}

#[test]
fn encrypted_entries() {
    let mut bytes = with(&[]);
    // Bit 0 of the general purpose flags, in the central directory.
    let record = central_records(&bytes)[3];
    bytes[record + 8] |= 1;
    let result = check(bytes);
    assert!(matches!(result, Err(EpubError::Drm)), "{result:?}");
}

#[test]
fn other_compression_methods() {
    for method in [12u8, 14, 93, 99] {
        let mut bytes = with(&[]);
        let record = central_records(&bytes)[3];
        bytes[record + 10] = method;
        let local = local_headers(&bytes)[3];
        bytes[local + 8] = method;
        let result = check(bytes);
        assert!(result.is_err(), "method {method} should be refused");
    }
}

#[test]
fn a_bomb_stops_at_the_entry_limit() {
    let limits = Limits {
        max_entry_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let zeros = vec![0u8; 2 * 1024 * 1024];
    let result = check_with(with(&[("OEBPS/bomb.xhtml", &zeros)]), &limits);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

#[test]
fn many_small_entries_stop_at_the_total_limit() {
    let limits = Limits {
        max_total_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let chunk = vec![7u8; 300 * 1024];
    let names: Vec<String> = (0..5).map(|i| format!("OEBPS/{i}.bin")).collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), chunk.as_slice()))
        .collect();
    let result = check_with(with(&entries), &limits);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

#[test]
fn an_extreme_ratio_is_refused_above_the_floor_only() {
    let limits = Limits {
        max_ratio: 50,
        ratio_floor_bytes: 64 * 1024,
        ..Limits::default()
    };
    // Compressible, and past the floor.
    let zeros = vec![0u8; 1024 * 1024];
    let result = check_with(with(&[("OEBPS/z.bin", &zeros)]), &limits);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");

    // Just as compressible, but small.
    let small = vec![0u8; 32 * 1024];
    assert!(check_with(with(&[("OEBPS/z.bin", &small)]), &limits).is_ok());
}

/// The check that matters: a header claiming a small size can't get past limits
/// that the real output breaks.
#[test]
fn headers_that_lie_about_size_are_caught() {
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let mut bytes = with(&[("OEBPS/liar.bin", &zeros)]);
    let record = *central_records(&bytes).last().unwrap();
    bytes[record + 24..record + 28].copy_from_slice(&100u32.to_le_bytes());
    let local = *local_headers(&bytes).last().unwrap();
    bytes[local + 22..local + 26].copy_from_slice(&100u32.to_le_bytes());

    // It doesn't matter which check stops it, only that the full 8 MiB don't pass.
    let result = check(bytes);
    assert!(
        matches!(result, Err(EpubError::Corrupt | EpubError::TooLarge)),
        "{result:?}"
    );

    // And a limit below the true size stops it, whatever the header says.
    let mut bytes = with(&[("OEBPS/liar.bin", &zeros)]);
    let record = *central_records(&bytes).last().unwrap();
    bytes[record + 24..record + 28].copy_from_slice(&100u32.to_le_bytes());
    let limits = Limits {
        max_entry_bytes: 1024,
        ..Limits::default()
    };
    assert!(check_with(bytes, &limits).is_err());
}

#[test]
fn flipped_bits_are_caught_by_the_checksum() {
    let body = vec![b'x'; 4096];
    let mut zip = ZipWriter::new(Cursor::new(with(&[])));
    zip = ZipWriter::new_append(zip.finish().unwrap()).unwrap();
    zip.start_file("OEBPS/stored.bin", stored()).unwrap();
    zip.write_all(&body).unwrap();
    let mut bytes = zip.finish().unwrap().into_inner();

    let at = bytes.windows(100).position(|w| w == &body[..100]).unwrap();
    bytes[at + 10] ^= 0xFF;
    let result = check(bytes);
    assert!(matches!(result, Err(EpubError::Corrupt)), "{result:?}");
}

#[test]
fn too_many_entries() {
    let limits = Limits {
        max_entries: 8,
        ..Limits::default()
    };
    let names: Vec<String> = (0..10).map(|i| format!("OEBPS/{i}.bin")).collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), b"x".as_slice()))
        .collect();
    let result = check_with(with(&entries), &limits);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

/// The count has to be refused from the footer alone, before anything allocates.
#[test]
fn a_forged_entry_count_is_refused() {
    let mut bytes = with(&[]);
    let end = eocd(&bytes);
    bytes[end + 8..end + 10].copy_from_slice(&60_000u16.to_le_bytes());
    bytes[end + 10..end + 12].copy_from_slice(&60_000u16.to_le_bytes());
    let result = check(bytes);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

#[test]
fn a_second_end_record_hidden_in_the_comment_counts_too() {
    let mut bytes = with(&[]);
    let mut fake = b"PK\x05\x06".to_vec();
    fake.extend_from_slice(&[0, 0, 0, 0]);
    fake.extend_from_slice(&60_000u16.to_le_bytes());
    fake.extend_from_slice(&60_000u16.to_le_bytes());
    fake.extend_from_slice(&[0; 10]);
    let end = eocd(&bytes);
    let comment_len = u16::try_from(fake.len()).unwrap();
    bytes[end + 20..end + 22].copy_from_slice(&comment_len.to_le_bytes());
    bytes.extend_from_slice(&fake);
    let result = check(bytes);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

#[test]
fn overlapping_entries_are_refused() {
    // Point the last two central records at the same local header, as the
    // non-recursive ZIP bombs do.
    let mut bytes = with(&[("OEBPS/a.bin", b"aaaa"), ("OEBPS/b.bin", b"aaaa")]);
    let records = central_records(&bytes);
    let last = records[records.len() - 1];
    let previous = records[records.len() - 2];
    let mut offset = [0u8; 4];
    offset.copy_from_slice(&bytes[previous + 42..previous + 46]);
    bytes[last + 42..last + 46].copy_from_slice(&offset);
    let result = check(bytes);
    assert!(result.is_err(), "{result:?}");
}

#[test]
fn billion_laughs_is_refused_without_being_expanded() {
    let laughs = r#"<?xml version="1.0"?>
<!DOCTYPE package [
 <!ENTITY a "aaaaaaaaaa">
 <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
 <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
 <!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">
]>
<package><metadata><title xmlns="http://purl.org/dc/elements/1.1/">&d;</title></metadata></package>"#;
    let result = check(with(&[("OEBPS/content.opf", laughs.as_bytes())]));
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");

    let result = check(with(&[("META-INF/container.xml", laughs.as_bytes())]));
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");

    let result = check(with(&[("META-INF/encryption.xml", laughs.as_bytes())]));
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");
}

#[test]
fn external_entities_are_refused() {
    let xxe = r#"<!DOCTYPE p [<!ENTITY x SYSTEM "file:///etc/passwd">]>
<package><metadata><title xmlns="http://purl.org/dc/elements/1.1/">&x;</title></metadata></package>"#;
    let result = check(with(&[("OEBPS/content.opf", xxe.as_bytes())]));
    assert!(matches!(result, Err(EpubError::Unsafe(_))), "{result:?}");
}

#[test]
fn oversized_xml_is_refused() {
    let limits = Limits {
        max_xml_bytes: 1024,
        ..Limits::default()
    };
    let opf = format!("{OPF}<!-- {} -->", "x".repeat(2048));
    let result = check_with(with(&[("OEBPS/content.opf", opf.as_bytes())]), &limits);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

#[test]
fn an_oversized_cover_is_refused() {
    let limits = Limits {
        max_cover_bytes: 1024,
        ..Limits::default()
    };
    let cover = vec![1u8; 2048];
    let result = check_with(with(&[("OEBPS/cover.jpg", &cover)]), &limits);
    assert!(matches!(result, Err(EpubError::TooLarge)), "{result:?}");
}

#[test]
fn a_cover_outside_the_book_is_ignored() {
    let opf = OPF.replace(r#"href="cover.jpg""#, r#"href="../../../../etc/passwd""#);
    let book = check(with(&[("OEBPS/content.opf", opf.as_bytes())])).unwrap();
    assert!(book.cover.is_none());
}

#[test]
fn malformed_package_xml_is_refused() {
    let result = check(with(&[(
        "OEBPS/content.opf",
        b"<package><metadata></wrong>",
    )]));
    assert!(matches!(result, Err(EpubError::Corrupt)), "{result:?}");
}

#[test]
fn deep_nesting_is_harmless() {
    let deep = format!("{}{}", "<a>".repeat(100_000), "</a>".repeat(100_000));
    let opf = OPF.replace("<manifest>", &format!("{deep}<manifest>"));
    let _ = check(with(&[("OEBPS/content.opf", opf.as_bytes())]));
}

fn truncations() -> impl Strategy<Value = usize> {
    0usize..2000
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    /// Whatever the damage, the answer is a result, never a panic or a hang.
    #[test]
    fn damaged_files_never_panic(flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..16)) {
        let mut bytes = with(&[]);
        let len = bytes.len();
        for (at, value) in flips {
            bytes[at % len] = value;
        }
        let _ = check(bytes);
    }

    #[test]
    fn truncated_files_never_panic(cut in truncations()) {
        let mut bytes = with(&[]);
        bytes.truncate(cut.min(bytes.len()));
        let _ = check(bytes);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = check(bytes);
    }

    #[test]
    fn arbitrary_package_xml_never_panics(opf in ".{0,500}") {
        let _ = check(with(&[("OEBPS/content.opf", opf.as_bytes())]));
    }
}
