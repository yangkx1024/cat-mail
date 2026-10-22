//! An HTML engine for the reader: a message body laid out and painted into pixels, off
//! the UI thread.
//!
//! GPUI Kit's own text view lays out a subset of HTML; the engine here lays out the whole
//! page as a browser would. Each open [`Document`] lives on a thread of its own and
//! answers requests for its height at a width, a strip of its pixels, or the link under
//! a point. Nothing here depends on the UI toolkit. Blitz is the one engine; litehtml was
//! compared and dropped (see `docs/architecture/gui.md`), and another would plug in as a
//! `worker::Page` and an opener.
//!
//! An engine loads nothing by itself: every image the page refers to is asked of the
//! [`Source`]'s resolver, which decides what the reader shows. There is no network
//! provider, stylesheet loader or font loader behind it.

mod blitz;
mod pixels;
mod worker;

use std::fmt;
use std::sync::Arc;

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

/// The bytes of the image at a URL, or `None` for one that isn't shown. The only way an
/// engine gets anything that isn't in the HTML.
pub type Resolver = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

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

/// How the page looks where the message's own styles say nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appearance {
    /// What `prefers-color-scheme` answers.
    pub dark: bool,
    /// The color behind the page, as RGB: what shows where it paints nothing.
    pub canvas: [u8; 3],
    /// A user-agent style sheet, under the message's styles: the reader's colors.
    pub sheet: Arc<str>,
}

impl Default for Appearance {
    /// A browser's: light, on white, mail being written for a white canvas.
    fn default() -> Self {
        Self {
            dark: false,
            canvas: [255; 3],
            sheet: "".into(),
        }
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
        let link = |x, y| block_on(document.link_at(WIDTH, x, y)).unwrap();
        assert_eq!(
            link(10.0, 15.0).as_deref(),
            Some("https://example.com/x?y=1")
        );
        assert_eq!(link(290.0, 15.0), None, "past the end of the line");
        assert_eq!(link(250.0, 45.0).as_deref(), Some("mailto:a@example.com"));
        assert_eq!(link(10.0, 200.0), None, "below the page");
    }

    #[test]
    fn images_come_from_the_resolver_only() {
        let html = r#"<body style="margin:0"><img src="cid:logo@x" width="20" height="20"
            style="display:block"><img src="https://tracker.example/p.gif" width="1" height="1"></body>"#;
        let green = png(20, 20, [0, 255, 0]);
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
    fn paints_in_the_appearance_asked_for() {
        let dark = Appearance {
            dark: true,
            canvas: [0x10, 0x20, 0x30],
            sheet: "a { color: #ff0000 }".into(),
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
            pixel(&paint(adaptive, dark), 290, 30),
            [0x56, 0x34, 0x12, 255]
        );
        assert_eq!(
            pixel(&paint(adaptive, Appearance::default()), 290, 30),
            WHITE
        );
    }
}
