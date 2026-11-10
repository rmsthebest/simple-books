//! The table of contents, from an EPUB 3 navigation document or an EPUB 2
//! NCX. Entries come back as links to resolve, since chapters are numbered
//! only once they've all been read.

use crate::html::{Document, Element};

const MAX_TITLE_CHARS: usize = 300;
const MAX_DEPTH: u8 = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawEntry {
    pub(crate) title: String,
    pub(crate) depth: u8,
    pub(crate) href: String,
}

/// The `toc` navigation in an EPUB 3 navigation document, or failing that its
/// first `nav`.
pub(crate) fn from_nav(document: &Document, max_entries: usize) -> Vec<RawEntry> {
    let navs: Vec<(usize, &Element)> = document
        .elements()
        .filter(|(_, element)| element.name == "nav")
        .collect();
    let chosen = navs
        .iter()
        .find(|(_, nav)| nav.has_token("epub:type", "toc") || nav.has_token("role", "doc-toc"))
        .or_else(|| navs.first());
    let mut entries = Vec::new();
    if let Some((nav, _)) = chosen {
        for list in descendants_named(document, *nav, "ol", "ol") {
            nav_list(document, list, 0, max_entries, &mut entries);
        }
    }
    entries
}

fn nav_list(
    document: &Document,
    list: usize,
    depth: u8,
    max_entries: usize,
    entries: &mut Vec<RawEntry>,
) {
    let Some(list) = document.element(list) else {
        return;
    };
    for item in &list.children {
        let Some(element) = document.element(*item) else {
            continue;
        };
        if element.name != "li" {
            continue;
        }
        if entries.len() >= max_entries {
            return;
        }
        let link = descendants_named(document, *item, "a", "ol")
            .into_iter()
            .next()
            .and_then(|a| Some((a, document.element(a)?.attribute("href")?)));
        let mut child_depth = depth;
        if let Some((a, href)) = link {
            let title = title(&document.text_of(a));
            if !title.is_empty() {
                entries.push(RawEntry {
                    title,
                    depth,
                    href: href.to_owned(),
                });
                child_depth = (depth + 1).min(MAX_DEPTH);
            }
        }
        for nested in descendants_named(document, *item, "ol", "ol") {
            nav_list(document, nested, child_depth, max_entries, entries);
        }
    }
}

/// The nearest descendants with a name: none inside another of the same name,
/// nor inside a `stop` element.
fn descendants_named(document: &Document, from: usize, name: &str, stop: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut stack: Vec<usize> = document
        .element(from)
        .map(|element| element.children.iter().rev().copied().collect())
        .unwrap_or_default();
    while let Some(index) = stack.pop() {
        if let Some(element) = document.element(index) {
            if element.name == name {
                found.push(index);
            } else if element.name != stop {
                stack.extend(element.children.iter().rev());
            }
        }
    }
    found
}

pub(crate) fn from_ncx(document: &Document, max_entries: usize) -> Vec<RawEntry> {
    let mut entries = Vec::new();
    for map in descendants_named(document, Document::ROOT, "navmap", "navmap") {
        for point in descendants_named(document, map, "navpoint", "navpoint") {
            nav_point(document, point, 0, max_entries, &mut entries);
        }
    }
    entries
}

fn nav_point(
    document: &Document,
    point: usize,
    depth: u8,
    max_entries: usize,
    entries: &mut Vec<RawEntry>,
) {
    if entries.len() >= max_entries {
        return;
    }
    let label = descendants_named(document, point, "navlabel", "navpoint")
        .first()
        .map(|label| title(&document.text_of(*label)))
        .unwrap_or_default();
    let href = descendants_named(document, point, "content", "navpoint")
        .first()
        .and_then(|content| document.element(*content)?.attribute("src"))
        .map(str::to_owned);
    let mut child_depth = depth;
    if let Some(href) = href
        && !label.is_empty()
    {
        entries.push(RawEntry {
            title: label,
            depth,
            href,
        });
        child_depth = (depth + 1).min(MAX_DEPTH);
    }
    for nested in descendants_named(document, point, "navpoint", "navpoint") {
        nav_point(document, nested, child_depth, max_entries, entries);
    }
}

/// Collapses whitespace and caps the length.
pub(crate) fn title(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::HtmlLimits;

    const LIMITS: HtmlLimits = HtmlLimits {
        max_nodes: 10_000,
        max_depth: 64,
    };

    fn entry(title: &str, depth: u8, href: &str) -> RawEntry {
        RawEntry {
            title: title.to_owned(),
            depth,
            href: href.to_owned(),
        }
    }

    #[test]
    fn reads_a_nav_document() {
        let text = r#"<html><body>
            <nav epub:type="landmarks"><ol><li><a href="x.xhtml">Not this</a></li></ol></nav>
            <nav epub:type="toc"><h1>Contents</h1><ol>
              <li><a href="one.xhtml">  One
                 </a></li>
              <li><span>Part</span><ol>
                <li><a href="two.xhtml#s"><span>Two</span></a>
                  <ol><li><a href="two.xhtml#t">Deeper</a></li></ol></li>
              </ol></li>
            </ol></nav></body></html>"#;
        let found = from_nav(&Document::parse(text, LIMITS), 100);
        assert_eq!(
            found,
            [
                entry("One", 0, "one.xhtml"),
                entry("Two", 0, "two.xhtml#s"),
                entry("Deeper", 1, "two.xhtml#t"),
            ]
        );
        assert_eq!(from_nav(&Document::parse(text, LIMITS), 1).len(), 1);
    }

    #[test]
    fn reads_an_ncx() {
        let text = r#"<?xml version="1.0"?>
            <!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd">
            <ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
              <navPoint id="a"><navLabel><text>One</text></navLabel><content src="one.html"/>
                <navPoint id="b"><navLabel><text>One &amp; a half</text></navLabel><content src="one.html#h"/></navPoint>
              </navPoint>
              <navPoint id="c"><navLabel><text>Two</text></navLabel><content src="two.html"/></navPoint>
            </navMap></ncx>"#;
        assert_eq!(
            from_ncx(&Document::parse(text, LIMITS), 100),
            [
                entry("One", 0, "one.html"),
                entry("One & a half", 1, "one.html#h"),
                entry("Two", 0, "two.html"),
            ]
        );
    }
}
