//! Small helpers over the XML reader. A DOCTYPE is refused outright: nothing in a
//! container or package document needs one, and it's how entity attacks start.

use std::borrow::Cow;

use quick_xml::{
    Reader, XmlVersion,
    escape::resolve_predefined_entity,
    events::{BytesRef, BytesStart, Event},
};

use crate::EpubError;

/// Turns a document's bytes into text. UTF-16 is rare, but valid XML.
pub(crate) fn decode(bytes: &[u8]) -> Cow<'_, str> {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => Cow::Owned(from_utf16(rest, u16::from_le_bytes)),
        [0xFE, 0xFF, rest @ ..] => Cow::Owned(from_utf16(rest, u16::from_be_bytes)),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest),
        _ => String::from_utf8_lossy(bytes),
    }
}

fn from_utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> String {
    let units = bytes.as_chunks::<2>().0.iter().map(|pair| read(*pair));
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Calls `visit` for each element and piece of text.
pub(crate) enum Item<'a> {
    Open(&'a BytesStart<'a>, String),
    Close,
    Text(&'a str),
}

/// Walks a document. Elements that close themselves arrive as an open and a close.
pub(crate) fn walk(text: &str, mut visit: impl FnMut(Item<'_>)) -> Result<(), EpubError> {
    let mut reader = Reader::from_str(text);
    loop {
        let event = reader.read_event().map_err(|_| EpubError::Corrupt)?;
        match event {
            Event::Eof => return Ok(()),
            Event::DocType(_) => {
                return Err(EpubError::Unsafe("a document type declaration"));
            }
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_owned();
                visit(Item::Open(&start, name));
            }
            Event::Empty(start) => {
                let name = start.local_name().as_ref().to_owned();
                visit(Item::Open(&start, name));
                visit(Item::Close);
            }
            Event::End(_) => visit(Item::Close),
            Event::Text(body) => {
                visit(Item::Text(&body.xml_content(XmlVersion::Implicit1_0)));
            }
            Event::CData(body) => {
                visit(Item::Text(&body.xml_content(XmlVersion::Implicit1_0)));
            }
            Event::GeneralRef(entity) => {
                if let Some(text) = resolve_reference(&entity)? {
                    visit(Item::Text(&text));
                }
            }
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) => {}
        }
    }
}

fn resolve_reference(entity: &BytesRef<'_>) -> Result<Option<String>, EpubError> {
    if let Some(c) = entity.resolve_char_ref().map_err(|_| EpubError::Corrupt)? {
        return Ok(Some(c.to_string()));
    }
    let name = entity.xml_content(XmlVersion::Implicit1_0);
    Ok(resolve_predefined_entity(&name).map(str::to_owned))
}

/// An attribute's value, by its local name, ignoring any namespace prefix.
pub(crate) fn attribute(element: &BytesStart<'_>, name: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == name)
        .and_then(|attribute| {
            attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(Cow::into_owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_encoding_xml_allows() {
        assert_eq!(decode(b"abc"), "abc");
        assert_eq!(decode(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(decode(&[0xFF, 0xFE, b'a', 0, b'b', 0]), "ab");
        assert_eq!(decode(&[0xFE, 0xFF, 0, b'a', 0, b'b']), "ab");
    }

    #[test]
    fn text_comes_back_with_references_resolved() {
        let mut seen = String::new();
        walk("<a>x &amp; &#65;&unknown;</a>", |item| {
            if let Item::Text(text) = item {
                seen.push_str(text);
            }
        })
        .unwrap();
        assert_eq!(seen, "x & A");
    }

    #[test]
    fn a_doctype_is_refused() {
        let result = walk("<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>", |_| {});
        assert!(matches!(result, Err(EpubError::Unsafe(_))));
    }
}
