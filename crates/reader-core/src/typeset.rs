//! Lays out one block at a time with Parley and draws the lines it made.
//!
//! Parley breaks lines at soft hyphens but doesn't draw a hyphen there, so
//! this does. A block with soft hyphens is broken one hyphen narrower than
//! the column. Lines that end at a hyphen are justified to that narrower
//! width, and the "-" drawn after them ends at the margin. The other lines
//! are justified to the full column.

use std::borrow::Cow;

use book_format::{Align, Block, BlockKind, Chapter, ImageInfo, SOFT_HYPHEN, Vertical};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFeatures, FontStyle, FontVariations,
    FontWeight, IndentOptions, Layout, LayoutContext, LineHeight, OverflowWrap,
    PositionedLayoutItem, StyleProperty, VerticalAlign, WhiteSpaceCollapse, YieldData,
    fontique::Language,
};
use skrifa::{FontRef, MetadataProvider, instance::Size};

use crate::{Canvas, Settings, fonts, knuth_plass, render::Glyphs};

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
    layout_cx: LayoutContext<()>,
    layout: Layout<()>,
    /// The list marker or separator being drawn beside a block.
    aux: Layout<()>,
    locale: Option<Language>,
    text: String,
    /// For each line in `layout`, whether it ends at a soft hyphen.
    hyphens: Vec<bool>,
    current: Option<Current>,
    /// Whether the block in `layout` was broken with Knuth–Plass.
    optimal: bool,
    hyphen_em: f32,
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
        let hyphen_em = hyphen_em(&mut fonts);
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
            glyphs: Glyphs::default(),
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
        canvas: &mut Canvas,
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
                crate::render::image(canvas, &gray, x, top, width, height);
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
            let Some(line) = self.layout.get(line_index) else {
                break;
            };
            let mut last_run = None;
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
                            x0 + glyph.x,
                            y0 + glyph.y,
                        );
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
                self.glyphs
                    .draw_char(canvas, &font, size, &coords, '-', x0 + end, y0 + baseline);
            }
        }

        // A list marker sits in the gutter before the first line.
        if lines.start == 0
            && let BlockKind::ListItem { marker } = &block.kind
            && let Some(line) = self.layout.get(0)
        {
            let baseline = y0 + line.metrics().baseline;
            let size = self.settings.font_size * style.scale;
            let gap = self.settings.font_size * 0.5;
            self.lay_out_aux(marker, size);
            let x = x0 - gap - self.aux.width();
            self.draw_aux(canvas, x, baseline);
        }
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
                style.weight = Some(600.0);
                style.space_before = line * 1.5;
                style.space_after = line * 0.5;
                style.keep_with_next = true;
            }
            BlockKind::ListItem { .. } => style.inset += step,
            BlockKind::Preformatted => {
                style.preformatted = true;
                style.scale = 0.9;
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
        let s = &self.settings;
        let size = s.font_size * style.scale;
        let keep_hyphens = s.hyphenate;

        self.text.clear();
        let mut ranges = Vec::with_capacity(block.spans.len());
        if let BlockKind::Separator = block.kind {
            self.text.push_str("*\u{2003}*\u{2003}*");
        }
        for span in &block.spans {
            let start = self.text.len();
            if keep_hyphens {
                self.text.push_str(&span.text);
            } else {
                self.text
                    .extend(span.text.chars().filter(|c| *c != SOFT_HYPHEN));
            }
            ranges.push(start..self.text.len());
        }
        let has_hyphens = keep_hyphens && self.text.contains(SOFT_HYPHEN);

        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.fonts, &self.text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::named(
            fonts::BODY_FAMILY,
        )));
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
        // A run with nowhere to break, such as code spaced with no-break
        // spaces, is broken at the margin rather than drawn past it.
        builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::BreakWord));
        if let Some(weight) = style.weight {
            builder.push_default(StyleProperty::FontWeight(FontWeight::new(weight)));
        }
        for (span, range) in block.spans.iter().zip(ranges) {
            if range.is_empty() {
                continue;
            }
            let st = &span.style;
            if st.emphasis {
                builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
            }
            if st.strong {
                builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
            }
            if st.small_caps {
                builder.push(
                    StyleProperty::FontFeatures(FontFeatures::Source(Cow::Borrowed("\"smcp\" 1"))),
                    range.clone(),
                );
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
        builder.build_into(&mut self.layout, &self.text);
        self.layout
            .set_text_indent(style.first_line_indent, IndentOptions::default());

        let width = (s.content_width() - style.inset).max(size);
        let hyphen = self.hyphen_em * size;
        let optimal = s.optimal_breaks
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
        self.layout
            .align(style.alignment, AlignmentOptions::default());
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

    fn lay_out_aux(&mut self, text: &str, size: f32) {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.fonts, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::named(
            fonts::BODY_FAMILY,
        )));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::Locale(self.locale));
        builder.push_default(optical_size(size, self.settings.dpi));
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

/// Literata's optical size axis is in points, so small text gets sturdier
/// shapes and headings finer ones, as in print.
fn optical_size(px: f32, dpi: f32) -> StyleProperty<'static, ()> {
    let points = px / dpi / POINT;
    StyleProperty::FontVariations(FontVariations::Source(Cow::Owned(format!(
        "\"opsz\" {points:.1}"
    ))))
}

/// The hyphen's advance in the regular font, per pixel of font size.
fn hyphen_em(fonts: &mut FontContext) -> f32 {
    let Some(family) = fonts.collection.family_by_name(fonts::BODY_FAMILY) else {
        return 0.33;
    };
    let Some(font) = family.fonts().first() else {
        return 0.33;
    };
    let Some(blob) = font.load(Some(&mut fonts.source_cache)) else {
        return 0.33;
    };
    FontRef::from_index(blob.as_ref(), font.index())
        .ok()
        .and_then(|font| {
            let id = font.charmap().map('-')?;
            font.glyph_metrics(Size::new(1.0), skrifa::instance::LocationRef::default())
                .advance_width(id)
        })
        .unwrap_or(0.33)
}

#[cfg(test)]
mod tests {
    use book_format::Span;

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
}
