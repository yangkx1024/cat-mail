//! An HTML engine for the reader: a message body laid out as a browser would and painted
//! into pixels, off the UI thread.
//!
//! Each open [`Document`] lives on a thread of its own and answers requests for its
//! height at a width, a strip of its pixels, what is under a point, or the text between
//! two places ([`Selection`]). Nothing here depends on the UI toolkit. Blitz is the one
//! engine; litehtml was compared and dropped (see `docs/architecture/gui.md`), and another
//! would plug in as a `worker::Page` and an opener.
//!
//! An engine loads nothing by itself: every image the page refers to is asked of the
//! [`Source`]'s resolver, which decides what the reader shows. There is no network
//! provider, stylesheet loader or font loader behind it. Images the resolver has no answer
//! for yet can be given to the open document later ([`Document::set_images`]), without
//! opening it again.

#![forbid(unsafe_code)]

mod blitz;
mod pixels;
mod selection;
mod worker;

use std::fmt;
use std::sync::Arc;

pub use bytes::Bytes;

pub use worker::Document;

/// An HTML engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    /// [Blitz](https://github.com/DioxusLabs/blitz): Stylo (Firefox's CSS engine), Taffy
    /// and Parley, painted with vello_cpu.
    Blitz,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Blitz => "Blitz",
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The bytes of the image at a URL, or `None` for one that isn't shown (or not yet: see
/// [`Document::set_images`]). The only way an engine gets anything that isn't in the
/// HTML. `Bytes`, which is what Blitz keeps, so the caller can share its copy instead of
/// making one each time a document opens.
pub type Resolver = Arc<dyn Fn(&str) -> Option<Bytes> + Send + Sync>;

/// A page to render.
#[derive(Clone)]
pub struct Source {
    /// Sanitized HTML: no scripts, no stylesheets that load anything.
    pub html: Arc<str>,
    pub images: Resolver,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("html", &self.html.len())
            .finish_non_exhaustive()
    }
}

/// Rewrites a page's HTML before it is parsed, or declines to: `None`.
pub type Recolor = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// How the page looks where the message's own styles say nothing, and what becomes of
/// the colors they set.
#[derive(Clone)]
pub struct Appearance {
    /// What `prefers-color-scheme` answers.
    pub dark: bool,
    /// The color behind the page, as RGB: what shows where it paints nothing.
    pub canvas: [u8; 3],
    /// A user-agent style sheet, under the message's styles: the reader's colors.
    pub sheet: Arc<str>,
    /// Moves the page's own colors for this appearance, run on the document's thread:
    /// a pass over the whole HTML that the UI thread shouldn't wait for. If it can't, the
    /// page is shown in the default appearance, as written for a light page.
    pub recolor: Option<Recolor>,
}

impl Default for Appearance {
    /// A browser's: light, on white, mail being written for a white canvas.
    fn default() -> Self {
        Self {
            dark: false,
            canvas: [255; 3],
            sheet: "".into(),
            recolor: None,
        }
    }
}

impl fmt::Debug for Appearance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Appearance")
            .field("dark", &self.dark)
            .field("canvas", &self.canvas)
            .field("sheet", &self.sheet)
            .field("recolor", &self.recolor.is_some())
            .finish()
    }
}

/// A horizontal strip of a laid-out page, in device pixels: opaque BGRA rows, the page
/// drawn over [`Appearance::canvas`].
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl fmt::Debug for Tile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tile({}×{})", self.width, self.height)
    }
}

/// A rectangle of a page, in CSS pixels from its top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    fn right(&self) -> f32 {
        self.x + self.width
    }

    fn bottom(&self) -> f32 {
        self.y + self.height
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        (self.x..self.right()).contains(&x) && (self.y..self.bottom()).contains(&y)
    }
}

/// What is under a point of a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hit {
    /// The `href` of the link there.
    pub link: Option<String>,
    /// Whether it is on a letter.
    pub text: bool,
}

/// One end of a [`Selection`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionEnd {
    /// Before the page's first letter.
    Start,
    /// The place in the text nearest a point, in CSS pixels from the top left of the
    /// page: one between two paragraphs, or past the end of a line, is still in the text.
    At(f32, f32),
    /// After the page's last letter.
    End,
}

/// What to select.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Span {
    /// The text between two places, in either order.
    Between(SelectionEnd, SelectionEnd),
    /// The word or paragraph at a point, in CSS pixels from the top left of the page;
    /// nothing if it is on no paragraph.
    Around(f32, f32, Unit),
}

impl Span {
    /// The same span, with its points in units `factor` times as large (window pixels to
    /// CSS pixels, at a zoom).
    pub fn scaled_down(self, factor: f32) -> Self {
        let end = |end| match end {
            SelectionEnd::At(x, y) => SelectionEnd::At(x / factor, y / factor),
            end => end,
        };
        match self {
            Span::Between(from, to) => Span::Between(end(from), end(to)),
            Span::Around(x, y, unit) => Span::Around(x / factor, y / factor, unit),
        }
    }
}

/// What a double or triple click selects around the point it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Word,
    /// The run of text the point is in, between two blocks or line breaks.
    Paragraph,
}

/// The text between two places of a page, and where it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    /// What to highlight: a rectangle for each line of each paragraph.
    pub rects: Vec<Rect>,
    /// The paragraphs, a line each.
    pub text: String,
}

/// The document's thread has stopped: the engine panicked on the page.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("the engine stopped on this message")]
pub struct Stopped;

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Mutex;

    use pollster::block_on;

    use super::*;
    use SelectionEnd::{At, End, Start};

    const WIDTH: f32 = 300.0;

    fn source(html: &str) -> Source {
        Source {
            html: html.into(),
            images: Arc::new(|_| None),
        }
    }

    fn open(source: Source, appearance: Appearance, scale: f32) -> Document {
        Document::open(Engine::Blitz, source, appearance, scale).unwrap()
    }

    /// The BGRA pixel at (`x`, `y`) device pixels.
    fn pixel(tile: &Tile, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * tile.width + x) * 4) as usize;
        tile.bgra[at..at + 4].try_into().unwrap()
    }

    fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(rgb));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    #[test]
    fn paints_strips_of_the_page_at_the_scale_it_was_opened_with() {
        let html = r#"<body style="margin:0">
            <div style="height:40px;background:#ff0000"></div>
            <div style="height:40px;background:#0000ff"></div></body>"#;
        let document = open(source(html), Appearance::default(), 2.0);
        let height = block_on(document.layout(WIDTH)).unwrap();
        assert!((80.0..=VIEWPORT).contains(&height), "{height}");

        let top = block_on(document.paint(WIDTH, 0.0, 40.0)).unwrap();
        assert_eq!((top.width, top.height), (600, 80));
        assert_eq!(pixel(&top, 10, 10), RED);
        assert_eq!(pixel(&top, 590, 70), RED);

        let below = block_on(document.paint(WIDTH, 40.0, 40.0)).unwrap();
        assert_eq!(pixel(&below, 10, 10), BLUE);
        assert_eq!(pixel(&below, 10, 78), BLUE);
    }

    const VIEWPORT: f32 = worker::VIEWPORT_HEIGHT;

    #[test]
    fn a_narrower_page_is_taller() {
        let words = "word ".repeat(400);
        let html = format!("<p>{words}</p>");
        let document = open(source(&html), Appearance::default(), 1.0);
        let wide = block_on(document.layout(600.0)).unwrap();
        let narrow = block_on(document.layout(200.0)).unwrap();
        assert!(narrow > wide * 2.0, "{narrow} vs {wide}");
        assert!(narrow > VIEWPORT, "{narrow}");
    }

    #[test]
    fn draws_text() {
        let html =
            r#"<body style="margin:0"><p style="margin:0;font-size:32px">Hello 你好</p></body>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        let tile = block_on(document.paint(WIDTH, 0.0, 40.0)).unwrap();
        let inked = tile
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] < 128)
            .count();
        assert!(inked > 50, "{inked} dark pixels");
    }

    #[test]
    fn finds_the_link_under_a_point() {
        let html = r#"<body style="margin:0">
            <p style="margin:0;font-size:20px;line-height:30px">
            <a href="https://example.com/x?y=1">a link here</a> and text</p>
            <div style="height:30px"><a href="mailto:a@example.com"
                style="display:block;height:30px">block</a></div></body>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        let hit = |x, y| block_on(document.hit(WIDTH, x, y)).unwrap();
        let link = |x, y| hit(x, y).link;
        assert_eq!(
            link(10.0, 15.0).as_deref(),
            Some("https://example.com/x?y=1")
        );
        assert_eq!(link(290.0, 15.0), None, "past the end of the line");
        assert_eq!(link(250.0, 45.0).as_deref(), Some("mailto:a@example.com"));
        assert_eq!(link(10.0, 200.0), None, "below the page");

        assert!(hit(10.0, 15.0).text);
        assert!(!hit(290.0, 15.0).text);
    }

    /// What `span` selects in `document`, laid out `WIDTH` wide.
    fn select(document: &Document, span: Span) -> Selection {
        block_on(document.select(WIDTH, span)).unwrap()
    }

    /// The text `span` selects in `document`.
    fn text(document: &Document, span: Span) -> String {
        select(document, span).text
    }

    #[test]
    fn a_cell_made_a_block_is_laid_out_as_a_cell() {
        // Templates restyle a cell as a block, inline (Constant Contact) or from their
        // style sheet (Mailchimp); a browser wraps it in an anonymous cell.
        let html = r#"<body style="margin:0;font-size:20px;line-height:30px">
            <style>td.container { display: block; max-width: 200px; margin: 0 auto }</style>
            <table width="100%"><tr><td style="display:block">inline</td></tr></table>
            <table width="100%"><tr><td class="container">sheet</td></tr></table>
            </body>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        assert_eq!(text(&document, Span::Between(Start, End)), "inline\nsheet");
    }

    #[test]
    fn legacy_color_attributes_are_read_as_a_browser_reads_them() {
        // Mail writes `bgcolor` and `<font color>` in every form the HTML standard reads:
        // short hex, names, without the `#`, with spaces around.
        let values = ["#ff0000", "#fff", "#f00", "red", "ff0000", " #ff0000"];
        let expected = [RED, WHITE, RED, RED, RED, RED];
        let rows: String = values
            .iter()
            .map(|v| {
                format!(
                    r#"<table width="100%" height="10" bgcolor="{v}"><tr><td></td></tr></table>"#
                )
            })
            .collect();
        // Text color shown as a background, so the test doesn't read glyphs.
        let fonts: String = values
            .iter()
            .map(|v| format!(r#"<font color="{v}"><div style="height:10px;background:currentcolor"></div></font>"#))
            .collect();
        let html = format!(
            r#"<body style="margin:0;background:#0000ff">
            <style>table {{ border-spacing: 0 }} td {{ padding: 0 }}</style>{rows}{fonts}</body>"#
        );
        let document = open(source(&html), Appearance::default(), 1.0);
        let tile = block_on(document.paint(WIDTH, 0.0, 120.0)).unwrap();
        for (i, (value, color)) in values.iter().zip(expected).enumerate() {
            let y = i as u32 * 10 + 5;
            assert_eq!(pixel(&tile, 150, y), color, "bgcolor={value:?}");
            assert_eq!(pixel(&tile, 150, y + 60), color, "color={value:?}");
        }
    }

    /// Two paragraphs a line each, 30 px apart: 0..30 and 60..90, and a table below.
    const PARAGRAPHS: &str = r#"<body style="margin:0;font-size:20px;line-height:30px">
        <p style="margin:0 0 30px">alpha beta</p><p style="margin:0">gamma delta<br>epsilon</p>
        <table cellspacing="0" cellpadding="0"><tr><td>left</td><td>right</td></tr></table>
        </body>"#;

    #[test]
    fn selects_the_text_between_two_places() {
        let document = open(source(PARAGRAPHS), Appearance::default(), 1.0);
        let between = |from, to| select(&document, Span::Between(from, to));

        let all = between(Start, End);
        assert_eq!(all.text, "alpha beta\ngamma delta\nepsilon\nleft\nright");
        let first = all.rects[0];
        assert_eq!((first.x, first.y, first.height), (0.0, 0.0, 30.0));
        assert!((60.0..150.0).contains(&first.width), "{first:?}");
        assert_eq!(all.rects[1].y, 60.0, "{:?}", all.rects);

        // From the first letter into the gap below, whichever way it is dragged.
        let first = between(At(1.0, 15.0), At(10.0, 45.0));
        assert_eq!(first.text, "alpha beta");
        assert_eq!(between(At(10.0, 45.0), At(1.0, 15.0)), first);
        // From past the end of a line to before the start of the next: the line break.
        assert_eq!(between(At(290.0, 75.0), At(0.0, 105.0)).text, "\n");
        // Off the side of a paragraph, the nearest end of its line.
        assert_eq!(between(At(-5.0, 75.0), At(290.0, 75.0)).text, "gamma delta");
        // Above or below all the text.
        assert_eq!(between(At(10.0, -20.0), At(1.0, 15.0)).text, "");
        assert_eq!(between(At(1.0, 135.0), At(10.0, 900.0)).text, "left\nright");

        let none = open(source("<p></p>"), Appearance::default(), 1.0);
        let nothing = select(&none, Span::Between(Start, End));
        assert_eq!(nothing, Selection::default());
    }

    #[test]
    fn an_inline_block_is_selected_where_it_sits_in_its_line() {
        let html = r#"<body style="margin:0;font-size:20px;line-height:30px">
            <p style="margin:0">before <span style="display:inline-block">INNER</span> after</p>
            <p style="margin:0">next</p></body>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        let between = |from, to| text(&document, Span::Between(from, to));
        assert_eq!(between(Start, End), "before INNER after\nnext");
        assert_eq!(
            between(At(1.0, 15.0), At(299.0, 15.0)),
            "before INNER after"
        );
        // Its own words are found inside it, not in the line around it.
        let word = text(&document, Span::Around(90.0, 15.0, Unit::Word));
        assert_eq!(word, "INNER");

        // It is highlighted once, by its own text, not by the line around it too.
        let all = select(&document, Span::Between(Start, End));
        for (i, a) in all.rects.iter().enumerate() {
            for b in &all.rects[i + 1..] {
                let overlap = (a.right().min(b.right()) - a.x.max(b.x))
                    .min(a.bottom().min(b.bottom()) - a.y.max(b.y));
                assert!(overlap <= 0.5, "{a:?} and {b:?} overlap");
            }
        }
        let inner = all.rects[1];
        let after = select(
            &document,
            Span::Between(At(inner.right() + 1.0, 15.0), At(299.0, 15.0)),
        );
        assert_eq!(after.text, " after");
        for rect in &after.rects {
            assert!(rect.x >= inner.right() - 0.5, "{rect:?} covers {inner:?}");
        }
    }

    #[test]
    fn an_inline_block_at_either_end_of_a_line_is_in_it() {
        // One ends the first line, the other starts the second.
        let html = r#"<body style="margin:0;font-size:20px;line-height:30px">
            <p style="margin:0">before <span style="display:inline-block">INNER</span></p>
            <p style="margin:0"><span style="display:inline-block">FIRST</span> after</p>
            </body>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        let between = |from, to| text(&document, Span::Between(from, to));
        let around = |x, y, unit| text(&document, Span::Around(x, y, unit));
        assert_eq!(between(Start, End), "before INNER\nFIRST after");
        // Dragged across the whole line, past the block, or to before it.
        assert_eq!(between(At(1.0, 15.0), At(299.0, 15.0)), "before INNER");
        let two_lines = between(At(1.0, 15.0), At(299.0, 45.0));
        assert_eq!(two_lines, "before INNER\nFIRST after");
        // From after the first block: nothing of its line, so no line break either.
        assert_eq!(between(At(299.0, 15.0), At(299.0, 45.0)), "FIRST after");
        // A paragraph has its blocks; a word next to one doesn't.
        assert_eq!(around(5.0, 15.0, Unit::Paragraph), "before INNER");
        assert_eq!(around(200.0, 45.0, Unit::Paragraph), "FIRST after");
        assert_eq!(around(5.0, 15.0, Unit::Word), "before");
        assert_eq!(around(200.0, 45.0, Unit::Word), "after");
    }

    #[test]
    fn what_is_not_painted_is_not_selected() {
        // Blitz paints nothing inside a hidden or transparent element, even what sets
        // itself visible again.
        let html = r#"<p style="visibility:hidden">HIDDEN</p><p>VISIBLE</p>
            <p style="opacity:0">TRANSPARENT</p>
            <div style="visibility:hidden"><p style="visibility:visible">INSIDE</p></div>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        let all = select(&document, Span::Between(Start, End));
        assert_eq!(all.text, "VISIBLE");
        assert_eq!(all.rects.len(), 1);
    }

    #[test]
    fn selects_the_word_or_paragraph_at_a_point() {
        let document = open(source(PARAGRAPHS), Appearance::default(), 1.0);
        let around = |x, y, unit| select(&document, Span::Around(x, y, unit));
        assert_eq!(around(5.0, 15.0, Unit::Word).text, "alpha");
        assert_eq!(around(5.0, 75.0, Unit::Paragraph).text, "gamma delta");
        assert_eq!(around(5.0, 105.0, Unit::Paragraph).text, "epsilon");
        assert_eq!(
            around(5.0, 45.0, Unit::Word),
            Selection::default(),
            "between"
        );
    }

    #[test]
    fn images_come_from_the_resolver_only() {
        let html = r#"<body style="margin:0"><img src="cid:logo@x" width="20" height="20"
            style="display:block"><img src="https://tracker.example/p.gif" width="1" height="1"></body>"#;
        let green = Bytes::from(png(20, 20, [0, 255, 0]));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let images: Resolver = Arc::new({
            let asked = asked.clone();
            move |url| {
                asked.lock().unwrap().push(url.to_owned());
                (url == "cid:logo@x").then(|| green.clone())
            }
        });
        let source = Source {
            html: html.into(),
            images,
        };
        let document = open(source, Appearance::default(), 1.0);
        let tile = block_on(document.paint(WIDTH, 0.0, 30.0)).unwrap();
        assert_eq!(pixel(&tile, 10, 10), GREEN);
        assert_eq!(pixel(&tile, 30, 10), WHITE);
        let mut asked = asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, ["cid:logo@x", "https://tracker.example/p.gif"]);
    }

    #[test]
    fn images_given_later_show_in_the_open_document() {
        // No size of its own: the page grows when the image comes.
        let html = r#"<body style="margin:0"><img src="cid:logo@x" style="display:block">
            <img src="https://x.example/later.png" style="display:block"></body>"#;
        let document = open(source(html), Appearance::default(), 1.0);
        let short = block_on(document.layout(WIDTH)).unwrap();

        let green = Bytes::from(png(20, 30, [0, 255, 0]));
        let asked = Arc::new(Mutex::new(Vec::new()));
        document.set_images(Arc::new({
            let asked = asked.clone();
            move |url| {
                asked.lock().unwrap().push(url.to_owned());
                (url == "cid:logo@x").then(|| green.clone())
            }
        }));
        let tall = block_on(document.layout(WIDTH)).unwrap();
        assert_eq!(tall, short + 30.0);
        let tile = block_on(document.paint(WIDTH, 0.0, 30.0)).unwrap();
        assert_eq!(pixel(&tile, 10, 10), GREEN);

        // Only what was waiting is asked again; what is answered isn't asked twice.
        let mut asked_now = asked.lock().unwrap().clone();
        asked_now.sort();
        assert_eq!(asked_now, ["cid:logo@x", "https://x.example/later.png"]);
        let red = Bytes::from(png(20, 30, [255, 0, 0]));
        document.set_images(Arc::new(move |_| Some(red.clone())));
        assert_eq!(block_on(document.layout(WIDTH)).unwrap(), short + 60.0);
        let tile = block_on(document.paint(WIDTH, 0.0, 60.0)).unwrap();
        assert_eq!(pixel(&tile, 10, 10), GREEN, "kept");
        assert_eq!(pixel(&tile, 10, 40), RED);
    }

    #[test]
    fn paints_in_the_appearance_asked_for() {
        let dark = Appearance {
            dark: true,
            canvas: [0x10, 0x20, 0x30],
            sheet: "a { color: #ff0000 }".into(),
            recolor: None,
        };
        let paint = |html: &str, appearance: Appearance| {
            let document = open(source(html), appearance, 1.0);
            block_on(document.paint(WIDTH, 0.0, 40.0)).unwrap()
        };

        // Where the page paints nothing, the canvas shows.
        let plain = paint("<p>x</p>", dark.clone());
        assert_eq!(pixel(&plain, 290, 30), [0x30, 0x20, 0x10, 255]);
        // The message's own dark styles apply, and its light ones otherwise.
        let adaptive = r#"<style>body{background:#ffffff}
            @media (prefers-color-scheme: dark){body{background:#123456}}</style>
            <body style="margin:0;height:40px">x</body>"#;
        assert_eq!(
            pixel(&paint(adaptive, dark.clone()), 290, 30),
            [0x56, 0x34, 0x12, 255]
        );
        assert_eq!(
            pixel(&paint(adaptive, Appearance::default()), 290, 30),
            WHITE
        );
        // The page's own colors are moved as the appearance says.
        let recolored = Appearance {
            recolor: Some(Arc::new(|html: &str| {
                Some(html.replace("#ffffff", "#00ff00"))
            })),
            ..dark.clone()
        };
        let light = r#"<body style="margin:0;height:40px;background:#ffffff">x</body>"#;
        assert_eq!(pixel(&paint(light, recolored), 290, 30), GREEN);
        // One it can't move shows light, as written: its light styles, on white.
        let declined = Appearance {
            recolor: Some(Arc::new(|_: &str| None)),
            ..dark
        };
        assert_eq!(pixel(&paint(adaptive, declined.clone()), 290, 30), WHITE);
        assert_eq!(pixel(&paint("<p>x</p>", declined), 290, 30), WHITE);
    }
}
