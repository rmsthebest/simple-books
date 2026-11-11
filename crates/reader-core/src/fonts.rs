//! The fonts: the bundled ones, and any found in the folders the platform
//! names ([`find_fonts`]), such as the Kobo's own and those put in `fonts/`
//! on its storage. Fontique's system-font discovery stays off, so nothing
//! looks for fontconfig on the device and the desktop draws the same pages.
//!
//! Finding fonts reads only each file's table directory and `name` table, so
//! it's quick however many there are. A family is read in full only once a
//! book is set in it.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, PoisonError},
    time::UNIX_EPOCH,
};

use skrifa::{
    FontRef, MetadataProvider, Tag,
    instance::{LocationRef, Size},
    raw::{FontData, FontRead, tables::name::Name, types::NameId},
    string::StringId,
};

use parley::{
    FontContext, FontStyle, FontWeight,
    fontique::{Blob, Collection, CollectionOptions, FontInfoOverride, SourceCache},
};

use crate::Fnv;

/// Literata, OFL 1.1 (see `fonts/Literata-OFL.txt`). Variable in weight
/// and optical size.
static LITERATA: &[u8] = include_bytes!("../fonts/Literata.ttf");
static LITERATA_ITALIC: &[u8] = include_bytes!("../fonts/Literata-Italic.ttf");
/// Intel One Mono 1.5, OFL 1.1 (see `fonts/IntelOneMono-OFL.txt`), for code.
static MONO: [&[u8]; 4] = [
    include_bytes!("../fonts/IntelOneMono-Regular.ttf"),
    include_bytes!("../fonts/IntelOneMono-Italic.ttf"),
    include_bytes!("../fonts/IntelOneMono-Bold.ttf"),
    include_bytes!("../fonts/IntelOneMono-BoldItalic.ttf"),
];

pub(crate) const BODY_FAMILY: &str = "Literata";
pub(crate) const MONO_FAMILY: &str = "Intel One Mono";
/// Code is set this much smaller than the text around it. A monospace font's
/// letters are wide, so at the same size code looks louder than prose.
pub(crate) const MONO_SCALE: f32 = 0.9;

/// The fonts a [`crate::Typesetter`] sets text in.
pub(crate) struct Fonts {
    pub(crate) context: FontContext,
    /// The body text's family, as fontique knows it: Literata, or the
    /// chosen family if it could be read.
    pub(crate) body: String,
    /// Whether the body family can be drawn at any weight, so the reader's
    /// weight setting does something.
    pub(crate) varies_in_weight: bool,
}

/// The bundled fonts, with the body text in `family` if it's one
/// [`find_fonts`] found and its files can still be read. Otherwise in
/// Literata.
pub(crate) fn context(family: Option<&str>) -> Fonts {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    for data in [LITERATA, LITERATA_ITALIC].into_iter().chain(MONO) {
        collection.register_fonts(Blob::new(Arc::new(data)), None);
    }
    let mut body = BODY_FAMILY.to_owned();
    let mut varies_in_weight = true;
    if let Some((wanted, loaded)) = family.and_then(|f| Some((f, load(f)?))) {
        let mut names = Vec::new();
        for face in loaded.faces.iter() {
            let correction = named_style(face).map(|(weight, style)| FontInfoOverride {
                weight: Some(weight),
                style,
                ..FontInfoOverride::default()
            });
            for (id, _) in collection.register_fonts(face.clone(), correction) {
                if let Some(name) = collection.family_name(id) {
                    names.push(name.to_owned());
                }
            }
        }
        // Fontique's own name for it, which is the one it matches.
        if let Some(name) = names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(wanted))
            .or(names.first())
        {
            body.clone_from(name);
            varies_in_weight = loaded.varies_in_weight;
        }
    }
    Fonts {
        context: FontContext {
            collection,
            source_cache: SourceCache::default(),
        },
        body,
        varies_in_weight,
    }
}

/// The weight and slope a face's style name gives, such as "Bold Italic",
/// when its `OS/2` table says otherwise. Some fonts' regular and bold faces
/// both say they're regular, and then the bold one would be used for both.
fn named_style(face: &Blob<u8>) -> Option<(FontWeight, Option<FontStyle>)> {
    let font = FontRef::from_index(face.as_ref(), 0).ok()?;
    if font.axes().get_by_tag(Tag::new(b"wght")).is_some() {
        return None;
    }
    let style = [
        StringId::TYPOGRAPHIC_SUBFAMILY_NAME,
        StringId::SUBFAMILY_NAME,
    ]
    .into_iter()
    .find_map(|id| font.localized_strings(id).english_or_first())?
    .to_string()
    .to_ascii_lowercase();
    let said = font.attributes();
    let bold = style.contains("bold") && said.weight.value() < 600.0;
    let slanted = (style.contains("italic") || style.contains("oblique"))
        && said.style == skrifa::attribute::Style::Normal;
    (bold || slanted).then(|| {
        let weight = if bold { 700.0 } else { said.weight.value() };
        (
            FontWeight::new(weight),
            slanted.then_some(FontStyle::Italic),
        )
    })
}

/// A character's advance in a family's first face, at its default
/// variation, per pixel of font size.
pub(crate) fn advance_em(fonts: &mut FontContext, family: &str, c: char) -> Option<f32> {
    let family = fonts.collection.family_by_name(family)?;
    let font = family.fonts().first()?.clone();
    let blob = font.load(Some(&mut fonts.source_cache))?;
    let font = FontRef::from_index(blob.as_ref(), font.index()).ok()?;
    let glyph = font.charmap().map(c)?;
    font.glyph_metrics(Size::new(1.0), LocationRef::default())
        .advance_width(glyph)
}

/// A family found in a font folder.
#[derive(Debug, Clone)]
struct Found {
    files: Vec<PathBuf>,
    varies_in_weight: bool,
    /// Changes when any of its files does, so pages laid out with an older
    /// one aren't used.
    fingerprint: u64,
}

#[derive(Debug, Default)]
struct Shelf {
    families: BTreeMap<String, Found>,
    /// The last few families read in full, newest last.
    loaded: Vec<(String, Arc<Vec<Blob<u8>>>)>,
}

static SHELF: LazyLock<Mutex<Shelf>> = LazyLock::new(Mutex::default);

/// Families kept read in full: the book's, and one being tried in settings.
const LOADED_KEPT: usize = 2;
/// Larger `name` or `fvar` tables, or font files, are taken for damaged.
const TABLE_MOST: u32 = 1 << 20;
const FILE_MOST: u64 = 64 << 20;
/// Faces read from one collection file, at most.
const COLLECTION_MOST: u32 = 64;

/// Looks for fonts in `folders`, and in the folders inside them, and offers
/// the families found as well as Literata. A file that isn't a font, or
/// can't be read, is passed over. Asking again adds to what was found,
/// and a family's files that have gone since are passed over when it's read.
/// Returns the families found this time.
pub fn find_fonts(folders: &[PathBuf]) -> Vec<String> {
    let mut files = Vec::new();
    for folder in folders {
        gather(folder, 1, &mut files);
    }
    files.sort();
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for file in files {
        let Some((faces, stamp)) = probe(&file) else {
            continue;
        };
        for (family, varies) in faces {
            if family.eq_ignore_ascii_case(BODY_FAMILY) {
                continue;
            }
            let entry = found.entry(family).or_insert_with(|| Found {
                files: Vec::new(),
                varies_in_weight: false,
                fingerprint: Fnv::default().0,
            });
            if !entry.files.contains(&file) {
                entry.files.push(file.clone());
                let mut hash = Fnv(entry.fingerprint);
                hash.write(file.as_os_str().as_encoded_bytes());
                hash.write(&stamp.to_le_bytes());
                entry.fingerprint = hash.0;
            }
            entry.varies_in_weight |= varies;
        }
    }
    let names = found.keys().cloned().collect();
    let mut shelf = SHELF.lock().unwrap_or_else(PoisonError::into_inner);
    for (family, found) in found {
        shelf.loaded.retain(|(name, _)| *name != family);
        match shelf.families.get_mut(&family) {
            // Found before, maybe in another folder: both sets of files.
            Some(known) => {
                for file in found.files {
                    if !known.files.contains(&file) {
                        known.files.push(file);
                    }
                }
                let mut hash = Fnv(known.fingerprint);
                hash.write(&found.fingerprint.to_le_bytes());
                known.fingerprint = hash.0;
                known.varies_in_weight |= found.varies_in_weight;
            }
            None => {
                shelf.families.insert(family, found);
            }
        }
    }
    names
}

/// The families text can be set in: Literata first, then those
/// [`find_fonts`] found, by name.
pub fn font_families() -> Vec<String> {
    let shelf = SHELF.lock().unwrap_or_else(PoisonError::into_inner);
    std::iter::once(BODY_FAMILY.to_owned())
        .chain(shelf.families.keys().cloned())
        .collect()
}

/// Whether text in `family` (Literata for `None`) can be made darker by
/// [`crate::Settings::weight`]. Most fonts come only in regular and bold.
pub fn varies_in_weight(family: Option<&str>) -> bool {
    match family {
        None => true,
        Some(family) if family.eq_ignore_ascii_case(BODY_FAMILY) => true,
        Some(family) => SHELF
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .families
            .get(family)
            .is_none_or(|found| found.varies_in_weight),
    }
}

/// Identifies `family`'s files as found, for page maps' cache keys: 0 for
/// Literata, or a family that isn't there.
pub(crate) fn fingerprint(family: Option<&str>) -> u64 {
    family.map_or(0, |family| {
        SHELF
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .families
            .get(family)
            .map_or(0, |found| found.fingerprint)
    })
}

/// A family's faces, read in full.
struct Loaded {
    faces: Arc<Vec<Blob<u8>>>,
    varies_in_weight: bool,
}

/// `family`'s faces read in full, kept for next time.
fn load(family: &str) -> Option<Loaded> {
    let found = {
        let mut shelf = SHELF.lock().unwrap_or_else(PoisonError::into_inner);
        let found = shelf.families.get(family)?.clone();
        if let Some(at) = shelf.loaded.iter().position(|(name, _)| name == family) {
            let entry = shelf.loaded.remove(at);
            let faces = entry.1.clone();
            shelf.loaded.push(entry);
            return Some(Loaded {
                faces,
                varies_in_weight: found.varies_in_weight,
            });
        }
        found
    };
    // Read without holding the lock: other threads may be setting text.
    let faces: Vec<Blob<u8>> = found
        .files
        .iter()
        .filter_map(|file| {
            let data = read_font(file)?;
            FontRef::from_index(&data, 0).ok()?;
            Some(Blob::new(Arc::new(data)))
        })
        .collect();
    if faces.is_empty() {
        return None;
    }
    let faces = Arc::new(faces);
    let mut shelf = SHELF.lock().unwrap_or_else(PoisonError::into_inner);
    shelf.loaded.retain(|(name, _)| name != family);
    shelf.loaded.push((family.to_owned(), faces.clone()));
    let extra = shelf.loaded.len().saturating_sub(LOADED_KEPT);
    shelf.loaded.drain(..extra);
    Some(Loaded {
        faces,
        varies_in_weight: found.varies_in_weight,
    })
}

fn read_font(file: &Path) -> Option<Vec<u8>> {
    let file = File::open(file).ok()?;
    let mut data = Vec::new();
    file.take(FILE_MOST + 1).read_to_end(&mut data).ok()?;
    (data.len() as u64 <= FILE_MOST).then_some(data)
}

/// The font files in `folder`, and in its folders down to `depth` more.
fn gather(folder: &Path, depth: u32, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
            if depth > 0 {
                gather(&path, depth - 1, files);
            }
        } else if is_font_file(&path) {
            files.push(path);
        }
    }
}

fn is_font_file(path: &Path) -> bool {
    let hidden = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| name.starts_with('.'));
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    !hidden && matches!(extension.as_deref(), Some("ttf" | "otf" | "ttc" | "otc"))
}

const COLLECTION: u32 = u32::from_be_bytes(*b"ttcf");

/// The family and whether it varies in weight, for each face in `file`,
/// and a stamp of its size and time. `None` if it isn't a font.
fn probe(file: &Path) -> Option<(Vec<(String, bool)>, u64)> {
    let mut f = File::open(file).ok()?;
    let meta = f.metadata().ok()?;
    if meta.len() > FILE_MOST {
        return None;
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |t| t.as_secs());
    let offsets = if read_u32(&mut f, 0)? == COLLECTION {
        let count = read_u32(&mut f, 8)?.min(COLLECTION_MOST);
        (0..count)
            .map(|i| read_u32(&mut f, 12 + 4 * u64::from(i)))
            .collect::<Option<Vec<_>>>()?
    } else {
        vec![0]
    };
    let faces: Vec<_> = offsets
        .into_iter()
        .filter_map(|offset| probe_face(&mut f, u64::from(offset)))
        .collect();
    let stamp = meta.len() ^ modified.rotate_left(32);
    (!faces.is_empty()).then_some((faces, stamp))
}

fn probe_face(f: &mut File, offset: u64) -> Option<(String, bool)> {
    const TRUE_TYPE: u32 = 0x0001_0000;
    let version = read_u32(f, offset)?;
    if ![
        TRUE_TYPE,
        u32::from_be_bytes(*b"OTTO"),
        u32::from_be_bytes(*b"true"),
    ]
    .contains(&version)
    {
        return None;
    }
    let tables = usize::from(read_u16(f, offset + 4)?.min(512));
    let mut directory = vec![0; 16 * tables];
    read_at(f, offset + 12, &mut directory)?;
    let table = |tag: &[u8; 4]| {
        directory
            .as_chunks::<16>()
            .0
            .iter()
            .find(|record| &record[..4] == tag)
            .map(|record| (be32(&record[8..12]), be32(&record[12..16])))
    };
    // Something to draw with: a character map and outlines.
    table(b"cmap")?;
    if table(b"glyf").is_none() && table(b"CFF ").is_none() && table(b"CFF2").is_none() {
        return None;
    }
    // Fonts made for Chinese, Japanese or Korean are left out: they're
    // large, and their Latin letters are an afterthought.
    if let Some(os2) = table(b"OS/2").and_then(|at| read_table(f, at))
        && is_for_cjk(&os2)
    {
        return None;
    }
    let name = read_table(f, table(b"name")?)?;
    let family = family_name(&name)?;
    let varies = table(b"fvar")
        .and_then(|at| read_table(f, at))
        .is_some_and(|fvar| has_axis(&fvar, *b"wght"));
    Some((family, varies))
}

/// The typographic family's name, or the family's, in English if it's
/// there, as fontique picks it.
fn family_name(table: &[u8]) -> Option<String> {
    let name = Name::read(FontData::new(table)).ok()?;
    let records = name.name_record();
    [NameId::TYPOGRAPHIC_FAMILY_NAME, NameId::FAMILY_NAME]
        .into_iter()
        .find_map(|id| {
            let ours = records.iter().filter(|r| r.name_id() == id);
            let english = ours.clone().find(|r| {
                (r.platform_id() == 3 && r.language_id() == 0x409)
                    || (r.platform_id() == 1 && r.language_id() == 0)
            });
            english
                .into_iter()
                .chain(ours)
                .find_map(|r| r.string(name.string_data()).ok())
                .map(|s| s.to_string().trim().to_owned())
                .filter(|s| !s.is_empty())
        })
}

/// Whether an `OS/2` table says the font covers kana, hangul or CJK
/// ideographs (Unicode ranges 49, 56 and 59, in `ulUnicodeRange2`).
fn is_for_cjk(os2: &[u8]) -> bool {
    os2.get(46..50).is_some_and(|range| {
        let range = be32(range);
        [49, 56, 59]
            .iter()
            .any(|bit| range & (1 << (bit - 32)) != 0)
    })
}

/// Whether an `fvar` table has the axis `tag`.
fn has_axis(fvar: &[u8], tag: [u8; 4]) -> bool {
    let field = |at: usize| {
        fvar.get(at..at + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let (Some(axes), Some(count), Some(size)) = (field(4), field(8), field(10)) else {
        return false;
    };
    (0..count).any(|i| fvar.get(axes + i * size..axes + i * size + 4) == Some(&tag[..]))
}

fn read_table(f: &mut File, (offset, length): (u32, u32)) -> Option<Vec<u8>> {
    if length > TABLE_MOST {
        return None;
    }
    let mut table = vec![0; length as usize];
    read_at(f, u64::from(offset), &mut table)?;
    Some(table)
}

fn read_at(f: &mut File, offset: u64, into: &mut [u8]) -> Option<()> {
    f.seek(SeekFrom::Start(offset)).ok()?;
    f.read_exact(into).ok()
}

fn read_u32(f: &mut File, offset: u64) -> Option<u32> {
    let mut bytes = [0; 4];
    read_at(f, offset, &mut bytes)?;
    Some(u32::from_be_bytes(bytes))
}

fn read_u16(f: &mut File, offset: u64) -> Option<u16> {
    let mut bytes = [0; 2];
    read_at(f, offset, &mut bytes)?;
    Some(u16::from_be_bytes(bytes))
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Whether a face's data is one of the Intel One Mono faces.
#[cfg(test)]
pub(crate) fn is_mono(data: &[u8]) -> bool {
    MONO.iter().any(|face| std::ptr::eq(*face, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bundled_family_is_visible() {
        let mut fonts = context(None).context;
        let mut names: Vec<String> = fonts.collection.family_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(names, [MONO_FAMILY, BODY_FAMILY]);
        let family = fonts.collection.family_by_name(BODY_FAMILY).unwrap();
        assert_eq!(family.fonts().len(), 2, "upright and italic");
        let family = fonts.collection.family_by_name(MONO_FAMILY).unwrap();
        assert_eq!(
            family.fonts().len(),
            4,
            "regular, italic, bold, bold italic"
        );
    }

    fn bundled(face: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fonts")
            .join(face)
    }

    #[test]
    fn a_font_says_its_family_and_whether_it_varies_in_weight() {
        let (faces, _) = probe(&bundled("Literata.ttf")).unwrap();
        assert_eq!(faces, [("Literata".to_owned(), true)]);
        let (faces, _) = probe(&bundled("IntelOneMono-BoldItalic.ttf")).unwrap();
        assert_eq!(faces, [("Intel One Mono".to_owned(), false)]);
        assert!(probe(&bundled("Literata-OFL.txt")).is_none());
    }

    #[test]
    fn finding_fonts_passes_over_what_isnt_one() {
        let folder = tempfile::tempdir().unwrap();
        let inner = folder.path().join("Mono");
        std::fs::create_dir(&inner).unwrap();
        std::fs::copy(
            bundled("IntelOneMono-Italic.ttf"),
            inner.join("IntelOneMono-Italic.TTF"),
        )
        .unwrap();
        // Literata is offered anyway, as the first family.
        std::fs::copy(bundled("Literata.ttf"), folder.path().join("Literata.ttf")).unwrap();
        std::fs::write(folder.path().join("broken.ttf"), b"not a font at all").unwrap();
        std::fs::write(folder.path().join("._hidden.ttf"), b"\0\x01\0\0").unwrap();
        std::fs::copy(bundled("Literata-OFL.txt"), folder.path().join("OFL.txt")).unwrap();
        let found = find_fonts(&[folder.path().to_owned(), folder.path().join("missing")]);
        assert_eq!(found, ["Intel One Mono"]);
        let families = font_families();
        assert_eq!(families[0], BODY_FAMILY);
        assert!(families.contains(&"Intel One Mono".to_owned()));
        assert!(!varies_in_weight(Some("Intel One Mono")));
        assert!(varies_in_weight(None) && varies_in_weight(Some("Literata")));
        assert_ne!(fingerprint(Some("Intel One Mono")), 0);
    }
}
