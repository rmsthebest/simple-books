//! A forgiving reader for a book's XHTML. EPUB chapters are meant to be XML,
//! but many aren't quite: they have a DOCTYPE, HTML entities like `&nbsp;`,
//! end tags that don't match, or `<br>` without a slash. This keeps whatever
//! it can read and stops quietly at anything worse.
//!
//! A DOCTYPE is skipped, never interpreted, so no entity it declares is ever
//! expanded. Only the predefined and HTML entities are known.

use std::borrow::Cow;

use quick_xml::{
    Reader, XmlVersion,
    escape::{resolve_html5_entity, resolve_predefined_entity},
    events::{BytesRef, BytesStart, Event},
};

/// Elements that never have content, even when written without a slash.
const VOID: [&str; 12] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "wbr",
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct HtmlLimits {
    pub(crate) max_nodes: usize,
    /// Elements deeper than this are dropped, though their text is kept.
    pub(crate) max_depth: usize,
}

#[derive(Debug)]
pub(crate) struct Document {
    nodes: Vec<Node>,
}

#[derive(Debug)]
pub(crate) enum Node {
    Element(Element),
    Text(String),
}

#[derive(Debug)]
pub(crate) struct Element {
    /// The local name, lowercased.
    pub(crate) name: String,
    /// Names are lowercased and keep any prefix, as in `epub:type`, so it
    /// doesn't collide with a list's `type`.
    pub(crate) attributes: Vec<(String, String)>,
    pub(crate) children: Vec<usize>,
}

impl Element {
    pub(crate) fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// An attribute by its name without a prefix, such as `href` for
    /// `xlink:href`.
    pub(crate) fn attribute_any_prefix(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key.rsplit(':').next() == Some(name))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn classes(&self) -> impl Iterator<Item = &str> {
        self.attribute("class")
            .unwrap_or_default()
            .split_ascii_whitespace()
    }

    /// Whether a space-separated attribute such as `epub:type` has `token`.
    pub(crate) fn has_token(&self, attribute: &str, token: &str) -> bool {
        self.attribute(attribute)
            .is_some_and(|value| value.split_ascii_whitespace().any(|t| t == token))
    }
}

impl Document {
    pub(crate) const ROOT: usize = 0;

    pub(crate) fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }

    pub(crate) fn element(&self, index: usize) -> Option<&Element> {
        match &self.nodes[index] {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        }
    }

    /// Every element, in document order, with its index.
    pub(crate) fn elements(&self) -> impl Iterator<Item = (usize, &Element)> {
        let mut stack = vec![Self::ROOT];
        std::iter::from_fn(move || {
            while let Some(index) = stack.pop() {
                if let Node::Element(element) = &self.nodes[index] {
                    stack.extend(element.children.iter().rev());
                    return Some((index, element));
                }
            }
            None
        })
    }

    /// The text inside a node, as it appears.
    pub(crate) fn text_of(&self, index: usize) -> String {
        let mut out = String::new();
        let mut stack = vec![index];
        while let Some(index) = stack.pop() {
            match &self.nodes[index] {
                Node::Text(text) => out.push_str(text),
                Node::Element(element) => stack.extend(element.children.iter().rev()),
            }
        }
        out
    }

    pub(crate) fn parse(text: &str, limits: HtmlLimits) -> Self {
        let mut document = Self {
            nodes: vec![Node::Element(Element {
                name: String::new(),
                attributes: Vec::new(),
                children: Vec::new(),
            })],
        };
        // The open elements, by index and name. The root is always first.
        let mut open: Vec<(usize, String)> = vec![(Self::ROOT, String::new())];

        let mut reader = Reader::from_str(text);
        let config = reader.config_mut();
        config.check_end_names = false;
        config.allow_unmatched_ends = true;
        config.allow_dangling_amp = true;

        loop {
            if document.nodes.len() >= limits.max_nodes {
                break;
            }
            let Ok(event) = reader.read_event() else {
                break;
            };
            match event {
                Event::Eof => break,
                Event::Start(start) => {
                    let element = read_element(&start);
                    let void = VOID.contains(&element.name.as_str());
                    if open.len() > limits.max_depth {
                        continue;
                    }
                    let name = element.name.clone();
                    let index = document.push(&open, Node::Element(element));
                    if !void {
                        open.push((index, name));
                    }
                }
                Event::Empty(start) => {
                    if open.len() <= limits.max_depth {
                        document.push(&open, Node::Element(read_element(&start)));
                    }
                }
                Event::End(end) => {
                    let name = end.local_name().as_ref().to_lowercase();
                    // Close up to the matching element; ignore an end tag that
                    // matches nothing open.
                    if let Some(at) = open.iter().rposition(|(_, open)| *open == name)
                        && at > 0
                    {
                        open.truncate(at);
                    }
                }
                Event::Text(body) => {
                    document.push_text(&open, &body.xml_content(XmlVersion::Implicit1_0));
                }
                Event::CData(body) => {
                    document.push_text(&open, &body.xml_content(XmlVersion::Implicit1_0));
                }
                Event::GeneralRef(entity) => {
                    let text = resolve_reference(&entity);
                    document.push_text(&open, &text);
                }
                Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
            }
        }
        document
    }

    fn push(&mut self, open: &[(usize, String)], node: Node) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        let parent = open.last().map_or(Self::ROOT, |(parent, _)| *parent);
        if let Node::Element(parent) = &mut self.nodes[parent] {
            parent.children.push(index);
        }
        index
    }

    /// Adds text, joining it to text just before it.
    fn push_text(&mut self, open: &[(usize, String)], text: &str) {
        let outside = open.len() == 1;
        if text.is_empty() || (outside && text.trim().is_empty()) {
            return;
        }
        let parent = open.last().map_or(Self::ROOT, |(parent, _)| *parent);
        let last = match &self.nodes[parent] {
            Node::Element(element) => element.children.last().copied(),
            Node::Text(_) => None,
        };
        if let Some(last) = last
            && let Node::Text(existing) = &mut self.nodes[last]
        {
            existing.push_str(text);
            return;
        }
        self.push(open, Node::Text(text.to_owned()));
    }
}

fn read_element(start: &BytesStart<'_>) -> Element {
    let name = start.local_name().as_ref().to_lowercase();
    let mut attributes = Vec::new();
    for attribute in start.attributes().with_checks(false).flatten() {
        let key = attribute.key.as_ref().to_lowercase();
        let value = attribute
            .normalized_value_with(XmlVersion::Implicit1_0, 4, resolve_entity)
            .map_or_else(|_| attribute.value.clone().into_owned(), Cow::into_owned);
        attributes.push((key, value));
    }
    Element {
        name,
        attributes,
        children: Vec::new(),
    }
}

fn resolve_entity(name: &str) -> Option<&'static str> {
    resolve_predefined_entity(name).or_else(|| resolve_html5_entity(name))
}

/// An unknown entity is kept as written, as a browser would.
fn resolve_reference(entity: &BytesRef<'_>) -> String {
    if let Ok(Some(c)) = entity.resolve_char_ref() {
        return c.to_string();
    }
    let name = entity.xml_content(XmlVersion::Implicit1_0);
    match resolve_entity(&name) {
        Some(text) => text.to_owned(),
        None => format!("&{name};"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: HtmlLimits = HtmlLimits {
        max_nodes: 10_000,
        max_depth: 64,
    };

    /// The tree as a compact string: elements as `name(children)`, text quoted.
    fn shape(document: &Document, index: usize) -> String {
        match document.node(index) {
            Node::Text(text) => format!("{text:?}"),
            Node::Element(element) => {
                let children: Vec<String> = element
                    .children
                    .iter()
                    .map(|child| shape(document, *child))
                    .collect();
                format!("{}({})", element.name, children.join(","))
            }
        }
    }

    fn parsed(text: &str) -> String {
        let document = Document::parse(text, LIMITS);
        shape(&document, Document::ROOT)
    }

    #[test]
    fn reads_real_world_chapters() {
        let text = r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml"><body><p>A&nbsp;b &amp; &mdash;&#233;</p></body></html>"#;
        assert_eq!(parsed(text), r#"(html(body(p("A\u{a0}b & —é"))))"#);
    }

    #[test]
    fn a_doctype_declares_nothing() {
        let text = "<!DOCTYPE a [<!ENTITY x 'expanded'>]><p>&x;</p>";
        assert_eq!(parsed(text), r#"(p("&x;"))"#);
    }

    #[test]
    fn copes_with_html_habits() {
        assert_eq!(parsed("<p>a<br>b</p>"), r#"(p("a",br(),"b"))"#);
        assert_eq!(
            parsed("<p><i>a</p><p>b</p>"),
            r#"(p(i("a")),p("b"))"#,
            "an unclosed element ends with its parent"
        );
        assert_eq!(parsed("<p>a</q>b</p>"), r#"(p("ab"))"#);
        assert_eq!(parsed("<P CLASS=\"x\">a</P>"), r#"(p("a"))"#);
    }

    #[test]
    fn keeps_what_came_before_a_fatal_error() {
        assert_eq!(parsed("<p>kept</p><!-- never closed"), r#"(p("kept"))"#);
    }

    #[test]
    fn attributes_keep_prefixes_and_resolve_entities() {
        let document = Document::parse(
            r#"<a epub:type="noteref" xlink:href="x.xhtml#n&amp;1" title="&eacute;"/>"#,
            LIMITS,
        );
        let element = document.element(1).unwrap();
        assert!(element.has_token("epub:type", "noteref"));
        assert_eq!(element.attribute("type"), None);
        assert_eq!(element.attribute_any_prefix("href"), Some("x.xhtml#n&1"));
        assert_eq!(element.attribute("title"), Some("é"));
    }

    #[test]
    fn limits_hold() {
        let deep = "<div>".repeat(1000) + "text";
        let document = Document::parse(&deep, LIMITS);
        assert!(document.nodes.len() <= LIMITS.max_depth + 2);
        assert_eq!(document.text_of(Document::ROOT), "text");

        let wide = "<br/>".repeat(100_000);
        let document = Document::parse(&wide, LIMITS);
        assert_eq!(document.nodes.len(), LIMITS.max_nodes);
    }
}
