//! The container and package documents: where the book's metadata lives.

use std::{
    collections::HashMap,
    io::{Read, Seek},
};

use crate::{
    Book, CoverBytes, EpubError,
    archive::Archive,
    text,
    xml::{self, Item},
};

const CONTAINER: &str = "META-INF/container.xml";
const ENCRYPTION: &str = "META-INF/encryption.xml";
/// Files that only DRM-protected books have.
const DRM_MARKERS: [&str; 2] = ["META-INF/rights.xml", "META-INF/sinf.xml"];
/// Encryption that only hides embedded fonts from casual copying. The text is
/// readable, so a book using just these isn't DRM.
const FONT_OBFUSCATION: [&str; 2] = [
    "http://www.idpf.org/2008/embedding",
    "http://ns.adobe.com/pdf/enc#RC",
];

const MAX_TITLE_CHARS: usize = 500;
const MAX_FIELD_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 8000;
const MAX_AUTHORS: usize = 30;
const MAX_LANGUAGE_CHARS: usize = 35;

/// Metadata as found in the file. Everything is optional: a book with no title
/// is still a book, and the uploader's filename can stand in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    /// As `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
    pub published_on: Option<String>,
    /// The first identifier that is a valid ISBN, as an ISBN-13.
    pub isbn: Option<String>,
}

pub(crate) fn read_book<R: Read + Seek>(archive: &mut Archive<'_, R>) -> Result<Book, EpubError> {
    check_drm(archive)?;
    let max_xml = archive.limits().max_xml_bytes;

    let container = archive
        .read(CONTAINER, max_xml)?
        .ok_or(EpubError::NoContent)?;
    let package_path = package_path(&xml::decode(&container))?.ok_or(EpubError::NoContent)?;

    let package = archive
        .read(&package_path, max_xml)?
        .ok_or(EpubError::NoContent)?;
    let package = parse_package(&xml::decode(&package))?;

    let base = package_path
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);

    let readable = package.spine.iter().any(|id| {
        package
            .manifest
            .get(id)
            .and_then(|item| resolve_href(base, &item.href))
            .is_some_and(|path| archive.contains(&path))
    });
    if !readable {
        return Err(EpubError::NoContent);
    }

    let cover = match package
        .cover_href()
        .and_then(|href| resolve_href(base, href))
    {
        Some(path) => archive
            .read(&path, archive.limits().max_cover_bytes)?
            .map(CoverBytes),
        None => None,
    };
    Ok(Book {
        metadata: package.metadata,
        cover,
    })
}

fn check_drm<R: Read + Seek>(archive: &mut Archive<'_, R>) -> Result<(), EpubError> {
    if DRM_MARKERS.iter().any(|name| archive.contains(name)) {
        return Err(EpubError::Drm);
    }
    let max_xml = archive.limits().max_xml_bytes;
    let Some(encryption) = archive.read(ENCRYPTION, max_xml)? else {
        return Ok(());
    };

    let mut drm = false;
    xml::walk(&xml::decode(&encryption), |item| {
        if let Item::Open(element, name) = item
            && name == "EncryptionMethod"
        {
            let algorithm = xml::attribute(element, "Algorithm").unwrap_or_default();
            drm |= !FONT_OBFUSCATION.contains(&algorithm.as_str());
        }
    })?;
    if drm { Err(EpubError::Drm) } else { Ok(()) }
}

fn package_path(container: &str) -> Result<Option<String>, EpubError> {
    let mut first = None;
    let mut oebps = None;
    xml::walk(container, |item| {
        if let Item::Open(element, name) = item
            && name == "rootfile"
            && let Some(path) = xml::attribute(element, "full-path")
        {
            let media_type = xml::attribute(element, "media-type").unwrap_or_default();
            if media_type == "application/oebps-package+xml" && oebps.is_none() {
                oebps = Some(path.clone());
            }
            first.get_or_insert(path);
        }
    })?;
    Ok(oebps.or(first))
}

struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Default)]
struct Package {
    metadata: Metadata,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    cover_meta: Option<String>,
}

impl Package {
    fn is_image(item: &ManifestItem) -> bool {
        item.media_type.starts_with("image/")
    }

    /// EPUB 3 marks the cover in the manifest, EPUB 2 in a `meta`. Failing both,
    /// plenty of books still name it "cover".
    fn cover_href(&self) -> Option<&str> {
        let by_property = self.manifest.values().find(|item| {
            Self::is_image(item)
                && item
                    .properties
                    .split_whitespace()
                    .any(|p| p == "cover-image")
        });
        let by_meta = || {
            let item = self.manifest.get(self.cover_meta.as_deref()?)?;
            Self::is_image(item).then_some(item)
        };
        let by_name = || {
            let mut named: Vec<(&String, &ManifestItem)> = self
                .manifest
                .iter()
                .filter(|(id, item)| {
                    Self::is_image(item)
                        && (id.to_lowercase().contains("cover")
                            || item.href.to_lowercase().contains("cover"))
                })
                .collect();
            named.sort_by_key(|(id, _)| *id);
            named.first().map(|(_, item)| *item)
        };
        by_property
            .or_else(by_meta)
            .or_else(by_name)
            .map(|item| item.href.as_str())
    }
}

/// One thing in `<metadata>`, gathered until it closes.
#[derive(Default)]
struct Field {
    kind: FieldKind,
    id: Option<String>,
    role: Option<String>,
    text: String,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum FieldKind {
    #[default]
    None,
    Title,
    Creator,
    Description,
    Publisher,
    Language,
    Date,
    Identifier,
    /// An EPUB 3 `<meta property="...">`, kept by the property's meaning.
    Collection,
    GroupPosition,
    CreatorRole,
}

#[derive(Default)]
struct Gathered {
    titles: Vec<String>,
    /// `(id, role, name)`
    creators: Vec<(Option<String>, Option<String>, String)>,
    /// Roles given by `refines`, for EPUB 3.
    refined_roles: HashMap<String, String>,
    collection: Option<String>,
    position: Option<String>,
    calibre_series: Option<String>,
    calibre_index: Option<String>,
}

fn parse_package(text: &str) -> Result<Package, EpubError> {
    let mut package = Package::default();
    let mut gathered = Gathered::default();
    let mut depth = 0usize;
    let mut section: Option<(&'static str, usize)> = None;
    let mut field: Option<(Field, usize)> = None;

    xml::walk(text, |item| match item {
        Item::Open(element, name) => {
            depth += 1;
            match (section.map(|(s, _)| s), name.as_str()) {
                (None, "metadata") => section = Some(("metadata", depth)),
                (None, "manifest") => section = Some(("manifest", depth)),
                (None, "spine") => section = Some(("spine", depth)),
                (Some("metadata"), "meta") => {
                    field = open_meta(element, &mut package, &mut gathered).map(|f| (f, depth));
                }
                (Some("metadata"), other) if field.is_none() => {
                    if let Some(kind) = dublin_core_kind(other) {
                        let new = Field {
                            kind,
                            id: xml::attribute(element, "id"),
                            role: xml::attribute(element, "role"),
                            text: String::new(),
                        };
                        field = Some((new, depth));
                    }
                }
                (Some("manifest"), "item") => add_manifest_item(element, &mut package),
                (Some("spine"), "itemref") => {
                    if let Some(id) = xml::attribute(element, "idref") {
                        package.spine.push(id);
                    }
                }
                _ => {}
            }
        }
        Item::Close => {
            if field.as_ref().is_some_and(|(_, at)| *at == depth)
                && let Some((done, _)) = field.take()
            {
                store_field(done, &mut package.metadata, &mut gathered);
            }
            if section.is_some_and(|(_, at)| at == depth) {
                section = None;
            }
            depth = depth.saturating_sub(1);
        }
        Item::Text(body) => {
            if let Some((current, _)) = field.as_mut() {
                current.text.push_str(body);
            }
        }
    })?;

    finish_metadata(&mut package.metadata, gathered);
    Ok(package)
}

fn dublin_core_kind(name: &str) -> Option<FieldKind> {
    Some(match name {
        "title" => FieldKind::Title,
        "creator" => FieldKind::Creator,
        "description" => FieldKind::Description,
        "publisher" => FieldKind::Publisher,
        "language" => FieldKind::Language,
        "date" => FieldKind::Date,
        "identifier" => FieldKind::Identifier,
        _ => return None,
    })
}

/// Handles a `<meta>`: the EPUB 2 kind is complete in its attributes, while the
/// EPUB 3 kind has its value as text and so becomes a field.
fn open_meta(
    element: &quick_xml::events::BytesStart<'_>,
    package: &mut Package,
    gathered: &mut Gathered,
) -> Option<Field> {
    if let Some(name) = xml::attribute(element, "name") {
        let content = xml::attribute(element, "content");
        match name.as_str() {
            "cover" => package.cover_meta = content,
            "calibre:series" => gathered.calibre_series = content,
            "calibre:series_index" => gathered.calibre_index = content,
            _ => {}
        }
        return None;
    }
    let property = xml::attribute(element, "property")?;
    let kind = match property.as_str() {
        "belongs-to-collection" => FieldKind::Collection,
        "group-position" => FieldKind::GroupPosition,
        "role" => FieldKind::CreatorRole,
        _ => return None,
    };
    Some(Field {
        kind,
        id: xml::attribute(element, "refines"),
        role: None,
        text: String::new(),
    })
}

fn store_field(field: Field, metadata: &mut Metadata, gathered: &mut Gathered) {
    let body = field.text;
    match field.kind {
        FieldKind::None => {}
        FieldKind::Title => {
            if let Some(title) = text::line(&body, MAX_TITLE_CHARS) {
                gathered.titles.push(title);
            }
        }
        FieldKind::Creator => {
            if let Some(name) = text::line(&body, MAX_FIELD_CHARS) {
                gathered.creators.push((field.id, field.role, name));
            }
        }
        FieldKind::Description => {
            if metadata.description.is_none() {
                metadata.description = text::plain(&body, MAX_DESCRIPTION_CHARS);
            }
        }
        FieldKind::Publisher => {
            if metadata.publisher.is_none() {
                metadata.publisher = text::line(&body, MAX_FIELD_CHARS);
            }
        }
        FieldKind::Language => {
            if metadata.language.is_none() {
                metadata.language = text::line(&body, MAX_LANGUAGE_CHARS);
            }
        }
        FieldKind::Date => {
            if metadata.published_on.is_none() {
                metadata.published_on = date_prefix(body.trim());
            }
        }
        FieldKind::Identifier => {
            if metadata.isbn.is_none() {
                metadata.isbn = crate::isbn::normalize(&body);
            }
        }
        FieldKind::Collection => {
            if gathered.collection.is_none() {
                gathered.collection = text::line(&body, MAX_FIELD_CHARS);
            }
        }
        FieldKind::GroupPosition => {
            if gathered.position.is_none() {
                gathered.position = Some(body.trim().to_owned());
            }
        }
        FieldKind::CreatorRole => {
            if let Some(id) = field.id {
                let id = id.trim_start_matches('#').to_owned();
                gathered.refined_roles.insert(id, body.trim().to_owned());
            }
        }
    }
}

fn finish_metadata(metadata: &mut Metadata, gathered: Gathered) {
    metadata.title = gathered.titles.into_iter().next();

    // Translators and the like are listed as creators too. No role means author.
    let roles = gathered.refined_roles;
    metadata.authors = gathered
        .creators
        .into_iter()
        .filter(|(id, role, _)| {
            let role = role
                .as_deref()
                .or_else(|| id.as_ref().and_then(|id| roles.get(id)).map(String::as_str));
            role.is_none_or(|role| role == "aut")
        })
        .map(|(_, _, name)| name)
        .take(MAX_AUTHORS)
        .collect();

    // Calibre's is by far the commonest, so it wins if a book has both.
    let (series, index) = match gathered.calibre_series {
        Some(series) => (Some(series), gathered.calibre_index),
        None => (gathered.collection, gathered.position),
    };
    metadata.series = series.and_then(|series| text::line(&series, MAX_FIELD_CHARS));
    metadata.series_index = metadata.series.as_ref().and(index).and_then(|index| {
        index
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.0)
    });
}

fn add_manifest_item(element: &quick_xml::events::BytesStart<'_>, package: &mut Package) {
    if let (Some(id), Some(href)) = (
        xml::attribute(element, "id"),
        xml::attribute(element, "href"),
    ) {
        package.manifest.insert(
            id,
            ManifestItem {
                href,
                media_type: xml::attribute(element, "media-type").unwrap_or_default(),
                properties: xml::attribute(element, "properties").unwrap_or_default(),
            },
        );
    }
}

/// Keeps the leading date from a value like `2001-05-17T00:00:00+00:00`, if it
/// really is one.
fn date_prefix(value: &str) -> Option<String> {
    let date = value.split('T').next()?;
    let digits =
        |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    let parts: Vec<&str> = date.split('-').collect();
    let valid = match parts.as_slice() {
        [year] => digits(year, 4),
        [year, month] => digits(year, 4) && digits(month, 2),
        [year, month, day] => digits(year, 4) && digits(month, 2) && digits(day, 2),
        _ => false,
    };
    valid.then(|| date.to_owned())
}

/// Turns a manifest href into a name inside the archive. An href that climbs out
/// of the archive, or points outside it, has no answer.
fn resolve_href(base: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next()?;
    if href.contains("://") || href.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(href)?;
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(body: &str) -> Metadata {
        let text = format!(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata
               xmlns:dc="http://purl.org/dc/elements/1.1/"
               xmlns:opf="http://www.idpf.org/2007/opf">{body}</metadata></package>"#
        );
        parse_package(&text).unwrap().metadata
    }

    #[test]
    fn reads_the_common_fields() {
        let found = metadata(
            r#"<dc:title> A
                 Title &amp; More </dc:title>
               <dc:title>Second title</dc:title>
               <dc:creator>Ann Author</dc:creator>
               <dc:creator opf:role="aut">Bob Author</dc:creator>
               <dc:creator opf:role="trl">Tom Translator</dc:creator>
               <dc:publisher>Press</dc:publisher>
               <dc:language>en</dc:language>
               <dc:date>2001-05-17T00:00:00+00:00</dc:date>
               <dc:description>&lt;p&gt;Some &lt;b&gt;bold&lt;/b&gt; text&lt;/p&gt;</dc:description>"#,
        );
        assert_eq!(found.title.as_deref(), Some("A Title & More"));
        assert_eq!(found.authors, ["Ann Author", "Bob Author"]);
        assert_eq!(found.publisher.as_deref(), Some("Press"));
        assert_eq!(found.language.as_deref(), Some("en"));
        assert_eq!(found.published_on.as_deref(), Some("2001-05-17"));
        assert_eq!(found.description.as_deref(), Some("Some bold text"));
    }

    #[test]
    fn the_first_identifier_that_is_an_isbn() {
        let found = metadata(
            r#"<dc:identifier id="uuid_id" opf:scheme="uuid">4be28826-6838-4ff7-ba73-cc553a0dba81</dc:identifier>
               <dc:identifier>http://example.com/books/1</dc:identifier>
               <dc:identifier opf:scheme="ISBN">978-1-101-59243-4</dc:identifier>
               <dc:identifier>urn:isbn:9780345339683</dc:identifier>"#,
        );
        assert_eq!(found.isbn.as_deref(), Some("9781101592434"));
        assert_eq!(
            metadata("<dc:identifier>not one</dc:identifier>").isbn,
            None
        );
    }

    #[test]
    fn epub3_roles_and_series() {
        let found = metadata(
            r##"<dc:creator id="a">Ann</dc:creator>
                <dc:creator id="b">Tom</dc:creator>
                <meta refines="#a" property="role" scheme="marc:relators">aut</meta>
                <meta refines="#b" property="role" scheme="marc:relators">trl</meta>
                <meta property="belongs-to-collection" id="c">The Series</meta>
                <meta refines="#c" property="group-position">2.5</meta>"##,
        );
        assert_eq!(found.authors, ["Ann"]);
        assert_eq!(found.series.as_deref(), Some("The Series"));
        assert_eq!(found.series_index, Some(2.5));
    }

    #[test]
    fn calibre_series_wins() {
        let found = metadata(
            r##"<meta name="calibre:series" content="Calibre Series"/>
                <meta name="calibre:series_index" content="3"/>
                <meta property="belongs-to-collection" id="c">Other</meta>
                <meta refines="#c" property="group-position">9</meta>"##,
        );
        assert_eq!(found.series.as_deref(), Some("Calibre Series"));
        assert_eq!(found.series_index, Some(3.0));
    }

    #[test]
    fn nonsense_values_are_dropped_rather_than_kept() {
        let found = metadata(
            r#"<dc:date>sometime in spring</dc:date>
               <dc:title>   </dc:title>
               <meta name="calibre:series" content="S"/>
               <meta name="calibre:series_index" content="NaN"/>"#,
        );
        assert_eq!(found.published_on, None);
        assert_eq!(found.title, None);
        assert_eq!(found.series_index, None);
    }

    #[test]
    fn long_values_are_capped() {
        let long = "x".repeat(MAX_TITLE_CHARS + 50);
        let found = metadata(&format!("<dc:title>{long}</dc:title>"));
        assert_eq!(found.title.unwrap().chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn dates_are_checked() {
        for good in ["2001", "2001-05", "2001-05-17", "2001-05-17T10:00:00Z"] {
            assert!(date_prefix(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "May 2001",
            "01-05-17",
            "2001-5",
            "2001-05-17-01",
            "20x1",
        ] {
            assert_eq!(date_prefix(bad), None, "{bad}");
        }
    }

    #[test]
    fn hrefs_resolve_inside_the_archive_only() {
        assert_eq!(
            resolve_href("OEBPS", "a.jpg").as_deref(),
            Some("OEBPS/a.jpg")
        );
        assert_eq!(resolve_href("", "a.jpg").as_deref(), Some("a.jpg"));
        assert_eq!(
            resolve_href("OEBPS/x", "../a.jpg").as_deref(),
            Some("OEBPS/a.jpg")
        );
        assert_eq!(
            resolve_href("OEBPS", "a%20b.jpg#f").as_deref(),
            Some("OEBPS/a b.jpg")
        );
        assert_eq!(resolve_href("OEBPS", "../../etc/passwd"), None);
        assert_eq!(resolve_href("", "../a.jpg"), None);
        assert_eq!(resolve_href("OEBPS", "/etc/passwd"), None);
        assert_eq!(resolve_href("OEBPS", "https://evil.example/a.jpg"), None);
        assert_eq!(resolve_href("OEBPS", "a%2"), None);
    }

    fn cover_of(manifest: &str, meta: &str) -> Option<String> {
        let text = format!(
            "<package><metadata>{meta}</metadata><manifest>{manifest}</manifest></package>"
        );
        parse_package(&text)
            .unwrap()
            .cover_href()
            .map(str::to_owned)
    }

    #[test]
    fn finds_covers_each_way_in_order() {
        let property =
            r#"<item id="x" href="p.jpg" media-type="image/jpeg" properties="cover-image"/>"#;
        let meta_item = r#"<item id="m" href="m.jpg" media-type="image/jpeg"/>"#;
        let named = r#"<item id="cover-img" href="n.jpg" media-type="image/jpeg"/>"#;
        let meta = r#"<meta name="cover" content="m"/>"#;

        assert_eq!(
            cover_of(&format!("{property}{meta_item}{named}"), meta).as_deref(),
            Some("p.jpg")
        );
        assert_eq!(
            cover_of(&format!("{meta_item}{named}"), meta).as_deref(),
            Some("m.jpg")
        );
        assert_eq!(cover_of(named, "").as_deref(), Some("n.jpg"));
        assert_eq!(cover_of(meta_item, "").as_deref(), None);
    }

    #[test]
    fn a_cover_page_is_not_a_cover_image() {
        let page = r#"<item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>"#;
        assert_eq!(
            cover_of(page, r#"<meta name="cover" content="cover"/>"#),
            None
        );
    }
}
