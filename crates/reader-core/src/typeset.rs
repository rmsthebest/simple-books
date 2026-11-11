//! Lays out one block at a time with Parley and draws the lines it made.
//!
//! Parley breaks lines at soft hyphens but doesn't draw a hyphen there, so
//! this does. A block with soft hyphens is broken one hyphen narrower than
//! the column. Lines that end at a hyphen are justified to that narrower
//! width, and the "-" drawn after them ends at the margin. The other lines
//! are justified to the full column.
//!
//! Links are underlined, and where they were drawn is kept so a tap can find
//! them. Parley carries each run's link target as its brush.

use std::borrow::Cow;

use book_format::{Align, Block, BlockKind, Chapter, ImageInfo, Position, SOFT_HYPHEN, Vertical};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFeatures, FontStyle, FontVariations,
    FontWeight, IndentOptions, Layout, LayoutContext, LineHeight, OverflowWrap,
    PositionedLayoutItem, StyleProperty, VerticalAlign, WhiteSpaceCollapse, YieldData,
    fontique::Language,
};

use crate::{Canvas, Settings, fonts, knuth_plass, render::Glyphs};

/// Stands in for a space inside inline code. Parley justifies by widening
/// ordinary spaces, and leaves fixed-width ones like this alone; a line may
/// still break after it. Intel One Mono has no glyph for it, so Literata's is
/// used, widened with letter spacing to a monospace space.
pub(crate) const CODE_SPACE: char = '\u{2008}';

/// Parley's brush: where a run links to, if anywhere.
pub(crate) type Link = Option<Position>;

/// Where a link was drawn on a page, for finding what a tap hit. Spans the
/// height of its line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LinkArea {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub target: Position,
}

impl LinkArea {
    /// How far (`x`, `y`) is from the area: 0 inside it.
    pub(crate) fn distance(&self, x: f32, y: f32) -> f32 {
        let dx = (self.left - x).max(x - self.right).max(0.0);
        let dy = (self.top - y).max(y - self.bottom).max(0.0);
        dx.hypot(dy)
    }
}

/// Where a word was drawn on a page, for finding what a long press rests
/// on. Spans the height of its line.
#[derive(Debug, Clone, PartialEq)]
pub struct WordArea {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    /// The whole word as the book has it, with the punctuation next to it
    /// and any soft hyphens, even where the line breaks it.
    pub text: String,
}

impl WordArea {
    /// How far (`x`, `y`) is from the area: 0 inside it.
    pub(crate) fn distance(&self, x: f32, y: f32) -> f32 {
        let dx = (self.left - x).max(x - self.right).max(0.0);
        let dy = (self.top - y).max(y - self.bottom).max(0.0);
        dx.hypot(dy)
    }
}

/// A drawn page: its pixels, and where its links and words are.
#[derive(Debug, Clone)]
pub(crate) struct Page {
    pub canvas: Canvas,
    pub links: Vec<LinkArea>,
    pub words: Vec<WordArea>,
}

impl Page {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            canvas: Canvas::new(width, height),
            links: Vec::new(),
            words: Vec::new(),
        }
    }

    /// Clears it for drawing again.
    pub(crate) fn clear(&mut self) {
        self.canvas.clear();
        self.links.clear();
        self.words.clear();
    }
}

/// Whether `c` comes between words rather than in one: a space, or a dash
/// or slash that joins two words without spaces.
fn between_words(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{2013}' | '\u{2014}' | '\u{2015}' | '/' | '\u{2026}')
}

/// A link's run of ink on one line, collected while the line is drawn.
struct Underline {
    area: LinkArea,
    /// The top of the underline, and its thickness.
    y: f32,
    thickness: f32,
}

impl Underline {
    fn finish(self, page: &mut Page) {
        let left = self.area.left.round();
        let width = (self.area.right.round() - left).max(0.0);
        let thickness = self.thickness.round().max(1.0);
        page.canvas.underline(
            left as i32,
            self.y.round() as i32,
            width as u32,
            thickness as u32,
            thickness as u32,
        );
        page.links.push(self.area);
    }
}

/// A typographic point in inches.
const POINT: f32 = 1.0 / 72.0;

/// One line of a laid-out block, or the whole of an image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LineInfo {
    /// Where the line starts, in characters into the block, not counting
    /// soft hyphens.
    pub offset: u32,
    pub height: f32,
}

/// What [`Typesetter::block`] works out for a block.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placed {
    pub lines: Vec<LineInfo>,
    /// Space above the block, already merged with the previous block's space
    /// below. Dropped at the top of a page.
    pub space_before: f32,
    /// A heading is kept on the same page as what follows it.
    pub keep_with_next: bool,
}

/// How a block is set, from its kind and its neighbour.
#[derive(Debug, Clone, Copy)]
struct BlockStyle {
    scale: f32,
    weight: Option<f32>,
    alignment: Alignment,
    /// From the column's left edge.
    inset: f32,
    first_line_indent: f32,
    space_before: f32,
    space_after: f32,
    keep_with_next: bool,
    preformatted: bool,
}

/// What's in the [`Layout`] right now, so drawing can check it's the block it
/// was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Current {
    chapter: u32,
    block: usize,
}

/// A chapter and what's needed to lay it out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChapterRef<'a> {
    /// The chapter's index in the book, which identifies it to the typesetter.
    pub index: u32,
    pub chapter: &'a Chapter,
    pub images: &'a [ImageInfo],
}

/// Lays out and draws blocks. Keep one per thread: it holds Parley's caches
/// and the glyph cache.
pub struct Typesetter {
    settings: Settings,
    fonts: FontContext,
    layout_cx: LayoutContext<Link>,
    layout: Layout<Link>,
    /// The list marker or separator being drawn beside a block.
    aux: Layout<Link>,
    locale: Option<Language>,
    text: String,
    /// For each line in `layout`, whether it ends at a soft hyphen.
    hyphens: Vec<bool>,
    current: Option<Current>,
    /// Whether the block in `layout` was broken with Knuth–Plass.
    optimal: bool,
    hyphen_em: f32,
    /// A monospace space, per pixel of font size.
    code_space: f32,
    /// What [`CODE_SPACE`] is short of a monospace space, per pixel.
    code_space_spacing: f32,
    /// Where the current block's code spaces are, to widen them.
    code_spaces: Vec<std::ops::Range<usize>>,
    glyphs: Glyphs,
}

impl std::fmt::Debug for Typesetter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Typesetter")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Typesetter {
    /// `language` is the book's BCP 47 tag, which picks shaping and
    /// line-breaking rules.
    pub fn new(settings: Settings, language: Option<&str>) -> Self {
        let mut fonts = fonts::context();
        let hyphen_em = fonts::advance_em(&mut fonts, fonts::BODY_FAMILY, '-').unwrap_or(0.33);
        let code_space = fonts::advance_em(&mut fonts, fonts::MONO_FAMILY, ' ').unwrap_or(0.6);
        let code_space_spacing = code_space
            - fonts::advance_em(&mut fonts, fonts::BODY_FAMILY, CODE_SPACE).unwrap_or(code_space);
        let glyphs = Glyphs::new(settings.gamma);
        Self {
            settings,
            fonts,
            layout_cx: LayoutContext::new(),
            layout: Layout::new(),
            aux: Layout::new(),
            locale: language.and_then(|tag| tag.parse().ok()),
            text: String::new(),
            hyphens: Vec::new(),
            current: None,
            optimal: false,
            hyphen_em,
            code_space,
            code_space_spacing,
            code_spaces: Vec::new(),
            glyphs,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Lays out block `index` of a chapter, and keeps the layout for
    /// [`Self::draw_lines`].
    pub(crate) fn block(&mut self, at: ChapterRef<'_>, index: usize) -> Placed {
        let chapter = at.chapter;
        let block = &chapter.blocks[index];
        let previous = index.checked_sub(1).map(|i| &chapter.blocks[i]);
        let style = self.style(block, previous);
        let previous_after = previous.map_or(0.0, |p| self.style(p, None).space_after);
        let space_before = style.space_before.max(previous_after);

        let lines = if let BlockKind::Image { image, .. } = &block.kind {
            self.current = None;
            let (_, height) = self.image_size(at.images.get(*image as usize));
            vec![LineInfo { offset: 0, height }]
        } else {
            self.lay_out(block, &style);
            self.current = Some(Current {
                chapter: at.index,
                block: index,
            });
            self.line_infos()
        };
        Placed {
            lines,
            space_before,
            keep_with_next: style.keep_with_next,
        }
    }

    /// Draws `lines` of block `index`, with the first one's top at `top`.
    /// `image` decodes an image of the book.
    pub(crate) fn draw_lines(
        &mut self,
        page: &mut Page,
        at: ChapterRef<'_>,
        index: usize,
        lines: std::ops::Range<usize>,
        top: f32,
        image: &mut dyn FnMut(u32) -> Option<image::GrayImage>,
    ) {
        let chapter = at.chapter;
        let block = &chapter.blocks[index];
        let left = self.settings.margin_side as f32;
        if let BlockKind::Image { image: id, .. } = &block.kind {
            let (width, height) = self.image_size(at.images.get(*id as usize));
            if let Some(gray) = image(*id) {
                let x = left + (self.settings.content_width() - width) / 2.0;
                crate::render::image(&mut page.canvas, &gray, x, top, width, height);
            }
            return;
        }
        let wanted = Current {
            chapter: at.index,
            block: index,
        };
        if self.current != Some(wanted) {
            self.block(at, index);
        }
        let previous = index.checked_sub(1).map(|i| &chapter.blocks[i]);
        let style = self.style(block, previous);
        let x0 = left + style.inset;

        let Some(first) = self.layout.get(lines.start) else {
            return;
        };
        let y0 = top - first.metrics().block_min_coord;
        for line_index in lines.clone() {
            self.draw_line(page, line_index, x0, y0);
        }

        // A list marker sits in the gutter before the first line.
        if lines.start == 0
            && let BlockKind::ListItem { marker } = &block.kind
            && let Some(line) = self.layout.get(0)
        {
            let baseline = y0 + line.metrics().baseline;
            let size = self.settings.font_size * style.scale;
            let gap = self.settings.font_size * 0.5;
            self.lay_out_aux(marker, size, self.settings.weight_above(0.0));
            let x = x0 - gap - self.aux.width();
            self.draw_aux(&mut page.canvas, x, baseline);
        }
    }

    /// Draws a line of the block in the layout, with the layout's origin at
    /// (`x0`, `y0`) on the page, and underlines and records its links.
    fn draw_line(&mut self, page: &mut Page, line_index: usize, x0: f32, y0: f32) {
        let Some(line) = self.layout.get(line_index) else {
            return;
        };
        let mut last_run = None;
        let mut underline: Option<Underline> = None;
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                let font = run.run().font();
                let size = run.run().font_size();
                let coords: Vec<i16> = run
                    .run()
                    .normalized_coords()
                    .iter()
                    .map(|c| c.to_bits())
                    .collect();
                // From the first inked glyph to the end of the last, so
                // spaces at either end of a link aren't underlined.
                let mut ink: Option<(f32, f32)> = None;
                for glyph in run.positioned_glyphs() {
                    let x = x0 + glyph.x;
                    if self.glyphs.draw(
                        &mut page.canvas,
                        font,
                        size,
                        &coords,
                        glyph.id,
                        x,
                        y0 + glyph.y,
                    ) {
                        let (start, end) = ink.get_or_insert((x, x));
                        *start = start.min(x);
                        *end = end.max(x + glyph.advance);
                    }
                }
                match (run.style().brush, ink) {
                    (Some(target), Some((start, end))) => {
                        let metrics = run.run().font_metrics();
                        // The offset is up from the baseline; the page's y
                        // runs down.
                        let y = y0 + run.baseline() - metrics.underline_offset;
                        match &mut underline {
                            Some(u) if u.area.target == target => {
                                u.area.right = end;
                                u.y = u.y.max(y);
                                u.thickness = u.thickness.max(metrics.underline_size);
                            }
                            _ => {
                                if let Some(done) = underline.take() {
                                    done.finish(page);
                                }
                                let m = line.metrics();
                                underline = Some(Underline {
                                    area: LinkArea {
                                        left: start,
                                        top: y0 + m.block_min_coord,
                                        right: end,
                                        bottom: y0 + m.block_max_coord,
                                        target,
                                    },
                                    y,
                                    thickness: metrics.underline_size,
                                });
                            }
                        }
                    }
                    // Only spaces: a link carries on across them.
                    (Some(_), None) => {}
                    (None, _) => {
                        if let Some(done) = underline.take() {
                            done.finish(page);
                        }
                    }
                }
                last_run = Some((
                    font.clone(),
                    size,
                    coords,
                    run.offset() + run.advance(),
                    run.baseline(),
                ));
            }
        }
        if self.hyphens.get(line_index) == Some(&true)
            && let Some((font, size, coords, end, baseline)) = last_run
        {
            self.glyphs.draw_char(
                &mut page.canvas,
                &font,
                size,
                &coords,
                '-',
                x0 + end,
                y0 + baseline,
            );
            // A link broken at a hyphen is underlined under it too.
            if let Some(u) = &mut underline
                && (u.area.right - (x0 + end)).abs() < 1.0
            {
                u.area.right += self.hyphen_em * size;
            }
        }
        if let Some(done) = underline.take() {
            done.finish(page);
        }
        self.record_words(page, line_index, x0, y0);
    }

    /// Records where each word of a line is, with the layout's origin at
    /// (`x0`, `y0`). A word's text runs on past the line's ends, so both
    /// halves of a word broken at a hyphen have all of it.
    fn record_words(&self, page: &mut Page, line_index: usize, x0: f32, y0: f32) {
        let Some(line) = self.layout.get(line_index) else {
            return;
        };
        let metrics = line.metrics();
        let (top, bottom) = (y0 + metrics.block_min_coord, y0 + metrics.block_max_coord);
        let text = self.text.as_str();
        // The word so far: its left and right, and where it is in the text.
        let mut word: Option<(f32, f32, std::ops::Range<usize>)> = None;
        let mut finish = |word: &mut Option<(f32, f32, std::ops::Range<usize>)>| {
            let Some((left, right, range)) = word.take() else {
                return;
            };
            let start = text[..range.start]
                .char_indices()
                .rev()
                .find(|(_, c)| between_words(*c))
                .map_or(0, |(at, c)| at + c.len_utf8());
            let end = text[range.end..]
                .find(between_words)
                .map_or(text.len(), |at| range.end + at);
            page.words.push(WordArea {
                left,
                top,
                right,
                bottom,
                text: text[start..end].to_owned(),
            });
        };
        // A run's clusters span all its glyph runs, one per style, so each
        // run is walked once, from where its first glyph run starts.
        let mut walked = None;
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                finish(&mut word);
                continue;
            };
            if walked.replace(run.run().index()) == Some(run.run().index()) {
                continue;
            }
            let mut x = x0 + run.offset();
            for cluster in run.run().clusters() {
                let range = cluster.text_range();
                let advance = cluster.advance();
                let Some(piece) = text.get(range.clone()) else {
                    continue;
                };
                let inked = piece.chars().any(|c| !between_words(c) && c != SOFT_HYPHEN);
                if inked {
                    match &mut word {
                        Some((_, right, at)) => {
                            *right = x + advance;
                            at.end = range.end;
                        }
                        None => word = Some((x, x + advance, range)),
                    }
                } else if piece.chars().any(between_words) {
                    finish(&mut word);
                }
                x += advance;
            }
        }
        finish(&mut word);
    }

    fn style(&self, block: &Block, previous: Option<&Block>) -> BlockStyle {
        let s = &self.settings;
        let em = s.font_size;
        let line = s.line();
        let step = em * 1.5;
        let mut style = BlockStyle {
            scale: 1.0,
            weight: None,
            alignment: Alignment::Start,
            inset: f32::from(block.indent) * step,
            first_line_indent: 0.0,
            space_before: 0.0,
            space_after: 0.0,
            keep_with_next: false,
            preformatted: false,
        };
        match &block.kind {
            BlockKind::Paragraph { indent_first } => {
                if *indent_first {
                    style.first_line_indent = step;
                } else if matches!(previous.map(|p| &p.kind), Some(BlockKind::Paragraph { .. }))
                    && block.align == Align::Default
                {
                    // Paragraphs that the book doesn't indent are told apart by space.
                    style.space_before = line * 0.5;
                }
                if s.justify {
                    style.alignment = Alignment::Justify;
                }
            }
            BlockKind::Heading { level } => {
                style.scale = match level {
                    1 => 1.5,
                    2 => 1.3,
                    3 => 1.15,
                    _ => 1.0,
                };
                style.weight = Some(self.settings.weight_above(200.0));
                style.space_before = line * 1.5;
                style.space_after = line * 0.5;
                style.keep_with_next = true;
            }
            BlockKind::ListItem { .. } => style.inset += step,
            BlockKind::Preformatted => {
                style.preformatted = true;
                style.scale = fonts::MONO_SCALE;
                style.space_before = line * 0.5;
                style.space_after = line * 0.5;
            }
            BlockKind::Image { .. } | BlockKind::Separator => {
                style.alignment = Alignment::Center;
                style.space_before = line * 0.5;
                style.space_after = line * 0.5;
            }
        }
        match block.align {
            Align::Default => {}
            Align::Left => style.alignment = Alignment::Start,
            Align::Center => style.alignment = Alignment::Center,
            Align::Right => style.alignment = Alignment::End,
        }
        style
    }

    /// An image's size on the page: shrunk to fit the page, never enlarged.
    fn image_size(&self, info: Option<&ImageInfo>) -> (f32, f32) {
        let Some(info) = info else {
            return (0.0, 0.0);
        };
        let (width, height) = (info.width.max(1) as f32, info.height.max(1) as f32);
        let scale = (self.settings.content_width() / width)
            .min(self.settings.content_height() / height)
            .min(1.0);
        ((width * scale).floor(), (height * scale).floor())
    }

    fn lay_out(&mut self, block: &Block, style: &BlockStyle) {
        let size = self.settings.font_size * style.scale;
        let (ranges, has_hyphens) = self.gather_text(block, style);
        let s = &self.settings;
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.fonts, &self.text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(if style.preformatted {
            mono()
        } else {
            FontFamily::named(fonts::BODY_FAMILY)
        }));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            s.line_height,
        )));
        builder.push_default(StyleProperty::Locale(self.locale));
        builder.push_default(optical_size(size, s.dpi));
        builder.push_default(StyleProperty::WhiteSpaceCollapse(if style.preformatted {
            WhiteSpaceCollapse::Preserve
        } else {
            WhiteSpaceCollapse::PreserveBreaks
        }));
        // A run with nowhere to break, such as a long URL, is broken at the
        // margin rather than drawn past it.
        builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::BreakWord));
        let weight = style.weight.unwrap_or(self.settings.weight_above(0.0));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(weight)));
        for (span, range) in block.spans.iter().zip(ranges) {
            if range.is_empty() {
                continue;
            }
            let st = &span.style;
            if st.emphasis {
                builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
            }
            if st.strong {
                builder.push(
                    StyleProperty::FontWeight(FontWeight::new(self.settings.weight_above(300.0))),
                    range.clone(),
                );
            }
            if st.code && !style.preformatted {
                builder.push(StyleProperty::FontFamily(mono()), range.clone());
                builder.push(
                    StyleProperty::FontSize(size * fonts::MONO_SCALE),
                    range.clone(),
                );
            }
            if st.small_caps {
                builder.push(
                    StyleProperty::FontFeatures(FontFeatures::Source(Cow::Borrowed("\"smcp\" 1"))),
                    range.clone(),
                );
            }
            if let Some(target) = span.link {
                builder.push(StyleProperty::Brush(Some(target)), range.clone());
            }
            if st.vertical != Vertical::Baseline {
                let shift = if st.vertical == Vertical::Super {
                    VerticalAlign::SUPER
                } else {
                    VerticalAlign::SUB
                };
                builder.push(StyleProperty::VerticalAlign(shift), range.clone());
                builder.push(StyleProperty::FontSize(size * 0.7), range.clone());
                builder.push(optical_size(size * 0.7, s.dpi), range);
            }
        }
        let code_size = size * fonts::MONO_SCALE;
        for range in &self.code_spaces {
            builder.push(
                StyleProperty::LetterSpacing(self.code_space_spacing * code_size),
                range.clone(),
            );
        }
        builder.build_into(&mut self.layout, &self.text);
        if style.preformatted {
            // A code line too long for the page carries on indented, as in an
            // editor, so it isn't mistaken for the next line.
            self.layout.set_text_indent(
                2.0 * self.code_space * size,
                IndentOptions {
                    each_line: true,
                    hanging: true,
                },
            );
        } else {
            self.layout
                .set_text_indent(style.first_line_indent, IndentOptions::default());
        }

        self.break_block(style, size, has_hyphens);
        self.layout
            .align(style.alignment, AlignmentOptions::default());
    }

    /// Gathers the block's text, without soft hyphens when hyphenation is
    /// off. Returns each span's range in it, and whether it has soft hyphens.
    fn gather_text(
        &mut self,
        block: &Block,
        style: &BlockStyle,
    ) -> (Vec<std::ops::Range<usize>>, bool) {
        let keep_hyphens = self.settings.hyphenate;
        self.text.clear();
        let mut ranges = Vec::with_capacity(block.spans.len());
        if let BlockKind::Separator = block.kind {
            self.text.push_str("*\u{2003}*\u{2003}*");
        }
        self.code_spaces.clear();
        for span in &block.spans {
            let start = self.text.len();
            let inline_code = span.style.code && !style.preformatted;
            for c in span.text.chars() {
                if c == SOFT_HYPHEN && !keep_hyphens {
                    continue;
                }
                if c == ' ' && inline_code {
                    // Justifying shouldn't spread code out.
                    let at = self.text.len();
                    self.text.push(CODE_SPACE);
                    self.code_spaces.push(at..self.text.len());
                } else {
                    self.text.push(c);
                }
            }
            ranges.push(start..self.text.len());
        }
        let has_hyphens = keep_hyphens && self.text.contains(SOFT_HYPHEN);
        (ranges, has_hyphens)
    }

    /// Breaks the built layout into lines, optimally if it can.
    fn break_block(&mut self, style: &BlockStyle, size: f32, has_hyphens: bool) {
        let width = (self.settings.content_width() - style.inset).max(size);
        let hyphen = self.hyphen_em * size;
        let optimal = self.settings.optimal_breaks
            && style.alignment == Alignment::Justify
            && !style.preformatted
            && knuth_plass::break_layout(
                &mut self.layout,
                &self.text,
                width,
                style.first_line_indent,
                hyphen,
            );
        self.optimal = optimal;
        if optimal {
            let text = &self.text;
            self.hyphens.clear();
            self.hyphens.extend(
                self.layout
                    .lines()
                    .map(|line| text[..line.text_range().end].ends_with(SOFT_HYPHEN)),
            );
        } else {
            self.break_lines(width, size, has_hyphens);
        }
    }

    /// Breaks the block into lines `width` wide one line at a time, leaving
    /// room for the hyphen on lines that end at a soft hyphen.
    fn break_lines(&mut self, width: f32, size: f32, has_hyphens: bool) {
        self.hyphens.clear();
        if has_hyphens {
            let narrow = width - self.hyphen_em * size;
            self.layout.break_all_lines(Some(narrow));
            let text = &self.text;
            self.hyphens.extend(
                self.layout
                    .lines()
                    .map(|line| text[..line.text_range().end].ends_with(SOFT_HYPHEN)),
            );
            // The same breaks again, now telling Parley which lines can use the
            // whole column.
            let mut breaker = self.layout.break_lines();
            breaker.state_mut().set_layout_max_advance(narrow);
            breaker.state_mut().set_line_max_advance(narrow);
            let mut index = 0;
            while let Some(data) = breaker.break_next() {
                match data {
                    YieldData::LineBreak(_) => {
                        if self.hyphens.get(index) != Some(&true) {
                            breaker.set_prior_line_width(width);
                        }
                        index += 1;
                    }
                    YieldData::InlineBoxBreak(_) => {
                        let x = breaker.state().line_x();
                        breaker.state_mut().append_inline_box_to_line(x, 0.0, 0.0);
                    }
                    YieldData::MaxHeightExceeded(_) => {}
                }
            }
            breaker.finish();
        } else {
            self.layout.break_all_lines(Some(width));
        }
    }

    fn line_infos(&self) -> Vec<LineInfo> {
        let mut offset = 0u32;
        let mut counted = 0usize;
        self.layout
            .lines()
            .map(|line| {
                let start = line.text_range().start;
                if start > counted {
                    offset += self.text[counted..start]
                        .chars()
                        .filter(|c| *c != SOFT_HYPHEN)
                        .count() as u32;
                    counted = start;
                }
                let metrics = line.metrics();
                LineInfo {
                    offset,
                    height: metrics.block_max_coord - metrics.block_min_coord,
                }
            })
            .collect()
    }

    /// How wide a one-line label is, for the reader's own screens such as
    /// the menu. `weight` is absolute, 400 being regular.
    pub fn label_width(&mut self, text: &str, size: f32, weight: f32) -> f32 {
        self.lay_out_aux(text, size, weight);
        self.aux.width()
    }

    /// Draws a one-line label with its baseline's left end at (`x`,
    /// `baseline`).
    pub fn draw_label(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        (x, baseline): (f32, f32),
        size: f32,
        weight: f32,
    ) {
        self.lay_out_aux(text, size, weight);
        self.draw_aux(canvas, x, baseline);
    }

    fn lay_out_aux(&mut self, text: &str, size: f32, weight: f32) {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.fonts, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::named(
            fonts::BODY_FAMILY,
        )));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::Locale(self.locale));
        builder.push_default(optical_size(size, self.settings.dpi));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(weight)));
        builder.build_into(&mut self.aux, text);
        self.aux.break_all_lines(None);
    }

    /// Draws the first line of `aux` with its baseline at `baseline`.
    fn draw_aux(&mut self, canvas: &mut Canvas, x: f32, baseline: f32) {
        let Some(line) = self.aux.get(0) else {
            return;
        };
        let y0 = baseline - line.metrics().baseline;
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                let font = run.run().font();
                let size = run.run().font_size();
                let coords: Vec<i16> = run
                    .run()
                    .normalized_coords()
                    .iter()
                    .map(|c| c.to_bits())
                    .collect();
                for glyph in run.positioned_glyphs() {
                    self.glyphs.draw(
                        canvas,
                        font,
                        size,
                        &coords,
                        glyph.id,
                        x + glyph.x,
                        y0 + glyph.y,
                    );
                }
            }
        }
    }
}

/// Intel One Mono, falling back to Literata for anything it lacks.
fn mono() -> FontFamily<'static> {
    FontFamily::List(Cow::Borrowed(&[
        parley::FontFamilyName::Named(Cow::Borrowed(fonts::MONO_FAMILY)),
        parley::FontFamilyName::Named(Cow::Borrowed(fonts::BODY_FAMILY)),
    ]))
}

/// Literata's optical size axis is in points, so small text gets sturdier
/// shapes and headings finer ones, as in print.
fn optical_size(px: f32, dpi: f32) -> StyleProperty<'static, Link> {
    let points = px / dpi / POINT;
    StyleProperty::FontVariations(FontVariations::Source(Cow::Owned(format!(
        "\"opsz\" {points:.1}"
    ))))
}

#[cfg(test)]
mod tests {
    use book_format::{Position, Span};

    use super::*;

    fn hyphenated_chapter() -> Chapter {
        let word = format!("ex{SOFT_HYPHEN}tra{SOFT_HYPHEN}or{SOFT_HYPHEN}di{SOFT_HYPHEN}nary");
        Chapter {
            blocks: vec![Block {
                kind: BlockKind::Paragraph {
                    indent_first: false,
                },
                indent: 0,
                align: Align::Default,
                spans: vec![Span::plain(format!("{word} ").repeat(60))],
            }],
        }
    }

    /// Where each line's ink ends, as drawn: the end of its last glyph run.
    fn line_ends(typesetter: &Typesetter) -> Vec<f32> {
        typesetter
            .layout
            .lines()
            .map(|line| {
                line.items()
                    .filter_map(|item| match item {
                        PositionedLayoutItem::GlyphRun(run) => Some(run.offset() + run.advance()),
                        PositionedLayoutItem::InlineBox(_) => None,
                    })
                    .fold(0.0, f32::max)
            })
            .collect()
    }

    fn lay_out(settings: Settings) -> (Typesetter, Placed) {
        let mut typesetter = Typesetter::new(settings, Some("en"));
        let chapter = hyphenated_chapter();
        let at = ChapterRef {
            index: 0,
            chapter: &chapter,
            images: &[],
        };
        let placed = typesetter.block(at, 0);
        (typesetter, placed)
    }

    #[test]
    fn hyphens_fit_inside_the_column() {
        let settings = Settings::default();
        let width = settings.content_width();
        let (typesetter, placed) = lay_out(settings);
        let size = typesetter.settings.font_size;
        let hyphen = typesetter.hyphen_em * size;
        assert!(hyphen > 5.0, "{hyphen}");
        let mut hyphenated = 0;
        for (line, ends_at_hyphen) in typesetter.layout.lines().zip(&typesetter.hyphens) {
            let metrics = line.metrics();
            let right = metrics.offset + metrics.advance - metrics.hanging_advance;
            if *ends_at_hyphen {
                hyphenated += 1;
                assert!(
                    right + hyphen <= width + 0.5,
                    "{right} + {hyphen} > {width}"
                );
            } else {
                assert!(right <= width + 0.5, "{right} > {width}");
            }
        }
        assert!(hyphenated > 3, "{hyphenated} lines end at a hyphen");
        // Offsets don't count soft hyphens. Each word is 13 letters and a
        // space, and a line starts at a word or after one of its hyphens.
        assert!(
            placed
                .lines
                .iter()
                .all(|line| [0, 2, 5, 7, 9].contains(&(line.offset % 14)))
        );
        assert!(placed.lines.iter().any(|line| line.offset % 14 != 0));
    }

    #[test]
    fn justified_lines_end_at_the_margin() {
        for optimal_breaks in [true, false] {
            let settings = Settings {
                optimal_breaks,
                ..Settings::default()
            };
            let width = settings.content_width();
            let (typesetter, _) = lay_out(settings);
            assert_eq!(typesetter.optimal, optimal_breaks);
            let hyphen = typesetter.hyphen_em * typesetter.settings.font_size;
            let ends = line_ends(&typesetter);
            // Every line but the last is justified, ending exactly at the
            // margin, or a hyphen short of it.
            for (end, hyphenated) in ends[..ends.len() - 1].iter().zip(&typesetter.hyphens) {
                let target = if *hyphenated { width - hyphen } else { width };
                assert!(
                    (end - target).abs() < 0.5,
                    "optimal {optimal_breaks}: a line ends at {end}, not {target}"
                );
            }
            assert!(ends.last().unwrap() < &width);
        }
    }

    #[test]
    fn optimal_breaks_are_less_loose() {
        // Ordinary prose, where filling each line leaves some loose ones.
        let text = "It was a bright cold day in April, and the clocks were striking \
            thirteen. Winston Smith, his chin nuzzled into his breast in an effort \
            to escape the vile wind, slipped quickly through the glass doors of \
            Victory Mansions, though not quickly enough to prevent a swirl of \
            gritty dust from entering along with him. The hallway smelt of boiled \
            cabbage and old rag mats. At one end of it a coloured poster, too \
            large for indoor display, had been tacked to the wall.";
        let loosest = |optimal_breaks| {
            let settings = Settings {
                optimal_breaks,
                hyphenate: false,
                ..Settings::default()
            };
            let mut typesetter = Typesetter::new(settings, Some("en"));
            let chapter = Chapter {
                blocks: vec![Block {
                    kind: BlockKind::Paragraph {
                        indent_first: false,
                    },
                    indent: 0,
                    align: Align::Default,
                    spans: vec![Span::plain(text)],
                }],
            };
            let at = ChapterRef {
                index: 0,
                chapter: &chapter,
                images: &[],
            };
            typesetter.block(at, 0);
            assert_eq!(typesetter.optimal, optimal_breaks);
            // The widest gap Parley added to any line but the last.
            let lines: Vec<_> = typesetter.layout.lines().collect();
            lines[..lines.len() - 1]
                .iter()
                .map(|line| {
                    let m = line.metrics();
                    let spaces = text[line.text_range()].trim_end().matches(' ').count();
                    (m.inline_max_coord - m.inline_min_coord - m.advance + m.hanging_advance)
                        / spaces.max(1) as f32
                })
                .fold(0.0, f32::max)
        };
        let (optimal, greedy) = (loosest(true), loosest(false));
        assert!(optimal < greedy, "optimal {optimal} px, greedy {greedy} px");
    }

    #[test]
    fn hyphenation_can_be_turned_off() {
        let (typesetter, placed) = lay_out(Settings {
            hyphenate: false,
            ..Settings::default()
        });
        assert!(typesetter.hyphens.iter().all(|h| !h));
        assert!(placed.lines.iter().all(|line| line.offset % 14 == 0));
    }

    fn lay_out_block(block: Block) -> Typesetter {
        let mut typesetter = Typesetter::new(Settings::default(), Some("en"));
        let chapter = Chapter {
            blocks: vec![block],
        };
        let at = ChapterRef {
            index: 0,
            chapter: &chapter,
            images: &[],
        };
        typesetter.block(at, 0);
        typesetter
    }

    fn code(text: &str) -> Span {
        let mut span = Span::plain(text);
        span.style.code = true;
        span
    }

    /// Each glyph run's font, and the gaps between its glyphs' origins.
    fn runs(typesetter: &Typesetter) -> Vec<(bool, Vec<f32>)> {
        let mut runs = Vec::new();
        for line in typesetter.layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    let mono = fonts::is_mono(run.run().font().data.as_ref());
                    let xs: Vec<f32> = run.positioned_glyphs().map(|g| g.x).collect();
                    runs.push((mono, xs.windows(2).map(|w| w[1] - w[0]).collect()));
                }
            }
        }
        runs
    }

    #[test]
    fn justified_inline_code_keeps_its_spacing() {
        let mut spans = Vec::new();
        for _ in 0..12 {
            spans.push(Span::plain("Short words, then "));
            spans.push(code("let s = String::new();"));
            spans.push(Span::plain(" and on. "));
        }
        let typesetter = lay_out_block(Block {
            kind: BlockKind::Paragraph {
                indent_first: false,
            },
            indent: 0,
            align: Align::Default,
            spans,
        });
        let size = Settings::default().font_size * fonts::MONO_SCALE;
        let cell = typesetter.code_space * size;
        let mut checked = 0;
        for (mono, gaps) in runs(&typesetter) {
            if !mono {
                continue;
            }
            for gap in gaps {
                assert!(
                    (gap - cell).abs() < 0.05,
                    "a {gap} px step in a {cell} px grid"
                );
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} code glyphs");
    }

    #[test]
    fn preformatted_text_is_all_mono() {
        let typesetter = lay_out_block(Block {
            kind: BlockKind::Preformatted,
            indent: 0,
            align: Align::Default,
            spans: vec![Span::plain("fn main() {\n    println!(\"hello\");\n}")],
        });
        let runs = runs(&typesetter);
        assert!(runs.len() >= 3, "{runs:?}");
        assert!(runs.iter().all(|(mono, _)| *mono));
    }

    #[test]
    fn wrapped_code_lines_hang() {
        let long = "// ".to_owned() + &"word ".repeat(40);
        let typesetter = lay_out_block(Block {
            kind: BlockKind::Preformatted,
            indent: 0,
            align: Align::Default,
            spans: vec![Span::plain(format!("short\n{long}\nshort"))],
        });
        let starts: Vec<f32> = typesetter
            .layout
            .lines()
            // The offset includes the indent.
            .map(|line| line.metrics().offset)
            .collect();
        assert!(starts.len() >= 4, "{starts:?}");
        let size = Settings::default().font_size * fonts::MONO_SCALE;
        let hang = 2.0 * typesetter.code_space * size;
        let last = starts.len() - 1;
        for (i, start) in starts.iter().enumerate() {
            let expected = if i == 0 || i == 1 || i == last {
                0.0
            } else {
                hang
            };
            assert!(
                (start - expected).abs() < 0.01,
                "line {i} at {start}: {starts:?}"
            );
        }
    }

    #[test]
    fn links_are_underlined_where_they_are_drawn() {
        let target = Position {
            chapter: 2,
            block: 5,
            offset: 0,
        };
        let mut link =
            Span::plain("a linked phrase that runs on long enough to wrap onto the next line ");
        link.link = Some(target);
        let chapter = Chapter {
            blocks: vec![Block {
                kind: BlockKind::Paragraph {
                    indent_first: false,
                },
                indent: 0,
                align: Align::Default,
                spans: vec![
                    Span::plain("Some words before "),
                    link,
                    Span::plain("and some words after it, which aren't."),
                ],
            }],
        };
        let at = ChapterRef {
            index: 0,
            chapter: &chapter,
            images: &[],
        };
        let settings = Settings::default();
        let mut typesetter = Typesetter::new(settings.clone(), Some("en"));
        let placed = typesetter.block(at, 0);
        let mut page = Page::new(settings.width, settings.height);
        typesetter.draw_lines(&mut page, at, 0, 0..placed.lines.len(), 100.0, &mut |_| {
            None
        });
        let Page { canvas, links, .. } = page;
        assert_eq!(links.len(), 2, "one area per line: {links:?}");
        assert!(links.iter().all(|l| l.target == target));
        assert!(links[0].bottom <= links[1].top + 0.5);
        for area in &links {
            // Some row inside the area is mostly dark: the underline, with
            // gaps at descenders. Text alone never is.
            let (left, right) = (
                area.left.ceil() as usize + 1,
                area.right.floor() as usize - 1,
            );
            let width = settings.width as usize;
            let underlined = (area.top as usize..area.bottom as usize).any(|y| {
                let row = &canvas.pixels()[y * width + left..y * width + right];
                row.iter().filter(|p| **p < 128).count() * 10 > row.len() * 8
            });
            assert!(underlined, "no underline in {area:?}");
            assert_eq!(
                area.distance(f32::midpoint(area.left, area.right), area.top + 1.0),
                0.0
            );
        }
        // The first line is justified to the margin; the words after the
        // link carry on the second line past its end, not underlined.
        let margin = (settings.width - settings.margin_side) as f32;
        assert!((links[0].right - margin).abs() < 1.0, "{links:?}");
        assert!(links[1].right < margin - 40.0, "{links:?}");
    }

    #[test]
    fn words_are_found_where_they_are_drawn() {
        let settings = Settings::default();
        let mut typesetter = Typesetter::new(settings.clone(), Some("en"));
        let chapter = hyphenated_chapter();
        let at = ChapterRef {
            index: 0,
            chapter: &chapter,
            images: &[],
        };
        let placed = typesetter.block(at, 0);
        let mut page = Page::new(settings.width, settings.height);
        typesetter.draw_lines(&mut page, at, 0, 0..placed.lines.len(), 100.0, &mut |_| {
            None
        });
        let word = format!("ex{SOFT_HYPHEN}tra{SOFT_HYPHEN}or{SOFT_HYPHEN}di{SOFT_HYPHEN}nary");
        // Every piece, whole or broken at a hyphen, reads as the whole word.
        assert!(
            page.words.iter().all(|w| w.text == word),
            "{:?}",
            page.words
        );
        let broken = typesetter.hyphens.iter().filter(|h| **h).count();
        assert!(broken > 3);
        assert_eq!(page.words.len(), 60 + broken);
        // Left to right along a line, without overlapping, inside the
        // column; lines one under the other.
        let (left, right) = (
            settings.margin_side as f32,
            (settings.width - settings.margin_side) as f32,
        );
        for pair in page.words.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(a.left >= left - 0.5 && a.right <= right + 0.5, "{a:?}");
            assert!(a.right > a.left + 10.0, "{a:?}");
            if (a.top - b.top).abs() < 0.5 {
                assert!(a.right + 5.0 < b.left, "a space between {a:?} and {b:?}");
            } else {
                assert!(a.bottom <= b.top + 0.5, "{a:?} {b:?}");
            }
        }
        let first = &page.words[0];
        assert_eq!(first.distance(first.left + 1.0, first.top + 1.0), 0.0);
        assert!(first.distance(first.left - 10.0, first.top) > 9.0);
    }

    #[test]
    fn dashes_and_slashes_come_between_words() {
        let settings = Settings::default();
        let mut typesetter = Typesetter::new(settings.clone(), Some("en"));
        let chapter = Chapter {
            blocks: vec![Block {
                kind: BlockKind::Paragraph {
                    indent_first: false,
                },
                indent: 0,
                align: Align::Default,
                spans: vec![
                    Span::plain("\u{201c}Well\u{2014}the dog\u{2019}s "),
                    Span {
                        link: Some(Position {
                            chapter: 1,
                            block: 0,
                            offset: 0,
                        }),
                        ..Span::plain("well-known")
                    },
                    Span::plain(" and/or bone,\u{201d} she said."),
                ],
            }],
        };
        let at = ChapterRef {
            index: 0,
            chapter: &chapter,
            images: &[],
        };
        let placed = typesetter.block(at, 0);
        let mut page = Page::new(settings.width, settings.height);
        typesetter.draw_lines(&mut page, at, 0, 0..placed.lines.len(), 100.0, &mut |_| {
            None
        });
        let words: Vec<&str> = page.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            words,
            [
                "\u{201c}Well",
                "the",
                "dog\u{2019}s",
                "well-known",
                "and",
                "or",
                "bone,\u{201d}",
                "she",
                "said."
            ]
        );
        // Where the link's ink is, so is its word.
        let (link, word) = (&page.links[0], &page.words[3]);
        assert!((link.left - word.left).abs() < 2.0, "{link:?} {word:?}");
        assert!((link.right - word.right).abs() < 2.0, "{link:?} {word:?}");
    }

    #[test]
    fn heavier_text_has_more_ink() {
        let ink = |weight| {
            let settings = Settings {
                weight,
                ..Settings::default()
            };
            let mut typesetter = Typesetter::new(settings.clone(), Some("en"));
            let chapter = hyphenated_chapter();
            let at = ChapterRef {
                index: 0,
                chapter: &chapter,
                images: &[],
            };
            let placed = typesetter.block(at, 0);
            let mut page = Page::new(settings.width, settings.height);
            typesetter.draw_lines(&mut page, at, 0, 0..4, 100.0, &mut |_| None);
            assert!(placed.lines.len() > 4);
            page.canvas
                .pixels()
                .iter()
                .map(|p| u64::from(255 - p))
                .sum::<u64>()
        };
        let (regular, heavier) = (ink(400.0), ink(500.0));
        assert!(heavier > regular * 11 / 10, "{regular} → {heavier}");
    }
}
