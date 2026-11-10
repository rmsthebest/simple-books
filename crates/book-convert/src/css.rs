//! Just enough CSS to see what a book's styles mean for its text: italics,
//! bold, small caps, alignment and hiding. Many books put italics in a class
//! rather than an `<i>`, so this matters more than it might seem.
//!
//! Selectors are a tag, classes and an id, joined by descendant or child
//! combinators. Anything else (attributes, pseudo-classes, siblings) makes a
//! selector be skipped, which loses a style but never applies one wrongly.
//! Matching is greedy and never backtracks, so its cost stays linear.

use std::collections::HashMap;

use book_format::{Align, Vertical};

use crate::html::Element;

const MAX_COMPOUNDS: usize = 8;

/// What the styles say about an element. `None` is "not said".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Props {
    pub(crate) italic: Option<bool>,
    pub(crate) bold: Option<bool>,
    pub(crate) small_caps: Option<bool>,
    pub(crate) monospace: Option<bool>,
    pub(crate) vertical: Option<Vertical>,
    pub(crate) align: Option<Align>,
    /// Whether the first line is indented.
    pub(crate) indent: Option<bool>,
    pub(crate) display: Option<Display>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Display {
    None,
    Block,
    Inline,
}

impl Props {
    /// Lays `other` over this.
    fn apply(&mut self, other: Self) {
        macro_rules! over {
            ($($field:ident),*) => { $( if other.$field.is_some() { self.$field = other.$field; } )* };
        }
        over!(
            italic, bold, small_caps, monospace, vertical, align, indent, display
        );
    }

    /// Reads declarations such as `font-style: italic; text-align: center`.
    pub(crate) fn parse(declarations: &str) -> Self {
        let mut props = Self::default();
        for declaration in declarations.split(';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.to_ascii_lowercase();
            let value = value.trim().trim_end_matches("!important").trim();
            props.declare(&name, value);
        }
        props
    }

    fn declare(&mut self, name: &str, value: &str) {
        match name {
            "font-style" => self.italic = italic(value),
            "font-weight" => self.bold = bold(value),
            "font-variant" | "font-variant-caps" => {
                self.small_caps = match value {
                    "small-caps" | "all-small-caps" => Some(true),
                    "normal" => Some(false),
                    _ => None,
                };
            }
            "font-family" => self.monospace = Some(monospace(value)),
            "font" => {
                for word in value.split_ascii_whitespace() {
                    self.italic = italic(word).or(self.italic);
                    self.bold = bold(word).or(self.bold);
                    if word == "small-caps" {
                        self.small_caps = Some(true);
                    }
                }
                if value.contains("monospace") || value.contains("courier") {
                    self.monospace = Some(true);
                }
            }
            "vertical-align" => {
                self.vertical = match value {
                    "super" | "top" | "text-top" => Some(Vertical::Super),
                    "sub" | "bottom" | "text-bottom" => Some(Vertical::Sub),
                    "baseline" => Some(Vertical::Baseline),
                    _ => None,
                };
            }
            "text-align" => self.align = align(value),
            "text-indent" => self.indent = indent(value),
            "display" => {
                self.display = match value.split_ascii_whitespace().next() {
                    Some("none") => Some(Display::None),
                    Some("inline" | "inline-block" | "inline-flex" | "contents") => {
                        Some(Display::Inline)
                    }
                    Some(_) => Some(Display::Block),
                    None => None,
                };
            }
            _ => {}
        }
    }
}

fn italic(value: &str) -> Option<bool> {
    match value {
        "italic" | "oblique" => Some(true),
        "normal" => Some(false),
        _ => None,
    }
}

fn bold(value: &str) -> Option<bool> {
    match value {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        number => number.parse::<u32>().ok().map(|weight| weight >= 600),
    }
}

fn monospace(value: &str) -> bool {
    let mono = ["monospace", "courier", "mono", "consolas", "menlo"];
    mono.iter().any(|name| value.contains(name))
}

/// Left and justified both mean "as the reader likes" here.
fn align(value: &str) -> Option<Align> {
    match value {
        "center" | "-webkit-center" => Some(Align::Center),
        "right" | "end" => Some(Align::Right),
        "left" | "start" | "justify" => Some(Align::Default),
        _ => None,
    }
}

/// A positive indent indents; zero, or a negative (hanging) one, doesn't.
fn indent(value: &str) -> Option<bool> {
    let number: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
        .collect();
    let number: f64 = number.parse().ok()?;
    Some(number > 0.0)
}

#[derive(Debug, Clone, PartialEq)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Compound {
    fn matches(&self, element: &Element) -> bool {
        self.tag.as_ref().is_none_or(|tag| *tag == element.name)
            && self
                .id
                .as_ref()
                .is_none_or(|id| element.attribute("id") == Some(id))
            && self
                .classes
                .iter()
                .all(|class| element.classes().any(|c| c == class))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Combinator {
    Descendant,
    Child,
}

#[derive(Debug, Clone)]
struct Rule {
    /// The subject last; each earlier compound has the combinator that joins
    /// it to the one after.
    compounds: Vec<(Compound, Combinator)>,
    specificity: (u32, u32, u32),
    order: usize,
    props: Props,
}

impl Rule {
    /// `ancestors` runs from the root to the element's parent.
    fn matches(&self, element: &Element, ancestors: &[&Element]) -> bool {
        let Some(((subject, _), rest)) = self.compounds.split_last() else {
            return false;
        };
        if !subject.matches(element) {
            return false;
        }
        let mut remaining = ancestors;
        for (compound, combinator) in rest.iter().rev() {
            match combinator {
                Combinator::Child => match remaining.split_last() {
                    Some((parent, rest)) if compound.matches(parent) => remaining = rest,
                    _ => return false,
                },
                Combinator::Descendant => {
                    match remaining.iter().rposition(|a| compound.matches(a)) {
                        Some(at) => remaining = &remaining[..at],
                        None => return false,
                    }
                }
            }
        }
        true
    }
}

/// A book's stylesheets, indexed by what each rule's subject requires.
#[derive(Debug, Default)]
pub(crate) struct Styles {
    rules: Vec<Rule>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    max_rules: usize,
}

impl Styles {
    pub(crate) fn new(max_rules: usize) -> Self {
        Self {
            max_rules,
            ..Self::default()
        }
    }

    pub(crate) fn add_sheet(&mut self, text: &str) {
        for (selectors, declarations) in rules(text) {
            let props = Props::parse(declarations);
            if props == Props::default() {
                continue;
            }
            for selector in selectors.split(',') {
                if self.rules.len() >= self.max_rules {
                    return;
                }
                if let Some(compounds) = parse_selector(selector) {
                    self.add(compounds, props);
                }
            }
        }
    }

    fn add(&mut self, compounds: Vec<(Compound, Combinator)>, props: Props) {
        let mut specificity = (0, 0, 0);
        for (compound, _) in &compounds {
            specificity.0 += u32::from(compound.id.is_some());
            specificity.1 += compound.classes.len() as u32;
            specificity.2 += u32::from(compound.tag.is_some());
        }
        let index = self.rules.len();
        let subject = &compounds[compounds.len() - 1].0;
        if let Some(id) = &subject.id {
            self.by_id.entry(id.clone()).or_default().push(index);
        } else if let Some(class) = subject.classes.first() {
            self.by_class.entry(class.clone()).or_default().push(index);
        } else if let Some(tag) = &subject.tag {
            self.by_tag.entry(tag.clone()).or_default().push(index);
        } else {
            self.universal.push(index);
        }
        self.rules.push(Rule {
            compounds,
            specificity,
            order: index,
            props,
        });
    }

    /// What the stylesheets and the `style` attribute say about an element.
    pub(crate) fn props(&self, element: &Element, ancestors: &[&Element]) -> Props {
        let mut candidates: Vec<usize> = self.universal.clone();
        if let Some(rules) = self.by_tag.get(&element.name) {
            candidates.extend(rules);
        }
        if let Some(id) = element.attribute("id")
            && let Some(rules) = self.by_id.get(id)
        {
            candidates.extend(rules);
        }
        for class in element.classes() {
            if let Some(rules) = self.by_class.get(class) {
                candidates.extend(rules);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();

        let mut matched: Vec<&Rule> = candidates
            .into_iter()
            .map(|index| &self.rules[index])
            .filter(|rule| rule.matches(element, ancestors))
            .collect();
        matched.sort_by_key(|rule| (rule.specificity, rule.order));

        let mut props = Props::default();
        for rule in matched {
            props.apply(rule.props);
        }
        if let Some(style) = element.attribute("style") {
            props.apply(Props::parse(style));
        }
        props
    }
}

/// The rules in a stylesheet, as (selectors, declarations), outside any
/// at-rule. A rule inside `@media` is skipped along with it.
fn rules(text: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start = 0;
    let mut open_at = None;
    let mut at = 0;
    let mut quote = None;
    while at < bytes.len() {
        let byte = bytes[at];
        match (quote, byte) {
            (Some(q), b) if b == q => quote = None,
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'/') if bytes.get(at + 1) == Some(&b'*') => {
                at = text[at + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| at + 2 + end + 1);
            }
            (None, b'{') => {
                if depth == 0 {
                    open_at = Some(at);
                }
                depth += 1;
            }
            (None, b'}') if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(open) = open_at.take() {
                        let selectors = strip_comments(&text[start..open]);
                        if !selectors.trim_start().starts_with('@') {
                            found.push((selectors, &text[open + 1..at]));
                        }
                    }
                    start = at + 1;
                }
            }
            (None, b';') if depth == 0 => start = at + 1, // `@import ...;`
            _ => {}
        }
        at += 1;
    }
    found
}

/// Leaves out a comment that ends a stretch, the usual place for one before a
/// selector. Comments elsewhere in a selector make it fail to parse.
fn strip_comments(text: &str) -> &str {
    text.rsplit("*/").next().unwrap_or(text)
}

fn parse_selector(selector: &str) -> Option<Vec<(Compound, Combinator)>> {
    let mut compounds = Vec::new();
    let mut combinator = Combinator::Descendant;
    for token in selector.replace('>', " > ").split_ascii_whitespace() {
        if token == ">" {
            combinator = Combinator::Child;
            continue;
        }
        if let Some((_, previous)) = compounds.last_mut() {
            *previous = combinator;
        }
        compounds.push((parse_compound(token)?, Combinator::Descendant));
        combinator = Combinator::Descendant;
        if compounds.len() > MAX_COMPOUNDS {
            return None;
        }
    }
    (!compounds.is_empty() && combinator == Combinator::Descendant).then_some(compounds)
}

fn parse_compound(token: &str) -> Option<Compound> {
    let mut compound = Compound {
        tag: None,
        id: None,
        classes: Vec::new(),
    };
    let mut rest = token;
    let name_len = rest.find(['.', '#']).unwrap_or(rest.len());
    let tag = &rest[..name_len];
    // A namespace, as in `html|p`, is left out.
    let tag = tag.rsplit('|').next().unwrap_or(tag);
    if !tag.is_empty() && tag != "*" {
        if !is_ident(tag) {
            return None;
        }
        compound.tag = Some(tag.to_ascii_lowercase());
    }
    rest = &rest[name_len..];
    while let Some(kind) = rest.chars().next() {
        let body = &rest[1..];
        let len = body.find(['.', '#']).unwrap_or(body.len());
        let name = &body[..len];
        if !is_ident(name) {
            return None;
        }
        match kind {
            '.' => compound.classes.push(name.to_owned()),
            _ => compound.id = Some(name.to_owned()),
        }
        rest = &body[len..];
    }
    Some(compound)
}

fn is_ident(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::{Document, HtmlLimits};

    const LIMITS: HtmlLimits = HtmlLimits {
        max_nodes: 1000,
        max_depth: 64,
    };

    /// The props of the innermost element of `html`, which must be a chain of
    /// single elements.
    fn props_of(css: &str, html: &str) -> Props {
        let mut styles = Styles::new(1000);
        styles.add_sheet(css);
        let document = Document::parse(html, LIMITS);
        let chain: Vec<&Element> = document.elements().map(|(_, e)| e).skip(1).collect();
        let (element, ancestors) = chain.split_last().unwrap();
        styles.props(element, ancestors)
    }

    #[test]
    fn classes_tags_and_ids() {
        let css = ".it { font-style: italic } p.b { font-weight: 700 } #x { text-align: center }";
        let found = props_of(css, r#"<p class="b it" id="x"/>"#);
        assert_eq!(found.italic, Some(true));
        assert_eq!(found.bold, Some(true));
        assert_eq!(found.align, Some(Align::Center));
        assert_eq!(props_of(css, r#"<div class="b"/>"#).bold, None);
    }

    #[test]
    fn combinators() {
        let css = ".poem p { font-style: italic } div > span { font-variant: small-caps }";
        assert_eq!(
            props_of(css, r#"<div class="poem"><div><p/></div></div>"#).italic,
            Some(true)
        );
        assert_eq!(props_of(css, "<div><p/></div>").italic, None);
        assert_eq!(props_of(css, "<div><span/></div>").small_caps, Some(true));
        assert_eq!(props_of(css, "<div><b><span/></b></div>").small_caps, None);
    }

    #[test]
    fn specificity_order_and_inline_style() {
        let css = "p.a { font-style: normal } .a { font-style: italic } p { text-indent: 1em } p { text-indent: 0 }";
        let found = props_of(css, r#"<p class="a"/>"#);
        assert_eq!(found.italic, Some(false), "p.a is more specific");
        assert_eq!(found.indent, Some(false), "the later rule wins");
        let found = props_of(
            css,
            r#"<p class="a" style="font-style: italic !important"/>"#,
        );
        assert_eq!(found.italic, Some(true));
    }

    #[test]
    fn skips_what_it_cant_follow() {
        let css = "@import url(x.css); /* note */ .a { font-style: italic }
            @media screen { .b { font-weight: bold } }
            .c:first-child, .d + .e, [lang] { font-weight: bold }
            @font-face { font-family: X; src: url('}.ttf') }
            .f { content: '}'; font-weight: bold }";
        assert_eq!(props_of(css, r#"<p class="a"/>"#).italic, Some(true));
        assert_eq!(props_of(css, r#"<p class="b"/>"#).bold, None);
        assert_eq!(props_of(css, r#"<p class="c"/>"#).bold, None);
        assert_eq!(props_of(css, r#"<p class="e"/>"#).bold, None);
        assert_eq!(props_of(css, r#"<p class="f"/>"#).bold, Some(true));
    }

    #[test]
    fn reads_values() {
        let found = Props::parse(
            "display:none; vertical-align: super; font-family: 'Courier New', monospace; text-indent: -2em",
        );
        assert_eq!(found.display, Some(Display::None));
        assert_eq!(found.vertical, Some(Vertical::Super));
        assert_eq!(found.monospace, Some(true));
        assert_eq!(found.indent, Some(false));
        assert_eq!(Props::parse("text-indent: 1.5em").indent, Some(true));
        assert_eq!(
            Props::parse("font: italic bold 12px serif").bold,
            Some(true)
        );
    }

    #[test]
    fn broken_sheets_are_harmless() {
        for css in ["}}}{{{", "a { b", "/* never ends", "'", ".a{}", "{x:y}"] {
            let mut styles = Styles::new(10);
            styles.add_sheet(css);
        }
        let mut styles = Styles::new(3);
        styles.add_sheet(&".a { font-style: italic }".repeat(100));
        assert_eq!(styles.rules.len(), 3);
    }
}
