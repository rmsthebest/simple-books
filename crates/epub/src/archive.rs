//! The ZIP layer. It decides what's acceptable before any entry is decompressed,
//! then checks every limit again against what decompressing really produces.

use std::{
    collections::HashSet,
    io::{self, Read, Seek, SeekFrom},
};

use zip::{CompressionMethod, ZipArchive};

use crate::{EpubError, Limits};

/// The smallest possible local file header.
const LOCAL_HEADER_MIN_BYTES: u64 = 30;

const EOCD_SIGNATURE: [u8; 4] = *b"PK\x05\x06";
const EOCD_LEN: u64 = 22;
const MAX_COMMENT: u64 = 65_535;
const ZIP64_LOCATOR_LEN: u64 = 20;
const MAX_NAME_BYTES: usize = 1024;

/// An archive whose directory has passed inspection.
pub(crate) struct Archive<'l, R> {
    zip: ZipArchive<R>,
    limits: &'l Limits,
    names: HashSet<String>,
}

impl<'l, R: Read + Seek> Archive<'l, R> {
    pub(crate) fn open(mut reader: R, limits: &'l Limits) -> Result<Self, EpubError> {
        // Building the archive's index allocates per entry, so a forged count has
        // to be caught first.
        let declared = check_entry_count(&mut reader, limits)?;
        reader.seek(SeekFrom::Start(0)).map_err(EpubError::Io)?;

        let mut zip = ZipArchive::new(reader).map_err(|_| EpubError::NotAnArchive)?;
        if zip.len() as u64 > limits.max_entries {
            return Err(EpubError::TooLarge);
        }
        // The index keeps one entry per name, so a duplicate shows up only as a
        // shortfall. It has to be refused: another reader might pick the other
        // copy, and that one hasn't been checked.
        if !declared.contains(&(zip.len() as u64)) {
            return Err(EpubError::Unsafe("two files with the same name"));
        }
        let names = inspect_directory(&mut zip, limits)?;
        Ok(Self { zip, limits, names })
    }

    pub(crate) fn limits(&self) -> &Limits {
        self.limits
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Decompresses everything once. This enforces the real sizes, and the ZIP
    /// reader's checksum check, on every entry, including ones nothing else reads.
    pub(crate) fn verify_contents(&mut self) -> Result<(), EpubError> {
        let mut total = 0u64;
        for index in 0..self.zip.len() {
            let mut entry = self.zip.by_index(index).map_err(|_| EpubError::Corrupt)?;
            if entry.is_dir() {
                continue;
            }
            let declared = entry.size();
            let compressed = entry.compressed_size();

            let allowed = self
                .limits
                .max_entry_bytes
                .min(self.limits.max_total_bytes - total);
            let produced = io::copy(&mut (&mut entry).take(allowed + 1), &mut io::sink())
                .map_err(|_| EpubError::Corrupt)?;
            if produced > allowed {
                return Err(EpubError::TooLarge);
            }
            if produced != declared {
                return Err(EpubError::Corrupt);
            }
            if produced > self.limits.ratio_floor_bytes
                && produced / compressed.max(1) > self.limits.max_ratio
            {
                return Err(EpubError::TooLarge);
            }
            total += produced;
        }
        Ok(())
    }

    /// Reads one entry into memory, up to `limit` bytes. `None` if it isn't there.
    pub(crate) fn read(&mut self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, EpubError> {
        if !self.contains(name) {
            return Ok(None);
        }
        let entry = self.zip.by_name(name).map_err(|_| EpubError::Corrupt)?;
        let mut bytes = Vec::new();
        entry
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| EpubError::Corrupt)?;
        if bytes.len() as u64 > limit {
            return Err(EpubError::TooLarge);
        }
        Ok(Some(bytes))
    }
}

/// Reads the entry count from the end-of-central-directory record, without
/// trusting it to be well formed. Every candidate record has to be acceptable,
/// since a parser may settle on any of them. Returns the count from each.
fn check_entry_count<R: Read + Seek>(
    reader: &mut R,
    limits: &Limits,
) -> Result<Vec<u64>, EpubError> {
    let len = reader.seek(SeekFrom::End(0)).map_err(EpubError::Io)?;
    if len < EOCD_LEN {
        return Err(EpubError::NotAnArchive);
    }
    let window = (EOCD_LEN + MAX_COMMENT).min(len);
    let start = len - window;
    reader.seek(SeekFrom::Start(start)).map_err(EpubError::Io)?;
    let mut tail = Vec::new();
    reader
        .by_ref()
        .take(window)
        .read_to_end(&mut tail)
        .map_err(EpubError::Io)?;

    let mut counts = Vec::new();
    for at in 0..tail.len().saturating_sub(EOCD_LEN as usize - 1) {
        if tail[at..].starts_with(&EOCD_SIGNATURE) {
            let count = u16::from_le_bytes([tail[at + 10], tail[at + 11]]);
            if count == u16::MAX {
                counts.push(zip64_count(reader, start + at as u64, limits)?.unwrap_or(u64::MAX));
            } else if u64::from(count) > limits.max_entries {
                return Err(EpubError::TooLarge);
            } else {
                counts.push(u64::from(count));
            }
        }
    }
    if counts.is_empty() {
        Err(EpubError::NotAnArchive)
    } else {
        Ok(counts)
    }
}

/// A count of 0xFFFF means the real one is in a ZIP64 record, found via a locator
/// just before the end record. An EPUB never needs one, but it isn't wrong as such.
fn zip64_count<R: Read + Seek>(
    reader: &mut R,
    eocd_at: u64,
    limits: &Limits,
) -> Result<Option<u64>, EpubError> {
    let Some(locator_at) = eocd_at.checked_sub(ZIP64_LOCATOR_LEN) else {
        return Ok(None);
    };
    reader
        .seek(SeekFrom::Start(locator_at))
        .map_err(EpubError::Io)?;
    let mut locator = [0u8; 20];
    if reader.read_exact(&mut locator).is_err() || !locator.starts_with(b"PK\x06\x07") {
        return Ok(None);
    }
    let mut offset = [0u8; 8];
    offset.copy_from_slice(&locator[8..16]);

    let mut record = [0u8; 56];
    if reader
        .seek(SeekFrom::Start(u64::from_le_bytes(offset)))
        .is_err()
        || reader.read_exact(&mut record).is_err()
        || !record.starts_with(b"PK\x06\x06")
    {
        return Err(EpubError::Corrupt);
    }
    let mut count = [0u8; 8];
    count.copy_from_slice(&record[32..40]);
    let count = u64::from_le_bytes(count);
    if count > limits.max_entries {
        return Err(EpubError::TooLarge);
    }
    Ok(Some(count))
}

fn inspect_directory<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    limits: &Limits,
) -> Result<HashSet<String>, EpubError> {
    let mut names = HashSet::with_capacity(zip.len());
    let mut folded = HashSet::with_capacity(zip.len());
    let mut spans = Vec::with_capacity(zip.len());
    let mut declared_total = 0u64;

    for index in 0..zip.len() {
        // Raw: this looks at the entry without trying to decompress or decrypt it.
        let entry = zip.by_index_raw(index).map_err(|_| EpubError::Corrupt)?;
        let name = entry.name().map_err(|_| EpubError::Corrupt)?.into_owned();
        check_name(&name)?;

        if entry.encrypted() {
            return Err(EpubError::Drm);
        }
        if entry.is_symlink() || is_symlink_mode(entry.unix_mode()) {
            return Err(EpubError::Unsafe("a symbolic link"));
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(EpubError::Unsafe("an unsupported kind of compression"));
        }
        if entry.size() > limits.max_entry_bytes {
            return Err(EpubError::TooLarge);
        }
        declared_total = declared_total.saturating_add(entry.size());
        if declared_total > limits.max_total_bytes {
            return Err(EpubError::TooLarge);
        }

        spans.push((entry.header_start(), entry.compressed_size()));

        if !folded.insert(name.to_lowercase()) {
            return Err(EpubError::Unsafe("two files with the same name"));
        }
        if !entry.is_dir() {
            names.insert(name);
        }
    }
    check_no_overlap(spans)?;
    Ok(names)
}

/// Overlapping entries are how the worst ZIP bombs get their ratio. No normal
/// tool produces them.
fn check_no_overlap(mut spans: Vec<(u64, u64)>) -> Result<(), EpubError> {
    spans.sort_unstable();
    for pair in spans.windows(2) {
        let (start, compressed) = pair[0];
        let (next, _) = pair[1];
        let end = start
            .saturating_add(LOCAL_HEADER_MIN_BYTES)
            .saturating_add(compressed);
        if end > next {
            return Err(EpubError::Unsafe("overlapping files"));
        }
    }
    Ok(())
}

fn is_symlink_mode(mode: Option<u32>) -> bool {
    const FILE_TYPE_MASK: u32 = 0o170_000;
    const SYMLINK: u32 = 0o120_000;
    mode.is_some_and(|mode| mode & FILE_TYPE_MASK == SYMLINK)
}

/// Nothing here gets extracted, but kepubify and other readers see these names,
/// so a name has to be safe for anything that handles it.
fn check_name(name: &str) -> Result<(), EpubError> {
    const UNSAFE_NAME: EpubError = EpubError::Unsafe("a file name that points outside the book");

    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(UNSAFE_NAME);
    }
    if name.starts_with('/') || name.contains('\\') || name.chars().any(char::is_control) {
        return Err(UNSAFE_NAME);
    }
    let path = name.strip_suffix('/').unwrap_or(name);
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(UNSAFE_NAME);
        }
    }
    // A drive letter, as in C:evil.
    let mut chars = name.chars();
    if chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':') {
        return Err(UNSAFE_NAME);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in [
            "mimetype",
            "META-INF/container.xml",
            "OEBPS/Text/chapter 1.xhtml",
            "OEBPS/Images/",
            "OEBPS/Ünï-cödé.xhtml",
            "a..b/c..d.html",
        ] {
            assert!(check_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn refuses_names_that_escape() {
        let long = "x".repeat(MAX_NAME_BYTES + 1);
        for name in [
            "",
            "/etc/passwd",
            "../evil",
            "a/../../evil",
            "a/./b",
            "a//b",
            "..",
            "a\\b",
            "..\\evil",
            "a\0b",
            "a\nb",
            "C:evil",
            "c:/evil",
            long.as_str(),
        ] {
            assert!(check_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn spots_overlap_but_allows_adjacent_entries() {
        assert!(check_no_overlap(vec![(0, 10), (40, 10)]).is_ok());
        assert!(
            check_no_overlap(vec![(40, 10), (0, 10)]).is_ok(),
            "any order"
        );
        assert!(check_no_overlap(vec![(0, 11), (40, 10)]).is_err());
        assert!(check_no_overlap(vec![(0, 10), (0, 10)]).is_err());
        assert!(check_no_overlap(vec![(0, u64::MAX), (40, 10)]).is_err());
    }

    #[test]
    fn spots_symlink_modes() {
        assert!(is_symlink_mode(Some(0o120_777)));
        assert!(!is_symlink_mode(Some(0o100_644)));
        assert!(!is_symlink_mode(Some(0o040_755)));
        assert!(!is_symlink_mode(None));
    }
}
