//! Blitz, painted with vello_cpu.
//!
//! The only network Blitz has is the [`NetProvider`] it is given: [`Images`] answers
//! from the resolver, at once, and keeps the requests it has no answer for, to answer
//! from a later resolver (stylesheets and fonts never reach it from sanitized HTML). Built without blitz-dom's `svg` feature, which
//! hands images to usvg and its resolver of local paths.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use anyrender::ImageRenderer as _;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{
    BaseDocument, DocumentConfig, FontContext, Point, UserAgentStylesheet, local_name,
};
use blitz_html::HtmlDocument;
use blitz_traits::net::{NetHandler, NetProvider, Request};
use blitz_traits::shell::{ColorScheme, Viewport};

use crate::selection::Text;
use crate::worker::{Page, Pixmap, VIEWPORT_HEIGHT};
use crate::{Appearance, Hit, Resolver, Selection, Source, Span};

/// Unstyled mail reads like the rest of the app, not in the browsers' Times. A user-agent
/// sheet, so the message's own styles win.
const READER_SHEET: &str = "html { font-family: sans-serif; }";

/// The system fonts, found once: a new context scans them again (tens of milliseconds),
/// and clones share what was found.
static FONTS: LazyLock<Mutex<FontContext>> = LazyLock::new(Mutex::default);

/// The page `source` makes, painted at `scale`.
pub(crate) fn open(source: &Source, appearance: &Appearance, scale: f32) -> Box<dyn Page> {
    let mut ua_stylesheets = vec![
        UserAgentStylesheet::default_sheet(),
        UserAgentStylesheet::parse(READER_SHEET),
    ];
    if !appearance.sheet.is_empty() {
        ua_stylesheets.push(UserAgentStylesheet::parse(&appearance.sheet));
    }
    let scheme = if appearance.dark {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    };
    let fonts = FONTS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let images = Arc::new(Images {
        resolver: Mutex::new(source.images.clone()),
        waiting: Mutex::default(),
    });
    let config = DocumentConfig {
        viewport: Some(viewport(0.0, scale, scheme)),
        ua_stylesheets,
        net_provider: Some(images.clone()),
        font_ctx: Some(fonts),
        ..DocumentConfig::default()
    };
    let recolored = appearance
        .recolor
        .as_ref()
        .map(|recolor| recolor(&source.html));
    let html = recolored.as_deref().unwrap_or(&source.html);
    let document = HtmlDocument::from_html(html, config).into_inner();
    Box::new(BlitzPage {
        document,
        images,
        scale,
        scheme,
        width: 0.0,
        rasterizer: None,
        text: None,
    })
}

/// `width` CSS pixels by [`VIEWPORT_HEIGHT`]. Blitz shapes text at the viewport's scale,
/// so the document is laid out at the scale it is painted at. `scheme` answers
/// `prefers-color-scheme`.
fn viewport(width: f32, scale: f32, scheme: ColorScheme) -> Viewport {
    Viewport::new(
        (width * scale).ceil() as u32,
        (VIEWPORT_HEIGHT * scale).ceil() as u32,
        scale,
        scheme,
    )
}

struct BlitzPage {
    document: BaseDocument,
    images: Arc<Images>,
    scale: f32,
    scheme: ColorScheme,
    width: f32,
    /// Kept between strips, which are all the same size but the last.
    rasterizer: Option<((u32, u32), VelloCpuImageRenderer)>,
    /// The paragraphs of the last layout, once something was selected in it.
    text: Option<Text>,
}

impl BlitzPage {
    fn text(&mut self) -> (&BaseDocument, &Text) {
        // Positions are on the page, not in the strip last painted.
        self.document.set_viewport_scroll(Point { x: 0.0, y: 0.0 });
        let text = (self.text).get_or_insert_with(|| Text::of(&self.document));
        (&self.document, text)
    }
}

impl Page for BlitzPage {
    fn set_images(&mut self, images: Resolver) -> bool {
        self.images.answer_waiting(images)
    }

    fn layout(&mut self, width: f32) -> f32 {
        self.width = width;
        self.document
            .set_viewport(viewport(width, self.scale, self.scheme));
        self.document.resolve(0.0);
        self.text = None;
        self.document.root_element().scroll_height()
    }

    fn paint(&mut self, top: f32, height: f32) -> Pixmap {
        let width = (self.width * self.scale).ceil() as u32;
        let height = (height * self.scale).ceil() as u32;
        // Painting scrolls the viewport to the strip; the layout doesn't change.
        self.document.set_viewport_scroll(Point {
            x: 0.0,
            y: f64::from(top),
        });
        let renderer = match &mut self.rasterizer {
            Some((size, renderer)) if *size == (width, height) => {
                renderer.reset();
                renderer
            }
            rasterizer => {
                let (_, renderer) =
                    rasterizer.insert(((width, height), VelloCpuImageRenderer::new(width, height)));
                renderer
            }
        };
        let mut rgba = Vec::new();
        renderer.render_to_vec(
            |scene| {
                blitz_paint::paint_scene(
                    scene,
                    &mut self.document,
                    f64::from(self.scale),
                    width,
                    height,
                    0,
                    0,
                );
            },
            &mut rgba,
        );
        Pixmap {
            width,
            height,
            rgba,
        }
    }

    fn hit(&mut self, x: f32, y: f32) -> Hit {
        self.document.set_viewport_scroll(Point { x: 0.0, y: 0.0 });
        let Some(hit) = self.document.hit(x, y) else {
            return Hit::default();
        };
        let mut link = None;
        let mut node = self.document.get_node(hit.node_id);
        while let Some(current) = node {
            if current.data.is_element_with_tag_name(&local_name!("a"))
                && let Some(href) = current.attr(local_name!("href"))
            {
                link = Some(href.to_owned());
                break;
            }
            node = current.parent.and_then(|id| self.document.get_node(id));
        }
        Hit {
            link,
            text: hit.is_text,
        }
    }

    fn select(&mut self, span: Span) -> Selection {
        let (document, text) = self.text();
        text.select(document, span)
    }
}

/// Serves each image from the resolver, synchronously; nothing else is fetched. One it
/// has no answer for waits for the next resolver.
struct Images {
    resolver: Mutex<Resolver>,
    /// Blitz's handlers for the images not answered yet, by URL: one per element.
    waiting: Mutex<HashMap<String, Vec<Box<dyn NetHandler>>>>,
}

impl Images {
    /// Asks `resolver` from now on, and now for each image waiting. The document takes in
    /// the images answered at its next layout (the handler decodes them here). Returns
    /// whether there was any.
    fn answer_waiting(&self, resolver: Resolver) -> bool {
        *self.resolver.lock().unwrap_or_else(PoisonError::into_inner) = resolver.clone();
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let mut answered = false;
        waiting.retain(|url, handlers| {
            let Some(bytes) = resolver(url) else {
                return true;
            };
            for handler in handlers.drain(..) {
                handler.bytes(url.clone(), bytes.clone());
            }
            answered = true;
            false
        });
        answered
    }
}

impl NetProvider for Images {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.as_str();
        let resolver = self
            .resolver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match resolver(url) {
            Some(bytes) => handler.bytes(url.to_owned(), bytes),
            None => self
                .waiting
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(url.to_owned())
                .or_default()
                .push(handler),
        }
    }
}
