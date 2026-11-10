//! What Simple Books puts on a Kobo's drive, as one tar: the server builds
//! it from `scripts/build-kobo-files.sh`'s folder and serves it at
//! `/device/kobo-files`, and the website's installer reads it in the
//! browser and writes each file onto the drive.
//!
//! Only plain ustar is written or read: regular files, with paths relative
//! to the drive's root. Folders aren't entries; they're made for the files
//! in them. A reader is all the installer needs, so there's no dependency
//! that might not build for the browser.

use std::fmt;

const BLOCK: usize = 512;

/// A file, with its path from the drive's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// `/`-separated, as `.adds/simple-books/reader-kobo`.
    pub path: String,
    /// Unix permissions, such as `0o755` for a program. FAT drives don't
    /// keep them, but `tar -t` shows them.
    pub mode: u32,
    pub bytes: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// It ends in the middle of a header or a file.
    Truncated,
    /// A header's checksum is wrong.
    Checksum,
    /// A header isn't ustar.
    NotUstar,
    /// An entry other than a file or a folder, such as a link.
    Unsupported { path: String, kind: u8 },
    /// A path that's absolute, empty, or goes up with `..`.
    BadPath(String),
    /// A path or a file too long for a plain ustar header.
    TooLong(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("the archive is cut short"),
            Self::Checksum => f.write_str("a header in the archive is damaged"),
            Self::NotUstar => f.write_str("the archive isn't a plain tar"),
            Self::Unsupported { path, kind } => {
                write!(
                    f,
                    "{path} in the archive isn't a file (type {:?})",
                    char::from(*kind)
                )
            }
            Self::BadPath(path) => write!(f, "{path:?} isn't a path inside the drive"),
            Self::TooLong(path) => write!(f, "{path} is too long for the archive"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether `path` stays inside the folder it's unpacked into.
pub fn is_safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// A tar of `entries`, in the order given.
pub fn write(entries: &[Entry<'_>]) -> Result<Vec<u8>, Error> {
    let size: usize = entries
        .iter()
        .map(|e| BLOCK + e.bytes.len().div_ceil(BLOCK) * BLOCK)
        .sum();
    let mut out = Vec::with_capacity(size + 2 * BLOCK);
    for entry in entries {
        out.extend_from_slice(&header(entry)?);
        out.extend_from_slice(entry.bytes);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    // Two empty blocks end it.
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}

fn header(entry: &Entry<'_>) -> Result<[u8; BLOCK], Error> {
    let path = entry.path.as_str();
    if !is_safe(path) {
        return Err(Error::BadPath(path.to_owned()));
    }
    let too_long = || Error::TooLong(path.to_owned());
    // Longer paths are split at a `/` into a prefix and a name.
    let (prefix, name) = if path.len() <= 100 {
        ("", path)
    } else {
        let split = path
            .char_indices()
            .filter(|&(at, c)| c == '/' && at <= 155 && path.len() - at - 1 <= 100)
            .map(|(at, _)| at)
            .next()
            .ok_or_else(too_long)?;
        (&path[..split], &path[split + 1..])
    };
    let size = u64::try_from(entry.bytes.len()).map_err(|_| too_long())?;
    if size >= 1 << 33 {
        return Err(too_long());
    }
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], u64::from(entry.mode & 0o7777));
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 0);
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum = checksum(&h);
    octal(&mut h[148..155], u64::from(sum));
    h[155] = b' ';
    Ok(h)
}

/// `value` in octal, zero-padded to fill `field` but its last byte, which
/// is NUL.
fn octal(field: &mut [u8], mut value: u64) {
    let last = field.len() - 1;
    field[last] = 0;
    for byte in field[..last].iter_mut().rev() {
        *byte = b'0' + (value & 7) as u8;
        value >>= 3;
    }
}

/// The header's bytes added up, with the checksum's own as spaces.
fn checksum(header: &[u8; BLOCK]) -> u32 {
    header
        .iter()
        .enumerate()
        .map(|(at, &b)| u32::from(if (148..156).contains(&at) { b' ' } else { b }))
        .sum()
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let digits = field
        .iter()
        .copied()
        .skip_while(|&b| b == b' ')
        .take_while(|&b| b != 0 && b != b' ');
    let mut value: u64 = 0;
    let mut any = false;
    for digit in digits {
        if !(b'0'..=b'7').contains(&digit) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(digit - b'0'))?;
        any = true;
    }
    any.then_some(value)
}

/// The text of a NUL-padded field.
fn text(field: &[u8]) -> Result<&str, Error> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).map_err(|_| Error::NotUstar)
}

/// The files in a tar, in order. Folders are skipped; anything else that
/// isn't a file is an error.
pub fn read(archive: &[u8]) -> Result<Vec<Entry<'_>>, Error> {
    let mut entries = Vec::new();
    let mut at = 0;
    loop {
        let Some(h) = archive.get(at..at + BLOCK) else {
            // Some writers stop after the last file, with no empty blocks.
            return if at == archive.len() {
                Ok(entries)
            } else {
                Err(Error::Truncated)
            };
        };
        if h.iter().all(|&b| b == 0) {
            return Ok(entries);
        }
        let h: &[u8; BLOCK] = h.try_into().map_err(|_| Error::Truncated)?;
        if &h[257..262] != b"ustar" {
            return Err(Error::NotUstar);
        }
        if parse_octal(&h[148..156]) != Some(u64::from(checksum(h))) {
            return Err(Error::Checksum);
        }
        let (prefix, name) = (text(&h[345..500])?, text(&h[..100])?);
        let size = usize::try_from(parse_octal(&h[124..136]).ok_or(Error::NotUstar)?)
            .map_err(|_| Error::Truncated)?;
        let mode = parse_octal(&h[100..108]).ok_or(Error::NotUstar)?;
        at += BLOCK;
        let bytes = archive.get(at..at + size).ok_or(Error::Truncated)?;
        at += size.div_ceil(BLOCK) * BLOCK;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match h[156] {
            b'0' | 0 => {}
            b'5' => continue,
            kind => return Err(Error::Unsupported { path, kind }),
        }
        if !is_safe(&path) {
            return Err(Error::BadPath(path));
        }
        entries.push(Entry {
            path,
            mode: u32::try_from(mode & 0o7777).unwrap_or(0o644),
            bytes,
        });
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn entry(path: &str, mode: u32, bytes: &'static [u8]) -> Entry<'static> {
        Entry {
            path: path.to_owned(),
            mode,
            bytes,
        }
    }

    #[test]
    fn what_is_written_reads_back() {
        let long = format!("{}/{}", "a".repeat(120), "b".repeat(90));
        let entries = vec![
            entry(".adds/simple-books/reader-kobo", 0o755, b"\x7fELF..."),
            entry(".adds/simple-books/version", 0o644, b""),
            entry(".kobo/Kobo.tgz", 0o644, &[7; 1000]),
            entry(&long, 0o644, b"deep"),
        ];
        let archive = write(&entries).unwrap();
        assert_eq!(archive.len() % BLOCK, 0);
        assert_eq!(read(&archive).unwrap(), entries);
    }

    #[test]
    fn headers_are_laid_out_as_tar_expects() {
        let archive = write(&[entry("a/b", 0o755, b"hello")]).unwrap();
        let h = &archive[..BLOCK];
        assert_eq!(&h[..4], b"a/b\0");
        assert_eq!(&h[100..108], b"0000755\0");
        assert_eq!(&h[124..136], b"00000000005\0");
        assert_eq!(h[156], b'0');
        assert_eq!(&h[257..265], b"ustar\x0000");
        assert_eq!(&archive[BLOCK..BLOCK + 5], b"hello");
        // One header, one block of file, two empty blocks.
        assert_eq!(archive.len(), 4 * BLOCK);
    }

    #[test]
    fn unsafe_paths_are_refused_both_ways() {
        for path in [
            "/etc/rc.local",
            "../x",
            "a/../../x",
            "a//b",
            "",
            "a/./b",
            "a\\b",
        ] {
            assert!(!is_safe(path), "{path:?}");
            assert!(matches!(
                write(&[entry(path, 0o644, b"")]),
                Err(Error::BadPath(_))
            ));
        }
        // Written by hand, as another tool might.
        let mut archive = write(&[entry("ok", 0o644, b"")]).unwrap();
        archive[..3].copy_from_slice(b"../");
        let sum = checksum(archive[..BLOCK].try_into().unwrap());
        octal(&mut archive[148..155], u64::from(sum));
        assert_eq!(read(&archive), Err(Error::BadPath("../".to_owned())));
    }

    #[test]
    fn damage_and_links_are_errors() {
        let archive = write(&[entry("a", 0o644, b"hello")]).unwrap();
        let mut damaged = archive.clone();
        damaged[0] = b'b';
        assert_eq!(read(&damaged), Err(Error::Checksum));
        assert_eq!(read(&archive[..BLOCK + 3]), Err(Error::Truncated));
        assert_eq!(read(&archive[..BLOCK - 1]), Err(Error::Truncated));
        let mut link = archive.clone();
        link[156] = b'2';
        let sum = checksum(link[..BLOCK].try_into().unwrap());
        octal(&mut link[148..155], u64::from(sum));
        assert!(matches!(
            read(&link),
            Err(Error::Unsupported { kind: b'2', .. })
        ));
        assert_eq!(read(&[]), Ok(Vec::new()));
    }

    proptest! {
        #[test]
        fn any_files_round_trip(
            files in proptest::collection::vec(
                ("[a-z.-]{1,20}(/[a-z.-]{1,20}){0,3}", proptest::collection::vec(any::<u8>(), 0..2000)),
                0..6,
            )
        ) {
            let entries: Vec<Entry<'_>> = files
                .iter()
                .filter(|(path, _)| is_safe(path))
                .map(|(path, bytes)| Entry { path: path.clone(), mode: 0o644, bytes })
                .collect();
            let archive = write(&entries).unwrap();
            prop_assert_eq!(read(&archive).unwrap(), entries);
        }
    }
}
