//! Turns one chapter's HTML into a flat list of blocks.
//!
//! Text goes into the block being built until a block-level element starts or
//! ends. So text written straight into a `<div>`, or mixed with `<p>`s, still
//! becomes paragraphs, as a browser would show it.

use std::collections::HashMap;

use book_format::{Align, BlockKind, MAX_INDENT, SOFT_HYPHEN, Style, Vertical};

use crate::{
    css::{Display, Props, Styles},
    html::{Document, Element, Node},
};

/// Where a link goes, before chapters are numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// An entry in the EPUB.
    pub(crate) document: String,
    pub(crate) fragment: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawSpan {
    pub(crate) text: String,
    pub(crate) style: Style,
    /// An index into the book's list of [`Target`]s.
    pub(crate) link: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawBlock {
    pub(crate) kind: BlockKind,
    pub(crate) indent: u8,
    pub(crate) align: Align,
    pub(crate) spans: Vec<RawSpan>,
}

#[derive(Debug, Default)]
pub(crate) struct BuiltChapter {
    pub(crate) blocks: Vec<RawBlock>,
    /// Element ids, as (block, character offset).
    pub(crate) anchors: HashMap<String, (u32, u32)>,
}

/// What became of an image the chapter refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageUse {
    /// Its index in the book.
    Block(u32),
    /// Too small to show on its own: decoration, or a drop cap.
    Small,
    Missing,
}

/// What the builder needs from the rest of the book.
pub(crate) trait Book {
    /// Where an `href` or `src` in this chapter points, as an entry name and a
    /// fragment. `None` if it leaves the book.
    fn resolve(&self, href: &str) -> Option<Target>;
    fn image(&mut self, entry: &str) -> ImageUse;
    fn link(&mut self, target: Target) -> usize;
    /// Charges one unit of style matching. `false` once the book has used up
    /// its share, after which no more styles are looked up.
    fn charge(&mut self, units: u64) -> bool;
}

/// What text is inside, passed down from element to element.
#[derive(Debug, Clone, Copy)]
struct Context {
    style: Style,
    align: Align,
    indent: u8,
    first_indent: Option<bool>,
    kind: Kind,
    link: Option<usize>,
    pre: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Heading(u8),
    Preformatted,
}

/// An item before blank paragraphs are settled.
#[derive(Debug)]
enum Item {
    Block(RawBlock, Option<bool>),
    /// An empty paragraph: a scene break between paragraphs, padding elsewhere.
    Blank,
}

struct Pending {
    kind: BlockKind,
    indent: u8,
    align: Align,
    first_indent: Option<bool>,
    spans: Vec<RawSpan>,
    chars: u32,
    /// Collapsed whitespace waiting to become a space, with the style and
    /// link of the text it was in.
    space: Option<(Style, Option<usize>)>,
    pre: bool,
}

#[derive(Debug, Clone, Copy)]
struct List {
    ordered: bool,
    next: i64,
    numbering: u8,
}

struct Builder<'b, B> {
    book: &'b mut B,
    styles: &'b Styles,
    items: Vec<Item>,
    pending: Option<Pending>,
    /// A list item's marker, waiting for the item's first block.
    marker: Option<String>,
    lists: Vec<List>,
    anchors: HashMap<String, (u32, u32)>,
    styled: bool,
}

pub(crate) fn chapter<B: Book>(document: &Document, styles: &Styles, book: &mut B) -> BuiltChapter {
    let mut builder = Builder {
        book,
        styles,
        items: Vec::new(),
        pending: None,
        marker: None,
        lists: Vec::new(),
        anchors: HashMap::new(),
        styled: true,
    };
    let context = Context {
        style: Style::default(),
        align: Align::Default,
        indent: 0,
        first_indent: None,
        kind: Kind::Paragraph,
        link: None,
        pre: false,
    };
    let mut ancestors = Vec::new();
    builder.children(document, Document::ROOT, &context, &mut ancestors);
    builder.flush();
    builder.finish()
}

/// What an element does to the text inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Skip,
    Image,
    Svg,
    Break,
    Rule,
    Heading(u8),
    Paragraph,
    Container,
    Quote,
    Description,
    List { ordered: bool },
    ListItem,
    Pre,
    Row,
    Cell,
    Inline,
}

fn role(element: &Element) -> Role {
    match element.name.as_str() {
        "head" | "script" | "style" | "title" | "template" | "rt" | "rp" | "iframe" | "input"
        | "select" | "textarea" | "button" | "canvas" | "map" => Role::Skip,
        "img" | "image" => Role::Image,
        "svg" => Role::Svg,
        "br" => Role::Break,
        "hr" => Role::Rule,
        "h1" => Role::Heading(1),
        "h2" => Role::Heading(2),
        "h3" => Role::Heading(3),
        "h4" => Role::Heading(4),
        "h5" => Role::Heading(5),
        "h6" => Role::Heading(6),
        "p" => Role::Paragraph,
        "blockquote" => Role::Quote,
        "dd" => Role::Description,
        "ul" | "menu" | "dir" => Role::List { ordered: false },
        "ol" => Role::List { ordered: true },
        "li" => Role::ListItem,
        "pre" | "listing" | "xmp" | "plaintext" => Role::Pre,
        "tr" => Role::Row,
        "td" | "th" => Role::Cell,
        "html" | "body" | "div" | "section" | "article" | "header" | "footer" | "main"
        | "aside" | "nav" | "figure" | "figcaption" | "address" | "center" | "details"
        | "summary" | "hgroup" | "fieldset" | "legend" | "form" | "caption" | "dl" | "dt"
        | "table" | "thead" | "tbody" | "tfoot" | "noscript" | "" => Role::Container,
        _ => Role::Inline,
    }
}

fn is_hidden(element: &Element) -> bool {
    element.attribute("hidden").is_some()
        || element.has_token("epub:type", "pagebreak")
        || element.has_token("role", "doc-pagebreak")
}

/// The styles an element has before any stylesheet.
fn default_props(element: &Element) -> Props {
    let mut props = Props::default();
    match element.name.as_str() {
        "i" | "em" | "cite" | "var" | "dfn" => props.italic = Some(true),
        "b" | "strong" => props.bold = Some(true),
        "code" | "tt" | "kbd" | "samp" | "pre" => props.monospace = Some(true),
        "sup" => props.vertical = Some(Vertical::Super),
        "sub" => props.vertical = Some(Vertical::Sub),
        "center" => props.align = Some(Align::Center),
        _ => {}
    }
    if let Some(align) = element.attribute("align") {
        props.align = match align.to_ascii_lowercase().as_str() {
            "center" => Some(Align::Center),
            "right" => Some(Align::Right),
            _ => props.align,
        };
    }
    props
}

impl<B: Book> Builder<'_, B> {
    fn children<'d>(
        &mut self,
        document: &'d Document,
        index: usize,
        context: &Context,
        ancestors: &mut Vec<&'d Element>,
    ) {
        let Node::Element(element) = document.node(index) else {
            return;
        };
        for child in &element.children {
            match document.node(*child) {
                Node::Text(text) => self.text(text, context),
                Node::Element(child_element) => {
                    self.element(document, *child, child_element, context, ancestors);
                }
            }
        }
    }

    fn element<'d>(
        &mut self,
        document: &'d Document,
        index: usize,
        element: &'d Element,
        parent: &Context,
        ancestors: &mut Vec<&'d Element>,
    ) {
        let mut role = role(element);
        if role == Role::Skip || is_hidden(element) {
            return;
        }
        let props = self.props(element, ancestors);
        match props.display {
            Some(Display::None) => return,
            Some(Display::Block) if role == Role::Inline => role = Role::Container,
            Some(Display::Inline)
                if matches!(role, Role::Paragraph | Role::Container | Role::Quote) =>
            {
                role = Role::Inline;
            }
            _ => {}
        }

        let is_block = !matches!(
            role,
            Role::Inline | Role::Cell | Role::Break | Role::Image | Role::Svg
        );
        if is_block {
            // So an id on a block points at it, not at the text before it.
            self.flush();
        }
        let name = (element.name == "a")
            .then(|| element.attribute("name"))
            .flatten();
        for id in [element.attribute("id"), name].into_iter().flatten() {
            self.anchor(id);
        }

        let mut context = *parent;
        context.style.emphasis = props.italic.unwrap_or(context.style.emphasis);
        context.style.strong = props.bold.unwrap_or(context.style.strong);
        context.style.small_caps = props.small_caps.unwrap_or(context.style.small_caps);
        context.style.code = props.monospace.unwrap_or(context.style.code);
        if let Some(vertical) = props.vertical {
            context.style.vertical = vertical;
        }
        if let Some(align) = props.align {
            context.align = align;
        }
        if props.indent.is_some() {
            context.first_indent = props.indent;
        }

        match role {
            Role::Skip => {}
            Role::Image => self.image(element, &context),
            Role::Svg => {
                // Only an image inside is kept: the reader doesn't draw vectors.
                for (_, inner) in svg_images(document, index) {
                    self.image(inner, &context);
                }
            }
            Role::Break => self.line_break(),
            Role::Rule => {
                self.flush();
                self.push(RawBlock {
                    kind: BlockKind::Separator,
                    indent: 0,
                    align: Align::Default,
                    spans: Vec::new(),
                });
            }
            Role::Inline | Role::Cell => {
                if role == Role::Cell
                    && self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.chars > 0)
                {
                    self.text_raw(" · ", Style::default(), None);
                }
                if element.name == "a"
                    && let Some(href) = element.attribute_any_prefix("href")
                    && let Some(target) = self.book.resolve(href)
                {
                    context.link = Some(self.book.link(target));
                }
                if element.name == "wbr" {
                    self.text_raw("\u{200B}", context.style, context.link);
                }
                ancestors.push(element);
                self.children(document, index, &context, ancestors);
                ancestors.pop();
            }
            _ => {
                self.block(document, index, element, role, context, ancestors);
            }
        }
    }

    /// The element's own styles, then the stylesheets' over them.
    fn props(&mut self, element: &Element, ancestors: &[&Element]) -> Props {
        let own = default_props(element);
        if self.styled {
            self.styled = self.book.charge(1 + ancestors.len() as u64);
        }
        if !self.styled {
            return own;
        }
        let found = self.styles.props(element, ancestors);
        Props {
            italic: found.italic.or(own.italic),
            bold: found.bold.or(own.bold),
            small_caps: found.small_caps.or(own.small_caps),
            monospace: found.monospace.or(own.monospace),
            vertical: found.vertical.or(own.vertical),
            align: found.align.or(own.align),
            indent: found.indent,
            display: found.display,
        }
    }

    fn block<'d>(
        &mut self,
        document: &'d Document,
        index: usize,
        element: &'d Element,
        role: Role,
        mut context: Context,
        ancestors: &mut Vec<&'d Element>,
    ) {
        self.flush();
        let items_before = self.items.len();
        let mut list_pushed = false;
        let mut owns_marker = false;
        match role {
            Role::Heading(level) => {
                context.kind = Kind::Heading(level);
                // The reader sets headings in its own weight.
                context.style.strong = false;
            }
            Role::Paragraph | Role::Container => context.kind = Kind::Paragraph,
            Role::Row => {
                context.kind = Kind::Paragraph;
                context.first_indent = Some(false);
            }
            Role::Quote | Role::Description => {
                context.kind = Kind::Paragraph;
                context.indent = (context.indent + 1).min(MAX_INDENT);
            }
            Role::List { ordered } => {
                context.indent = (context.indent + 1).min(MAX_INDENT);
                let numbering = element
                    .attribute("type")
                    .and_then(|t| t.bytes().next())
                    .unwrap_or(b'1');
                let start = element
                    .attribute("start")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(1);
                self.lists.push(List {
                    ordered,
                    next: start,
                    numbering,
                });
                list_pushed = true;
            }
            Role::ListItem => {
                context.kind = Kind::Paragraph;
                self.marker = Some(self.next_marker(element));
                owns_marker = true;
            }
            Role::Pre => {
                context.kind = Kind::Preformatted;
                context.pre = true;
            }
            _ => {}
        }

        ancestors.push(element);
        self.children(document, index, &context, ancestors);
        ancestors.pop();
        self.flush();

        if list_pushed {
            self.lists.pop();
        }
        if owns_marker {
            self.marker = None;
        }
        if role == Role::Paragraph && self.items.len() == items_before {
            self.items.push(Item::Blank);
        }
    }

    fn next_marker(&mut self, item: &Element) -> String {
        let depth = self.lists.len();
        let Some(list) = self.lists.last_mut() else {
            return "•".to_owned();
        };
        if !list.ordered {
            return match depth {
                0 | 1 => "•",
                2 => "◦",
                _ => "▪",
            }
            .to_owned();
        }
        if let Some(value) = item.attribute("value").and_then(|v| v.trim().parse().ok()) {
            list.next = value;
        }
        let number = list.next;
        list.next = list.next.saturating_add(1);
        format!("{}.", numeral(number, list.numbering))
    }

    fn image(&mut self, element: &Element, context: &Context) {
        let Some(source) = element
            .attribute("src")
            .or_else(|| element.attribute_any_prefix("href"))
        else {
            return;
        };
        let Some(target) = self.book.resolve(source) else {
            return;
        };
        let alt = crate::toc::title(element.attribute("alt").unwrap_or_default());
        match self.book.image(&target.document) {
            ImageUse::Block(image) => {
                self.flush();
                self.push(RawBlock {
                    kind: BlockKind::Image { image, alt },
                    indent: context.indent,
                    align: Align::Default,
                    spans: Vec::new(),
                });
            }
            // A drop cap's letter is usually its alt text.
            ImageUse::Small if !alt.is_empty() && alt.chars().count() <= 3 => {
                self.text(&alt, context);
            }
            ImageUse::Small | ImageUse::Missing => {}
        }
    }

    fn anchor(&mut self, id: &str) {
        let block = self.items.len() as u32;
        let offset = self.pending.as_ref().map_or(0, |pending| {
            pending.chars + u32::from(pending.space.is_some() && pending.chars > 0)
        });
        self.anchors.entry(id.to_owned()).or_insert((block, offset));
    }

    fn line_break(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.space = None;
            if let Some(last) = pending.spans.last_mut() {
                let trimmed = last.text.trim_end_matches(' ').len();
                pending.chars -= (last.text.len() - trimmed) as u32;
                last.text.truncate(trimmed);
            }
            let style = pending.spans.last().map(|s| s.style).unwrap_or_default();
            self.text_raw("\n", style, None);
        }
    }

    fn text(&mut self, text: &str, context: &Context) {
        if context.pre {
            let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
            // A newline straight after `<pre>` isn't part of its text.
            if self.pending.is_none() && text.starts_with('\n') {
                text.remove(0);
            }
            let kept: String = text
                .chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                .collect();
            if !kept.is_empty() {
                self.start(context);
                self.text_raw(&kept, context.style, context.link);
            }
            return;
        }

        let mut run = String::new();
        for c in text.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C') {
                if !run.is_empty() {
                    self.text_raw(&run, context.style, context.link);
                    run.clear();
                }
                if let Some(pending) = &mut self.pending {
                    pending.space.get_or_insert((context.style, context.link));
                }
                continue;
            }
            if c.is_control() {
                continue;
            }
            if run.is_empty() {
                self.start(context);
                if let Some(pending) = &mut self.pending
                    && let Some((style, link)) = pending.space.take()
                    && pending
                        .spans
                        .last()
                        .is_some_and(|last| !last.text.ends_with('\n'))
                {
                    // In the style of the text it was in, so the space in
                    // `<i>word</i> next` isn't set in italics.
                    self.text_raw(" ", style, link);
                }
            }
            run.push(c);
        }
        if !run.is_empty() {
            self.text_raw(&run, context.style, context.link);
        }
    }

    /// Opens a block for text, if none is open.
    fn start(&mut self, context: &Context) {
        if self.pending.is_some() {
            return;
        }
        let kind = match (context.kind, self.marker.take()) {
            (Kind::Heading(level), _) => BlockKind::Heading { level },
            (Kind::Preformatted, _) => BlockKind::Preformatted,
            (Kind::Paragraph, Some(marker)) => BlockKind::ListItem { marker },
            (Kind::Paragraph, None) => BlockKind::Paragraph {
                indent_first: false,
            },
        };
        self.pending = Some(Pending {
            kind,
            indent: context.indent,
            align: context.align,
            first_indent: context.first_indent,
            spans: Vec::new(),
            chars: 0,
            space: None,
            pre: context.pre,
        });
    }

    /// Adds text as it is, to the open block.
    fn text_raw(&mut self, text: &str, style: Style, link: Option<usize>) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        pending.chars = pending
            .chars
            .saturating_add(text.chars().filter(|c| *c != SOFT_HYPHEN).count() as u32);
        match pending.spans.last_mut() {
            Some(last) if last.style == style && last.link == link => last.text.push_str(text),
            _ => pending.spans.push(RawSpan {
                text: text.to_owned(),
                style,
                link,
            }),
        }
    }

    fn flush(&mut self) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        // Trailing whitespace goes, and with it any span left empty.
        while let Some(last) = pending.spans.last_mut() {
            let trimmed = if pending.pre {
                last.text.trim_end_matches('\n')
            } else {
                last.text.trim_end_matches([' ', '\n'])
            };
            last.text.truncate(trimmed.len());
            if last.text.is_empty() {
                pending.spans.pop();
            } else {
                break;
            }
        }
        pending.spans.retain(|span| !span.text.is_empty());

        let text: String = pending.spans.iter().map(|s| s.text.as_str()).collect();
        let is_paragraph = matches!(pending.kind, BlockKind::Paragraph { .. });
        if text.chars().all(|c| c.is_whitespace() || c == SOFT_HYPHEN) {
            if is_paragraph {
                self.items.push(Item::Blank);
            }
            return;
        }
        if is_paragraph && is_decoration(&text) {
            self.push(RawBlock {
                kind: BlockKind::Separator,
                indent: 0,
                align: Align::Default,
                spans: Vec::new(),
            });
            return;
        }
        self.items.push(Item::Block(
            RawBlock {
                kind: pending.kind,
                indent: pending.indent,
                align: pending.align,
                spans: pending.spans,
            },
            pending.first_indent,
        ));
    }

    fn push(&mut self, block: RawBlock) {
        self.items.push(Item::Block(block, None));
    }

    /// Settles blank paragraphs and separators, works out first-line indents,
    /// and moves each anchor to the block it ended up in.
    fn finish(self) -> BuiltChapter {
        let mut blocks: Vec<RawBlock> = Vec::new();
        // For each item, the index of the first block at or after it.
        let mut moved = Vec::with_capacity(self.items.len() + 1);
        let mut blank_waiting = false;
        let mut indents: Vec<Option<bool>> = Vec::new();

        for item in self.items {
            moved.push(blocks.len() as u32);
            match item {
                Item::Blank => blank_waiting = true,
                Item::Block(block, first_indent) => {
                    let after_paragraph = blocks
                        .last()
                        .is_some_and(|b| matches!(b.kind, BlockKind::Paragraph { .. }));
                    let is_paragraph = matches!(block.kind, BlockKind::Paragraph { .. });
                    if std::mem::take(&mut blank_waiting) && after_paragraph && is_paragraph {
                        blocks.push(separator());
                        indents.push(None);
                    }
                    let is_separator = block.kind == BlockKind::Separator;
                    let repeats = blocks
                        .last()
                        .is_some_and(|b| b.kind == BlockKind::Separator);
                    if is_separator && (blocks.is_empty() || repeats) {
                        continue;
                    }
                    blocks.push(block);
                    indents.push(first_indent);
                }
            }
        }
        moved.push(blocks.len() as u32);
        if blocks
            .last()
            .is_some_and(|b| b.kind == BlockKind::Separator)
        {
            blocks.pop();
            indents.pop();
        }

        for at in 0..blocks.len() {
            let follows_paragraph =
                at > 0 && matches!(blocks[at - 1].kind, BlockKind::Paragraph { .. });
            let block = &mut blocks[at];
            if let BlockKind::Paragraph { indent_first } = &mut block.kind {
                *indent_first = follows_paragraph
                    && block.align == Align::Default
                    && indents[at].unwrap_or(true);
            }
        }

        let last = blocks.len().saturating_sub(1) as u32;
        let anchors = self
            .anchors
            .into_iter()
            .map(|(id, (item, offset))| {
                let block = moved.get(item as usize).copied().unwrap_or(last);
                // An anchor after the last block points at the last block.
                if block > last {
                    (id, (last, u32::MAX))
                } else if moved.get(item as usize) == moved.get(item as usize + 1) {
                    // Its item was dropped, so it points at the next block's start.
                    (id, (block, 0))
                } else {
                    (id, (block, offset))
                }
            })
            .collect();
        BuiltChapter { blocks, anchors }
    }
}

fn separator() -> RawBlock {
    RawBlock {
        kind: BlockKind::Separator,
        indent: 0,
        align: Align::Default,
        spans: Vec::new(),
    }
}

/// A short paragraph of only ornaments, as in `* * *`, is a scene break.
fn is_decoration(text: &str) -> bool {
    let mut marks = 0;
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        if !"*#~•·⁂❦❧✻✽✦✧◇◆○●-–—_=+§".contains(c) {
            return false;
        }
        marks += 1;
    }
    (1..=12).contains(&marks)
}

/// The SVG `<image>`s inside an element.
fn svg_images(document: &Document, from: usize) -> Vec<(usize, &Element)> {
    let mut found = Vec::new();
    let mut stack = vec![from];
    while let Some(index) = stack.pop() {
        if let Some(element) = document.element(index) {
            if element.name == "image" {
                found.push((index, element));
            } else {
                stack.extend(element.children.iter().rev());
            }
        }
    }
    found
}

/// A list number in the style `type` asks for: `1`, `a`, `A`, `i` or `I`.
fn numeral(number: i64, numbering: u8) -> String {
    match numbering {
        b'a' | b'A' if (1..=18_278).contains(&number) => {
            let mut n = number;
            let mut letters = Vec::new();
            while n > 0 {
                n -= 1;
                letters.push(b'a' + (n % 26) as u8);
                n /= 26;
            }
            letters.reverse();
            let text = String::from_utf8(letters).unwrap_or_default();
            if numbering == b'A' {
                text.to_uppercase()
            } else {
                text
            }
        }
        b'i' | b'I' if (1..4000).contains(&number) => {
            let text = roman(number as u32);
            if numbering == b'I' {
                text.to_uppercase()
            } else {
                text
            }
        }
        _ => number.to_string(),
    }
}

fn roman(mut number: u32) -> String {
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, letters) in NUMERALS {
        while number >= value {
            out.push_str(letters);
            number -= value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;
    use crate::html::HtmlLimits;

    const LIMITS: HtmlLimits = HtmlLimits {
        max_nodes: 100_000,
        max_depth: 64,
    };

    #[derive(Default)]
    struct TestBook {
        targets: Vec<Target>,
        budget: Option<u64>,
    }

    impl Book for TestBook {
        fn resolve(&self, href: &str) -> Option<Target> {
            if href.contains("://") {
                return None;
            }
            let (document, fragment) = match href.split_once('#') {
                Some((document, fragment)) => (document, Some(fragment.to_owned())),
                None => (href, None),
            };
            Some(Target {
                document: if document.is_empty() {
                    "this.xhtml"
                } else {
                    document
                }
                .to_owned(),
                fragment,
            })
        }

        fn image(&mut self, entry: &str) -> ImageUse {
            match entry {
                "big.png" => ImageUse::Block(0),
                "small.png" => ImageUse::Small,
                _ => ImageUse::Missing,
            }
        }

        fn link(&mut self, target: Target) -> usize {
            self.targets.push(target);
            self.targets.len() - 1
        }

        fn charge(&mut self, units: u64) -> bool {
            match &mut self.budget {
                None => true,
                Some(budget) => match budget.checked_sub(units) {
                    Some(left) => {
                        *budget = left;
                        true
                    }
                    None => false,
                },
            }
        }
    }

    fn built_with(css: &str, html: &str, book: &mut TestBook) -> BuiltChapter {
        let mut styles = Styles::new(1000);
        styles.add_sheet(css);
        chapter(&Document::parse(html, LIMITS), &styles, book)
    }

    fn built(css: &str, html: &str) -> BuiltChapter {
        built_with(css, html, &mut TestBook::default())
    }

    /// Blocks as short strings, such as `p|text` or `h1|Title`, with styled
    /// spans marked: `_italic_`, `*bold*`, `^super^`.
    fn shapes(chapter: &BuiltChapter) -> Vec<String> {
        chapter
            .blocks
            .iter()
            .map(|block| {
                let kind = match &block.kind {
                    BlockKind::Paragraph { indent_first: true } => "p>".to_owned(),
                    BlockKind::Paragraph {
                        indent_first: false,
                    } => "p".to_owned(),
                    BlockKind::Heading { level } => format!("h{level}"),
                    BlockKind::ListItem { marker } => format!("li{marker}"),
                    BlockKind::Preformatted => "pre".to_owned(),
                    BlockKind::Image { image, alt } => format!("img{image}:{alt}"),
                    BlockKind::Separator => "---".to_owned(),
                };
                let mut text = String::new();
                for span in &block.spans {
                    let mark = match span.style {
                        s if s.emphasis => "_",
                        s if s.strong => "*",
                        s if s.vertical == Vertical::Super => "^",
                        s if s.small_caps => "~",
                        s if s.code => "`",
                        _ => "",
                    };
                    write!(text, "{mark}{}{mark}", span.text).unwrap();
                }
                let indent = if block.indent > 0 {
                    format!("{}", block.indent)
                } else {
                    String::new()
                };
                let align = match block.align {
                    Align::Center => "c",
                    Align::Right => "r",
                    _ => "",
                };
                format!("{kind}{indent}{align}|{text}")
            })
            .collect()
    }

    #[test]
    fn paragraphs_headings_and_whitespace() {
        let found = built(
            "",
            "<body><h1>  The <b>Title</b> </h1>
             <p>First   line\n of <i>text</i> .</p>
             <p> Second.</p>
             Loose text
             <div>In a div</div></body>",
        );
        assert_eq!(
            shapes(&found),
            [
                "h1|The *Title*",
                "p|First line of _text_ .",
                "p>|Second.",
                "p>|Loose text",
                "p>|In a div",
            ]
        );
    }

    #[test]
    fn styles_come_from_tags_and_classes() {
        let found = built(
            ".it { font-style: italic } .sc { font-variant: small-caps } .c { text-align: center } .flat { text-indent: 0 }",
            r#"<p class="c">Centered</p><p>A <span class="it">word</span> and <span class="sc">caps</span>x<sup>2</sup></p>
               <p class="flat">Flat</p><p style="display:none">Hidden</p><p hidden="">Also</p>"#,
        );
        assert_eq!(
            shapes(&found),
            ["pc|Centered", "p>|A _word_ and ~caps~x^2^", "p|Flat"]
        );
    }

    #[test]
    fn lists_quotes_and_preformatted_text() {
        let found = built(
            "",
            r#"<ol start="3"><li>Three</li><li><p>Four</p><p>more</p></li>
               <li><ul><li>inner</li></ul></li></ol>
               <ol type="i"><li value="4">iv</li></ol>
               <blockquote><p>Quoted</p></blockquote>
               <pre>
  code  here
    indented</pre>"#,
        );
        assert_eq!(
            shapes(&found),
            [
                "li3.1|Three",
                "li4.1|Four",
                "p1|more",
                "li◦2|inner",
                "liiv.1|iv",
                "p1|Quoted",
                "pre|`  code  here\n    indented`",
            ]
        );
    }

    #[test]
    fn line_breaks_and_tables() {
        let found = built(
            "",
            "<p>Roses are red, <br/> violets <br/>are blue<br/></p>
             <table><tr><th>A</th><td>B</td></tr><tr><td>C</td></tr></table>",
        );
        assert_eq!(
            shapes(&found),
            ["p|Roses are red,\nviolets\nare blue", "p|A · B", "p|C",]
        );
    }

    #[test]
    fn scene_breaks() {
        let found = built(
            "",
            "<p>&#160;</p><h2>Start</h2><p></p><p>One.</p><p>&#160;</p><p>Two.</p>
             <p class='x'>* * *</p><hr/><p>Three.</p><hr/>",
        );
        assert_eq!(
            shapes(&found),
            ["h2|Start", "p|One.", "---|", "p|Two.", "---|", "p|Three."]
        );
    }

    #[test]
    fn images() {
        let found = built(
            "",
            r#"<p>Before</p><p><img src="big.png" alt="A  map"/></p>
               <p><img src="small.png" alt="T"/>he story <img src="small.png"/>went.</p>
               <svg><image xlink:href="big.png"/></svg><img src="gone.png"/>"#,
        );
        assert_eq!(
            shapes(&found),
            ["p|Before", "img0:A map|", "p|The story went.", "img0:|"]
        );
    }

    #[test]
    fn anchors_and_links() {
        let mut book = TestBook::default();
        let found = built_with(
            "",
            r##"<h1 id="top">T</h1><p id="empty"></p><p>See <a id="ref" href="#note">note</a>.</p>
               <div id="before"/><p>x<a href="http://x.example">out</a>
               <span epub:type="pagebreak" id="page2">2</span><a name="old"/></p>
               <aside id="note">Note.</aside><span id="end"/>"##,
            &mut book,
        );
        assert_eq!(found.anchors["top"], (0, 0));
        assert_eq!(found.anchors["ref"], (1, 4));
        assert_eq!(
            found.anchors["empty"],
            (1, 0),
            "a dropped block's anchor moves on"
        );
        assert_eq!(found.anchors["before"], (2, 0));
        assert_eq!(
            found.anchors["old"],
            (2, 5),
            "after the space; clamped later"
        );
        assert_eq!(found.anchors["note"], (3, 0));
        assert_eq!(found.anchors["end"].0, 3);
        assert!(!found.anchors.contains_key("page2"));
        assert_eq!(shapes(&found)[2], "p>|xout");

        let link = found.blocks[1].spans[1].link.unwrap();
        assert_eq!(
            book.targets[link],
            Target {
                document: "this.xhtml".to_owned(),
                fragment: Some("note".to_owned())
            }
        );
        assert!(found.blocks[2].spans.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn styles_stop_when_the_budget_runs_out() {
        let mut book = TestBook {
            budget: Some(2),
            ..TestBook::default()
        };
        let found = built_with(
            ".it { font-style: italic }",
            r#"<p class="it">a</p><p class="it">b</p><p class="it">c</p>"#,
            &mut book,
        );
        assert_eq!(shapes(&found), ["p|_a_", "p>|_b_", "p>|c"]);
    }

    #[test]
    fn numerals() {
        assert_eq!(numeral(4, b'1'), "4");
        assert_eq!(numeral(28, b'a'), "ab");
        assert_eq!(numeral(3, b'A'), "C");
        assert_eq!(numeral(1994, b'I'), "MCMXCIV");
        assert_eq!(numeral(-1, b'i'), "-1");
    }
}
