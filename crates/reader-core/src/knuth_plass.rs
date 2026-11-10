//! Knuth and Plass's line breaking, which picks a paragraph's breaks
//! together so its lines are evenly spaced, rather than filling each line in
//! turn and leaving the loose ones where they fall.
//!
//! A paragraph is a list of [`Item`]s: boxes (unbreakable text), glue (a
//! space, which may stretch, and where a line may end) and penalties (a
//! possible break, such as a soft hyphen, with a cost). The best set of breaks
//! has the fewest total demerits, which grow with how loose each line is.
//!
//! Glue only stretches here. Parley justifies by widening spaces and never
//! narrows them, so a break that needed shrinking would run into the margin.

use book_format::SOFT_HYPHEN;
use parley::Layout;

use crate::typeset::Link;

/// A box, glue or penalty. Widths are in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Item {
    Box {
        width: f32,
    },
    Glue {
        width: f32,
        stretch: f32,
    },
    Penalty {
        /// Added to the line when it breaks here, such as the hyphen's width.
        width: f32,
        cost: f32,
        /// Breaking at two flagged penalties in a row costs more, so hyphens
        /// don't stack down the margin.
        flagged: bool,
    },
}

/// A penalty with this cost forces a break.
pub(crate) const FORCED: f32 = -10_000.0;
/// The finishing glue's stretch, which lets the last line be any length.
pub(crate) const FILL: f32 = 1.0e6;

/// The stretch ratio of a line with nothing to stretch, and the most any
/// line is scored for.
pub(crate) const LOOSEST: f32 = 100.0;

const LINE_PENALTY: f32 = 10.0;
const DOUBLE_HYPHEN_DEMERITS: f32 = 3000.0;
const ADJACENT_DEMERITS: f32 = 3000.0;

#[derive(Debug, Clone, Copy)]
struct Node {
    /// The item broken at, or `usize::MAX` for the paragraph's start.
    at: usize,
    /// The first item of the line after this break.
    after: usize,
    line: usize,
    fitness: u8,
    flagged: bool,
    demerits: f32,
    previous: Option<usize>,
}

/// Returns the items to break at, ending with the last item, or `None` if
/// no set of breaks keeps every line within `tolerance`: the most a line's
/// spaces may stretch, as a multiple of their stretch. `width(n)` is the
/// width of line `n`. The items must end with a forced break.
pub(crate) fn breaks(
    items: &[Item],
    width: impl Fn(usize) -> f32,
    tolerance: f32,
) -> Option<Vec<usize>> {
    let (total_width, total_stretch) = running_totals(items);

    let mut nodes = vec![Node {
        at: usize::MAX,
        after: 0,
        line: 0,
        fitness: 1,
        flagged: false,
        demerits: 0.0,
        previous: None,
    }];
    let mut active: Vec<usize> = vec![0];

    for (index, item) in items.iter().enumerate() {
        let (break_width, cost, flagged) = match *item {
            // A line may end at a space after a word, but not at the
            // finishing glue, which belongs to the last line.
            Item::Glue { stretch, .. }
                if stretch < FILL && index > 0 && matches!(items[index - 1], Item::Box { .. }) =>
            {
                (0.0, 0.0, false)
            }
            Item::Penalty {
                width,
                cost,
                flagged,
            } if cost < -FORCED => (width, cost, flagged),
            _ => continue,
        };
        let forced = cost <= FORCED;

        // The best new break here for each fitness class.
        let mut best: [Option<Node>; 4] = [None; 4];
        active.retain(|&a| {
            let from = &nodes[a];
            let natural = total_width[index] - total_width[from.after] + break_width;
            let stretch = total_stretch[index] - total_stretch[from.after];
            let available = width(from.line);
            if natural > available + 0.01 {
                // Overfull, and any later break from here would be too.
                return false;
            }
            let ratio = if natural >= available - 0.01 {
                0.0
            } else if stretch > 0.0 {
                (available - natural) / stretch
            } else {
                // Nothing to stretch, such as one long word: as loose as a
                // line can be, but allowed when nothing better fits.
                LOOSEST
            };
            if ratio > tolerance && !forced {
                return true;
            }
            let (demerits, fitness) = demerits(ratio, cost, flagged, from);
            let total = from.demerits + demerits;
            let slot = &mut best[usize::from(fitness)];
            if slot.is_none_or(|node| total < node.demerits) {
                *slot = Some(Node {
                    at: index,
                    after: first_box_after(items, index),
                    line: from.line + 1,
                    fitness,
                    flagged,
                    demerits: total,
                    previous: Some(a),
                });
            }
            // After a forced break nothing can carry on from before it.
            !forced
        });
        for node in best.into_iter().flatten() {
            nodes.push(node);
            active.push(nodes.len() - 1);
        }
        if active.is_empty() {
            return None;
        }
    }

    let last = items.len() - 1;
    let best = *active
        .iter()
        .filter(|&&a| nodes[a].at == last)
        .min_by(|&&a, &&b| nodes[a].demerits.total_cmp(&nodes[b].demerits))?;
    let mut found = Vec::new();
    let mut next = Some(best);
    while let Some(node) = next.map(|n| nodes[n])
        && node.at != usize::MAX
    {
        found.push(node.at);
        next = node.previous;
    }
    found.reverse();
    Some(found)
}

/// The width and stretch of all the items before each one, and of them all
/// at the end, so a line's are a subtraction.
fn running_totals(items: &[Item]) -> (Vec<f32>, Vec<f32>) {
    let mut widths = Vec::with_capacity(items.len() + 1);
    let mut stretches = Vec::with_capacity(items.len() + 1);
    let (mut width, mut stretch) = (0.0f32, 0.0f32);
    for item in items {
        widths.push(width);
        stretches.push(stretch);
        match *item {
            Item::Box { width: w } => width += w,
            Item::Glue {
                width: w,
                stretch: s,
            } => {
                width += w;
                stretch += s;
            }
            Item::Penalty { .. } => {}
        }
    }
    widths.push(width);
    stretches.push(stretch);
    (widths, stretches)
}

/// The demerits of a line that stretches by `ratio` and ends at a break
/// with `cost`, following the line that ended at `from`; and its fitness
/// class, from tight (2) to very loose (0).
fn demerits(ratio: f32, cost: f32, flagged: bool, from: &Node) -> (f32, u8) {
    let ratio = ratio.min(LOOSEST);
    let badness = 100.0 * ratio.powi(3);
    let mut demerits = (LINE_PENALTY + badness).powi(2);
    if cost >= 0.0 {
        demerits += cost * cost;
    } else if cost > FORCED {
        demerits -= cost * cost;
    }
    if flagged && from.flagged {
        demerits += DOUBLE_HYPHEN_DEMERITS;
    }
    let fitness: u8 = match ratio {
        r if r > 1.0 => 0,
        r if r > 0.5 => 1,
        _ => 2,
    };
    if fitness.abs_diff(from.fitness) > 1 {
        demerits += ADJACENT_DEMERITS;
    }
    (demerits, fitness)
}

/// A line starts at the first box after a break: the glue and penalties
/// there vanish into the break.
fn first_box_after(items: &[Item], index: usize) -> usize {
    let mut at = index + 1;
    while at < items.len() && !matches!(items[at], Item::Box { .. }) {
        at += 1;
    }
    at
}

/// How a cluster of the text takes part in breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Space,
    /// A space that doesn't stretch, as inside inline code.
    FixedSpace,
    SoftHyphen,
    Newline,
    /// A hyphen or dash, after which a word may break.
    Dash,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct Cluster {
    /// Where the cluster ends in the text, in bytes.
    end: usize,
    chars: u32,
    advance: f32,
    kind: Kind,
    /// Part of a ligature that started in an earlier cluster, so a line
    /// can't break just before it.
    continues: bool,
}

/// How loose lines may get, tried in turn: the first pass that finds breaks
/// wins. The last allows anything, so only a word wider than the column
/// makes it give up.
const TOLERANCES: [f32; 3] = [1.5, 3.0, LOOSEST];

/// The cost of breaking at a hyphen, as in TeX.
const HYPHEN_COST: f32 = 50.0;

/// Breaks a laid-out paragraph with Knuth–Plass and justifies each line to
/// `width`, or one `hyphen` narrower when it ends at a soft hyphen. The first
/// line is `indent` narrower. Returns false, with the layout's lines in no
/// particular state, if it can't: right-to-left text, a word wider than the
/// column, or Parley not breaking where it was asked to.
pub(crate) fn break_layout(
    layout: &mut Layout<Link>,
    text: &str,
    width: f32,
    indent: f32,
    hyphen: f32,
) -> bool {
    // One line per hard break gives every cluster's natural advance.
    layout.break_all_lines(None);
    let mut clusters = Vec::new();
    for line in layout.lines() {
        for run in line.runs() {
            if run.is_rtl() {
                return false;
            }
            for cluster in run.clusters() {
                let range = cluster.text_range();
                let Some(first) = text[range.clone()].chars().next() else {
                    continue;
                };
                let kind = match first {
                    ' ' => Kind::Space,
                    crate::typeset::CODE_SPACE => Kind::FixedSpace,
                    SOFT_HYPHEN => Kind::SoftHyphen,
                    '\n' => Kind::Newline,
                    '-' | '\u{2010}' | '\u{2013}' | '\u{2014}' => Kind::Dash,
                    _ => Kind::Other,
                };
                clusters.push(Cluster {
                    end: range.end,
                    chars: text[range].chars().count() as u32,
                    advance: cluster.advance(),
                    kind,
                    continues: cluster.is_ligature_continuation(),
                });
            }
        }
    }
    if clusters.is_empty() {
        return false;
    }

    let (items, ends) = items(&clusters, hyphen);
    let Some(found) = TOLERANCES.iter().find_map(|&tolerance| {
        breaks(
            &items,
            |line| if line == 0 { width - indent } else { width },
            tolerance,
        )
    }) else {
        return false;
    };

    let mut breaker = layout.break_lines();
    breaker.state_mut().set_layout_max_advance(width);
    breaker.state_mut().set_line_max_advance(width);
    let mut start = 0;
    let mut expected = Vec::with_capacity(found.len());
    for &at in &found {
        let end = ends[at];
        let chars = clusters[start..end].iter().map(|c| c.chars).sum();
        if end > start && breaker.break_next_with_length(chars).is_none() {
            return false;
        }
        let hyphenated = end > 0 && clusters[end - 1].kind == Kind::SoftHyphen;
        breaker.set_prior_line_width(if hyphenated { width - hyphen } else { width });
        if end > start {
            expected.push(clusters[end - 1].end);
        }
        start = end;
    }
    if !breaker.is_done() {
        return false;
    }
    breaker.finish();
    // Parley breaks by characters, which only lands where it should if it
    // saw the text as these clusters did.
    layout.len() == expected.len()
        && layout
            .lines()
            .zip(&expected)
            .all(|(line, end)| line.text_range().end == *end)
}

/// The boxes, glue and penalties for a paragraph's clusters, and for each
/// item, how many clusters a line ending there takes in.
fn items(clusters: &[Cluster], hyphen: f32) -> (Vec<Item>, Vec<usize>) {
    const FINISH: [Item; 2] = [
        Item::Glue {
            width: 0.0,
            stretch: FILL,
        },
        Item::Penalty {
            width: 0.0,
            cost: FORCED,
            flagged: false,
        },
    ];
    let hyphen_break = |width| Item::Penalty {
        width,
        cost: HYPHEN_COST,
        flagged: true,
    };
    let mut out: Vec<(Item, usize)> = Vec::new();
    let mut word: Option<f32> = None;
    for (index, cluster) in clusters.iter().enumerate() {
        // Literata joins "ff" or "ft" across a soft hyphen into one glyph,
        // and a line can't end inside a glyph.
        let joined = clusters.get(index + 1).is_some_and(|next| next.continues);
        let kind = match cluster.kind {
            Kind::SoftHyphen | Kind::Dash if joined => Kind::Other,
            kind => kind,
        };
        if !matches!(kind, Kind::Other | Kind::Dash)
            && let Some(width) = word.take()
        {
            out.push((Item::Box { width }, index));
        }
        match kind {
            Kind::Space => out.push((
                Item::Glue {
                    width: cluster.advance,
                    stretch: cluster.advance,
                },
                index + 1,
            )),
            Kind::FixedSpace => out.push((
                Item::Glue {
                    width: cluster.advance,
                    stretch: 0.0,
                },
                index + 1,
            )),
            Kind::SoftHyphen => out.push((hyphen_break(hyphen), index + 1)),
            Kind::Newline => {
                out.push((FINISH[0], index));
                out.push((FINISH[1], index + 1));
            }
            Kind::Dash => {
                let before = word.take();
                let width = before.unwrap_or(0.0) + cluster.advance;
                let next_is_letter = clusters
                    .get(index + 1)
                    .is_some_and(|next| next.kind == Kind::Other);
                if before.is_some() && next_is_letter {
                    // "well-known" may break after the hyphen.
                    out.push((Item::Box { width }, index + 1));
                    out.push((hyphen_break(0.0), index + 1));
                } else {
                    word = Some(width);
                }
            }
            Kind::Other => word = Some(word.unwrap_or(0.0) + cluster.advance),
        }
    }
    if let Some(width) = word {
        out.push((Item::Box { width }, clusters.len()));
    }
    if !matches!(out.last(), Some((Item::Penalty { cost, .. }, _)) if *cost <= FORCED) {
        out.extend(FINISH.map(|item| (item, clusters.len())));
    }
    out.into_iter().unzip()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: Item = Item::Glue {
        width: 1.0,
        stretch: 1.0,
    };

    /// Words of the given widths between stretchy spaces, then the finishing
    /// glue and forced break.
    fn words(widths: &[f32]) -> Vec<Item> {
        let mut items = Vec::new();
        for (i, width) in widths.iter().enumerate() {
            if i > 0 {
                items.push(SPACE);
            }
            items.push(Item::Box { width: *width });
        }
        finish(&mut items);
        items
    }

    fn finish(items: &mut Vec<Item>) {
        items.push(Item::Glue {
            width: 0.0,
            stretch: FILL,
        });
        items.push(Item::Penalty {
            width: 0.0,
            cost: FORCED,
            flagged: false,
        });
    }

    /// How many boxes each line has.
    fn line_lengths(items: &[Item], breaks: &[usize]) -> Vec<usize> {
        let mut start = 0;
        breaks
            .iter()
            .map(|&end| {
                let boxes = items[start..end]
                    .iter()
                    .filter(|item| matches!(item, Item::Box { .. }))
                    .count();
                start = end;
                boxes
            })
            .collect()
    }

    /// Breaks by filling each line as full as it goes.
    fn greedy(items: &[Item], width: f32) -> Vec<usize> {
        let mut found = Vec::new();
        let mut line = 0.0;
        let mut last_glue = None;
        for (index, item) in items.iter().enumerate() {
            match *item {
                Item::Box { width: w } => {
                    if line + w > width
                        && let Some((glue, after)) = last_glue.take()
                    {
                        found.push(glue);
                        line -= after;
                    }
                    line += w;
                }
                Item::Glue { width: w, .. } => {
                    last_glue = Some((index, line + w));
                    line += w;
                }
                Item::Penalty { .. } => {}
            }
        }
        found.push(items.len() - 1);
        found
    }

    /// Scores a set of breaks the way [`breaks`] does.
    fn score(items: &[Item], found: &[usize], width: f32) -> f32 {
        let mut from = Node {
            at: usize::MAX,
            after: 0,
            line: 0,
            fitness: 1,
            flagged: false,
            demerits: 0.0,
            previous: None,
        };
        let mut total = 0.0;
        for &at in found {
            let (mut natural, mut stretch) = (0.0, 0.0);
            for item in &items[from.after..at] {
                match *item {
                    Item::Box { width } => natural += width,
                    Item::Glue { width, stretch: s } => {
                        natural += width;
                        stretch += s;
                    }
                    Item::Penalty { .. } => {}
                }
            }
            let ratio = if stretch > 0.0 {
                (width - natural) / stretch
            } else {
                0.0
            };
            let (cost, flagged) = match items[at] {
                Item::Penalty { cost, flagged, .. } => (cost, flagged),
                _ => (0.0, false),
            };
            let (demerits, fitness) = demerits(ratio, cost, flagged, &from);
            total += demerits;
            from = Node {
                at,
                after: first_box_after(items, at),
                fitness,
                flagged,
                ..from
            };
        }
        total
    }

    #[test]
    fn never_worse_than_filling_each_line() {
        // Paragraphs of pseudo-random word widths, like 3–10 letter words in
        // a column about 45 characters wide.
        let mut seed = 7u32;
        let mut better = 0;
        for _ in 0..300 {
            let widths: Vec<f32> = (0..60)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    3.0 + ((seed >> 16) % 8) as f32
                })
                .collect();
            let items = words(&widths);
            let optimal = breaks(&items, |_| 45.0, 100.0).unwrap();
            let filled = greedy(&items, 45.0);
            let (optimal_score, filled_score) =
                (score(&items, &optimal, 45.0), score(&items, &filled, 45.0));
            assert!(
                optimal_score <= filled_score + 0.5,
                "{optimal_score} > {filled_score}"
            );
            better += usize::from(optimal_score + 0.5 < filled_score);
            assert_eq!(*optimal.last().unwrap(), items.len() - 1);
        }
        // And it often finds better breaks than filling does.
        assert!(better > 100, "better on only {better} of 300");
    }

    #[test]
    fn evens_out_two_loose_lines() {
        // Width 10. Filling gives "3 2" (ratio 4) | "4 2" (ratio 3) | "5 3".
        let items = words(&[3.0, 2.0, 4.0, 2.0, 5.0, 3.0]);
        let filled = greedy(&items, 10.0);
        assert_eq!(line_lengths(&items, &filled), [2, 2, 2]);
        let found = breaks(&items, |_| 10.0, 100.0).unwrap();
        assert!(score(&items, &found, 10.0) <= score(&items, &filled, 10.0));
    }

    #[test]
    fn gives_up_when_a_word_is_wider_than_the_line() {
        let items = words(&[5.0, 30.0, 5.0]);
        assert_eq!(breaks(&items, |_| 10.0, 10.0), None);
    }

    #[test]
    fn gives_up_when_every_choice_is_too_loose() {
        // Two 4-wide words at width 20: the first line would need its one
        // space stretched by 11.
        let items = words(&[4.0, 4.0, 15.0]);
        assert_eq!(breaks(&items, |_| 20.0, 3.0), None);
        assert!(breaks(&items, |_| 20.0, 100.0).is_some());
    }

    #[test]
    fn a_hyphen_counts_toward_the_line() {
        // "aa b ccc-" | "ddd eeee": with the hyphen (width 1) the first line
        // is exactly full.
        let mut items = vec![
            Item::Box { width: 2.0 },
            SPACE,
            Item::Box { width: 1.0 },
            SPACE,
            Item::Box { width: 3.0 },
            Item::Penalty {
                width: 1.0,
                cost: 50.0,
                flagged: true,
            },
            Item::Box { width: 3.0 },
            SPACE,
            Item::Box { width: 4.0 },
        ];
        finish(&mut items);
        assert_eq!(breaks(&items, |_| 9.0, 100.0).unwrap(), [5, 10]);
        // With a wider hyphen, "aa b ccc-" no longer fits on the first line.
        items[5] = Item::Penalty {
            width: 2.0,
            cost: 50.0,
            flagged: true,
        };
        assert_ne!(breaks(&items, |_| 9.0, 100.0).unwrap()[0], 5);
    }

    fn cluster(kind: Kind, continues: bool) -> Cluster {
        Cluster {
            end: 0,
            chars: 1,
            advance: 1.0,
            kind,
            continues,
        }
    }

    #[test]
    fn no_break_inside_a_ligature() {
        let penalties = |clusters: &[Cluster]| {
            items(clusters, 1.0)
                .0
                .iter()
                .filter(|item| matches!(item, Item::Penalty { cost, .. } if *cost > FORCED))
                .count()
        };
        // "of·fer": the soft hyphen sits between the two f's.
        let plain = [
            cluster(Kind::Other, false),
            cluster(Kind::Other, false),
            cluster(Kind::SoftHyphen, false),
            cluster(Kind::Other, false),
            cluster(Kind::Other, false),
        ];
        assert_eq!(penalties(&plain), 1);
        // Shaped as an "ff" ligature, the second f continues the first.
        let mut joined = plain;
        joined[3].continues = true;
        assert_eq!(penalties(&joined), 0);
    }

    #[test]
    fn the_first_line_can_be_narrower() {
        let items = words(&[3.0; 6]);
        let found = breaks(&items, |line| if line == 0 { 7.0 } else { 11.0 }, 100.0).unwrap();
        assert_eq!(line_lengths(&items, &found), [2, 3, 1]);
    }
}
