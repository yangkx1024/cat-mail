//! The text of a Blitz page, for selecting it: where each paragraph is, and what lies
//! between two places.
//!
//! A page's text is in its inline roots: each run of inline content Blitz lays out as
//! one Parley paragraph. They are taken in layout order, which is reading order, and an
//! inline block (a paragraph of its own, in a line of another) is read where it sits in
//! that line: the line's text is cut into pieces around it. What the page doesn't paint
//! (`visibility: hidden`, `opacity: 0`) isn't selected.
//!
//! A place is found by geometry rather than by Blitz's hit test, which finds nothing
//! between two paragraphs: a drag through a margin still selects up to the text nearest
//! the pointer.
//!
//! Blitz's own selection isn't used. It paints itself into the page in a fixed light blue;
//! the reader draws the theme's over the page instead, from [`Selection::rects`].

use std::ops::Range;

use blitz_dom::node::{TextBrush, TextLayout};
use blitz_dom::{BaseDocument, Node, NodeId};
use parley::{Affinity, Cursor, PositionedLayoutItem};
use style::computed_values::visibility::T as Visibility;

use crate::{Rect, Selection, SelectionEnd, Span, Unit};

/// One paragraph of a page as laid out.
struct Paragraph {
    node: NodeId,
    /// The content box of its block, in CSS pixels from the top left of the page.
    bounds: Rect,
    /// Where its text layout is drawn from.
    origin: (f32, f32),
    /// The inline blocks in it with text of their own, by node: that text highlights
    /// them, so its own highlight leaves them out. (Parley's runs over them.)
    blocks: Vec<u64>,
}

/// A run of a paragraph's text, up to or after an inline block in it.
struct Piece {
    paragraph: usize,
    range: Range<usize>,
    /// The inline block it comes after, by node.
    after: Option<u64>,
    /// Whether it reads on in the line of the piece before it, rather than in a line of
    /// its own.
    joined: bool,
}

/// A place in the text: a piece, by its index, and a byte offset in its paragraph's text.
type Place = (usize, usize);

/// The paragraphs of a page at one layout, and their pieces in reading order.
pub(crate) struct Text {
    paragraphs: Vec<Paragraph>,
    pieces: Vec<Piece>,
}

/// What is left to read.
enum Visit {
    /// A node, and whether its text reads on in the line before it.
    Node(NodeId, bool),
    Piece(Piece),
}

impl Text {
    pub(crate) fn of(document: &BaseDocument) -> Self {
        let mut text = Self {
            paragraphs: Vec::new(),
            pieces: Vec::new(),
        };
        // Depth first, by hand: a message can nest its elements deeper than the stack goes.
        let mut stack = vec![Visit::Node(document.root_element().id, false)];
        while let Some(visit) = stack.pop() {
            let (id, joined) = match visit {
                Visit::Node(id, joined) => (id, joined),
                // Empty ones too: at an inline block that ends a line, the place after it.
                Visit::Piece(piece) => {
                    text.pieces.push(piece);
                    continue;
                }
            };
            let Some(node) = document.get_node(id) else {
                continue;
            };
            // What isn't painted isn't selected: Blitz paints nothing of an element hidden
            // or wholly transparent, whatever is inside it.
            let unpainted = node.primary_styles().is_some_and(|styles| {
                styles.clone_visibility() != Visibility::Visible
                    || styles.get_effects().opacity == 0.0
            });
            if unpainted {
                continue;
            }
            let children = node.layout_children.borrow();
            let children = children.as_deref().unwrap_or_default();
            let visits = match text.paragraph(document, node) {
                Some((index, layout)) => cut_around_boxes(index, layout, children, joined),
                // Its first child's text, if any, reads on where its own would have.
                None => (children.iter().enumerate())
                    .map(|(i, &child)| Visit::Node(child, joined && i == 0))
                    .collect(),
            };
            stack.extend(visits.into_iter().rev());
        }
        // A block with text has pieces of it between the two of its paragraph around it.
        let mut last = vec![None; text.paragraphs.len()];
        for (i, piece) in text.pieces.iter().enumerate() {
            let before = last[piece.paragraph].replace(i);
            if let (Some(block), Some(before)) = (piece.after, before)
                && i > before + 1
            {
                text.paragraphs[piece.paragraph].blocks.push(block);
            }
        }
        text
    }

    /// Adds `node` as a paragraph if it is one, with text; returns its index and text.
    fn paragraph<'d>(
        &mut self,
        document: &'d BaseDocument,
        node: &Node,
    ) -> Option<(usize, &'d TextLayout)> {
        let text = text_layout(document, node.id).filter(|text| !text.text.is_empty())?;
        if !node.flags.is_inline_root() {
            return None;
        }
        let layout = node.final_layout();
        // Where the node's content is drawn, scrolled as the element is.
        let at = node.absolute_position(
            layout.padding.left + layout.border.left,
            layout.padding.top + layout.border.top,
        );
        self.paragraphs.push(Paragraph {
            node: node.id,
            bounds: Rect {
                x: at.x,
                y: at.y,
                width: layout.content_box_width(),
                height: layout.content_box_height(),
            },
            origin: (at.x, at.y + text.block_offset),
            blocks: Vec::new(),
        });
        Some((self.paragraphs.len() - 1, text))
    }

    pub(crate) fn select(&self, document: &BaseDocument, span: Span) -> Selection {
        match span {
            Span::Between(from, to) => {
                let (Some(from), Some(to)) = (self.place(document, from), self.place(document, to))
                else {
                    return Selection::default();
                };
                self.between(document, from.min(to), from.max(to))
            }
            Span::Around(x, y, unit) => self.around(document, x, y, unit),
        }
    }

    fn around(&self, document: &BaseDocument, x: f32, y: f32, unit: Unit) -> Selection {
        let Some(index) = self.containing(x, y) else {
            return Selection::default();
        };
        let Some(layout) = text_layout(document, self.paragraphs[index].node) else {
            return Selection::default();
        };
        let (lx, ly) = self.layout_point(index, layout.layout.scale(), x, y);
        let around = match unit {
            Unit::Word => parley::Selection::word_from_point(&layout.layout, lx, ly),
            Unit::Paragraph => parley::Selection::hard_line_from_point(&layout.layout, lx, ly),
        };
        let range = around.text_range();
        let len = layout.text.len();
        let (start, end) = (range.start.min(len), range.end.min(len));
        let (at_start, at_end) = (self.pieces_at(index, start), self.pieces_at(index, end));
        // A paragraph takes the inline blocks at its ends, a word leaves them out.
        let (from, to) = match unit {
            Unit::Word => (at_start.last(), at_end.first()),
            Unit::Paragraph => (at_start.first(), at_end.last()),
        };
        let (Some(&from), Some(&to)) = (from, to) else {
            return Selection::default();
        };
        self.between(document, (from, start), (to, end))
    }

    /// Where an end of a selection is in the text.
    fn place(&self, document: &BaseDocument, end: SelectionEnd) -> Option<Place> {
        let last = self.pieces.len().checked_sub(1)?;
        let first = Some((0, self.pieces[0].range.start));
        let (x, y) = match end {
            SelectionEnd::Start => return first,
            SelectionEnd::End => return Some((last, self.pieces[last].range.end)),
            SelectionEnd::At(x, y) => (x, y),
        };
        // On no paragraph: the nearest one beside the point, on the same row.
        let beside = || {
            (self.paragraphs.iter().enumerate())
                .filter(|(_, p)| (p.bounds.y..p.bounds.bottom()).contains(&y))
                .min_by(|(_, a), (_, b)| {
                    horizontal_distance(&a.bounds, x).total_cmp(&horizontal_distance(&b.bounds, x))
                })
                .map(|(index, _)| index)
        };
        if let Some(index) = self.containing(x, y).or_else(beside) {
            let (offset, passed) = self.offset_at(document, index, x, y);
            let at = self.pieces_at(index, offset);
            return Some((*at.get(passed).or(at.last())?, offset));
        }
        // Between rows: after the paragraph that ends nearest above the point.
        let above = (self.paragraphs.iter().enumerate())
            .filter(|(_, p)| p.bounds.bottom() <= y)
            .max_by(|(_, a), (_, b)| a.bounds.bottom().total_cmp(&b.bounds.bottom()));
        let Some((index, _)) = above else {
            // Above all the text.
            return first;
        };
        let len = self.len(document, index);
        Some((*self.pieces_at(index, len).last()?, len))
    }

    /// The innermost paragraph whose block the point is in: one nested in another (an
    /// inline block in a line) comes after it.
    fn containing(&self, x: f32, y: f32) -> Option<usize> {
        (self.paragraphs.iter()).rposition(|p| p.bounds.contains(x, y))
    }

    /// The pieces of paragraph `index` an offset in its text is in, in order: more than
    /// one where inline blocks sit at it, which the text alone can't tell apart.
    fn pieces_at(&self, index: usize, offset: usize) -> Vec<usize> {
        (self.pieces.iter().enumerate())
            .filter(|(_, piece)| piece.paragraph == index)
            .filter(|(_, piece)| (piece.range.start..=piece.range.end).contains(&offset))
            .map(|(i, _)| i)
            .collect()
    }

    /// The offset in paragraph `index` nearest a point, in or beside it, and how many of
    /// the inline blocks at that offset the point is past.
    fn offset_at(&self, document: &BaseDocument, index: usize, x: f32, y: f32) -> (usize, usize) {
        let Some(layout) = text_layout(document, self.paragraphs[index].node) else {
            return (0, 0);
        };
        let (lx, ly) = self.layout_point(index, layout.layout.scale(), x, y);
        let offset = Cursor::from_point(&layout.layout, lx, ly)
            .index()
            .min(layout.text.len());
        let at: Vec<u64> = (layout.layout.inline_boxes())
            .filter(|inline_box| inline_box.index == offset)
            .map(|inline_box| inline_box.id)
            .collect();
        let rtl = layout.layout.is_rtl();
        let mut passed = 0;
        for line in layout.layout.lines().filter(|_| !at.is_empty()) {
            let metrics = line.metrics();
            for item in line.items() {
                let PositionedLayoutItem::InlineBox(placed) = item else {
                    continue;
                };
                // On a line below it, or past its middle on its line, in the line's direction.
                let middle = placed.x + placed.width / 2.0;
                let past = ly >= metrics.block_max_coord
                    || (ly >= metrics.block_min_coord && (lx >= middle) != rtl);
                if at.contains(&placed.id) && past {
                    passed += 1;
                }
            }
        }
        (offset, passed)
    }

    /// A point of the page in paragraph `index`'s layout, which is in device pixels.
    fn layout_point(&self, index: usize, scale: f32, x: f32, y: f32) -> (f32, f32) {
        let (ox, oy) = self.paragraphs[index].origin;
        ((x - ox) * scale, (y - oy) * scale)
    }

    fn len(&self, document: &BaseDocument, index: usize) -> usize {
        text_layout(document, self.paragraphs[index].node).map_or(0, |layout| layout.text.len())
    }

    /// The text from `start` to `end`, which doesn't come before it.
    fn between(&self, document: &BaseDocument, start: Place, end: Place) -> Selection {
        let mut selection = Selection::default();
        for index in start.0..=end.0 {
            let piece = &self.pieces[index];
            let paragraph = &self.paragraphs[piece.paragraph];
            let Some(layout) = text_layout(document, paragraph.node) else {
                continue;
            };
            let from = if index == start.0 {
                start.1.max(piece.range.start)
            } else {
                piece.range.start
            };
            let to = if index == end.0 {
                end.1.min(piece.range.end)
            } else {
                piece.range.end
            };
            let Some(text) = layout.text.get(from..to).filter(|text| !text.is_empty()) else {
                continue;
            };
            if !piece.joined && !selection.text.is_empty() {
                selection.text.push('\n');
            }
            selection.text.push_str(text);

            let scale = layout.layout.scale();
            let (ox, oy) = paragraph.origin;
            let range = parley::Selection::new(
                Cursor::from_byte_index(&layout.layout, from, Affinity::Downstream),
                Cursor::from_byte_index(&layout.layout, to, Affinity::Downstream),
            );
            range.geometry_with(&layout.layout, |bounds, line| {
                let (x0, x1) = (bounds.x0 as f32, bounds.x1 as f32);
                for (x0, x1) in leave_out_blocks(&layout.layout, line, &paragraph.blocks, x0, x1) {
                    selection.rects.push(Rect {
                        x: ox + x0 / scale,
                        y: oy + bounds.y0 as f32 / scale,
                        width: (x1 - x0) / scale,
                        height: bounds.height() as f32 / scale,
                    });
                }
            });
        }
        selection
    }
}

/// Paragraph `index`'s text in pieces, with each inline block in it read between the two
/// it sits between; then its other children (floats, positioned boxes), in lines of their
/// own.
fn cut_around_boxes(
    index: usize,
    layout: &TextLayout,
    children: &[NodeId],
    mut joined: bool,
) -> Vec<Visit> {
    let mut visits = Vec::new();
    let mut boxes = Vec::new();
    let (mut start, mut after) = (0, None);
    // In text order.
    for inline_box in layout.layout.inline_boxes() {
        let Some(&child) = (children.iter()).find(|child| child.as_u64() == inline_box.id) else {
            continue;
        };
        let range = start..inline_box.index.clamp(start, layout.text.len());
        // A block before any text reads on where the text would have.
        let child_joined = joined || !range.is_empty();
        start = range.end;
        visits.push(Visit::Piece(Piece {
            paragraph: index,
            range,
            after,
            joined,
        }));
        visits.push(Visit::Node(child, child_joined));
        boxes.push(child);
        after = Some(inline_box.id);
        joined = true;
    }
    visits.push(Visit::Piece(Piece {
        paragraph: index,
        range: start..layout.text.len(),
        after,
        joined,
    }));
    let others = children.iter().filter(|child| !boxes.contains(child));
    visits.extend(others.map(|&child| Visit::Node(child, false)));
    visits
}

/// The parts of `x0..x1` on line `line` of a paragraph's layout that none of `blocks` covers.
fn leave_out_blocks(
    layout: &parley::Layout<TextBrush>,
    line: usize,
    blocks: &[u64],
    x0: f32,
    x1: f32,
) -> Vec<(f32, f32)> {
    let mut parts = vec![(x0, x1)];
    let Some(line) = layout.get(line).filter(|_| !blocks.is_empty()) else {
        return parts;
    };
    for item in line.items() {
        let PositionedLayoutItem::InlineBox(placed) = item else {
            continue;
        };
        if !blocks.contains(&placed.id) {
            continue;
        }
        let (left, right) = (placed.x, placed.x + placed.width);
        parts = (parts.into_iter())
            .flat_map(|(x0, x1)| [(x0, x1.min(left)), (x0.max(right), x1)])
            .filter(|(x0, x1)| x1 > x0)
            .collect();
    }
    parts
}

fn text_layout(document: &BaseDocument, node: NodeId) -> Option<&TextLayout> {
    document
        .get_node(node)?
        .element_data()?
        .inline_layout_data
        .as_deref()
}

/// How far `x` is from a rectangle's left or right, or 0 inside it.
fn horizontal_distance(bounds: &Rect, x: f32) -> f32 {
    (bounds.x - x).max(x - bounds.right()).max(0.0)
}
