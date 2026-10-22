//! A message's HTML as an engine of `catmail-render` lays it out: the page painted in
//! strips on the engine's thread and shown as images.
//!
//! The view measures itself while painting (the pane's width decides the layout, the
//! scroll position which strips are needed) and asks the engine only when one of those
//! changed. Strips are dropped from the window's sprite atlas when they are evicted or
//! the view is released; GPUI keeps an image there until told otherwise.
//!
//! In a dark theme the page is dark too when the message allows it ([`Look`]): it sets
//! no colors, or has dark styles of its own. A message colored for a white page keeps
//! its white, as a browser would show it.
//!
//! The page zooms with the window's text size, as a browser page zooms: everything on it,
//! since most mail sets its own font sizes in pixels.

use std::collections::HashMap;
use std::future::Future;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use catmail_core::html::ColorUsage;
use catmail_render::{Appearance, Document, Engine, Source, Stopped, Tile};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Theme, h_flex};
use gpui_kit::{
    App, Bounds, ClickEvent, Context, FutureExt as _, Hsla, InteractiveElement as _, IntoElement,
    MouseMoveEvent, ObjectFit, ParentElement as _, Pixels, Point, Render, RenderImage,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, Timeout,
    Window, canvas, div, img, prelude::FluentBuilder as _, px,
};

use crate::html;
use crate::theme::colors::rgb;

/// The height of a strip, in CSS pixels: a few per screen, so scrolling asks for one at a
/// time and a long message never sits in memory whole.
const STRIP: f32 = 512.0;

/// Strips kept beyond those on screen, each way: painted ahead of scrolling, and not
/// painted again when scrolling back a little.
const AHEAD: u32 = 1;
const KEPT: u32 = 3;

/// A browser's text size where the page says nothing, in CSS pixels. At the same text
/// size a CSS pixel is one of the window's.
const BROWSER_TEXT_SIZE: f32 = 16.0;

/// How long the engine may take to answer: a message can be built to be slow to lay
/// out. The document is given up then (its thread ends when done), and the page says so.
const PATIENCE: Duration = Duration::from_secs(10);

/// The theme colors a page takes when its message lets it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    background: Hsla,
    foreground: Hsla,
    link: Hsla,
}

impl Look {
    /// The theme's, if the page is shown dark; `None` for a browser's light page.
    fn of(colors: ColorUsage, theme: &Theme) -> Option<Self> {
        shows_dark(colors, theme.is_dark()).then(|| Self {
            background: theme.background,
            foreground: theme.foreground,
            link: theme.link,
        })
    }

    /// For the engine: dark, on the pane's background, the theme's colors under the
    /// message's own.
    fn appearance(self) -> Appearance {
        let sheet = format!(
            "html {{ color: {}; background: {}; }} a {{ color: {}; }}",
            rgb(self.foreground).to_hex(),
            rgb(self.background).to_hex(),
            rgb(self.link).to_hex(),
        );
        let canvas = rgb(self.background);
        Appearance {
            dark: true,
            canvas: [canvas.r, canvas.g, canvas.b],
            sheet: sheet.into(),
        }
    }
}

/// Whether a message's page is dark in a theme: not when it colors itself for white,
/// which dark text on a dark page would make unreadable.
fn shows_dark(colors: ColorUsage, dark_theme: bool) -> bool {
    dark_theme && colors != ColorUsage::Light
}

/// A document as the engine has it, and how it was opened.
struct Opened {
    /// `None` once given up on, or if it couldn't be opened.
    document: Option<Document>,
    /// The window's scale.
    scale: f32,
    zoom: f32,
    look: Option<Look>,
    /// The width it was asked to lay out at, in CSS pixels.
    width: Option<f32>,
    /// Its height at that width, once the engine answered.
    height: Option<f32>,
}

/// A page laid out: its size in CSS pixels, and the window's pixels in one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    width: f32,
    height: f32,
    zoom: f32,
}

/// What an engine answered, or why it didn't.
type Answer<T> = Result<Result<T, Stopped>, Timeout>;

pub(super) struct PageView {
    engine: Engine,
    source: Source,
    colors: ColorUsage,
    opened: Option<Opened>,
    /// The layout shown, and its strips: the document's, or until it has one, the last
    /// document's.
    layout: Option<Layout>,
    /// Where the view was last painted, for the pointer's position on the page.
    bounds: Bounds<Pixels>,
    /// The strips wanted on screen, as last measured.
    wanted: RangeInclusive<u32>,
    strips: HashMap<u32, Arc<RenderImage>>,
    painting: HashMap<u32, Task<()>>,
    /// The `href` of the link under the pointer.
    hovered: Option<SharedString>,
    error: Option<SharedString>,
    _layout: Task<()>,
    _hover: Task<()>,
}

impl PageView {
    pub(super) fn new(
        engine: Engine,
        source: Source,
        colors: ColorUsage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release_in(window, |this, window, _| this.drop_strips(window))
            .detach();
        Self {
            engine,
            source,
            colors,
            opened: None,
            layout: None,
            bounds: Bounds::default(),
            wanted: 0..=0,
            strips: HashMap::new(),
            painting: HashMap::new(),
            hovered: None,
            error: None,
            _layout: Task::ready(()),
            _hover: Task::ready(()),
        }
    }

    /// Shows `source` instead, once it is laid out (the inline images have loaded): the
    /// page as it is stays until then.
    pub(super) fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        self.source = source;
        self.opened = None;
        self._layout = Task::ready(());
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn painted_strips(&self) -> usize {
        self.strips.len()
    }

    /// The painted strips' indices and heights in device pixels, and the height each
    /// should have for the layout shown.
    #[cfg(test)]
    pub(super) fn strip_heights(&self, scale: f32) -> Vec<(u32, u32, u32)> {
        let Some(layout) = self.layout else {
            return Vec::new();
        };
        let mut strips: Vec<_> = (self.strips.iter())
            .map(|(&index, image)| {
                let top = index as f32 * STRIP;
                let wanted = STRIP.min(layout.height - top) * (scale * layout.zoom);
                (index, image.size(0).height.0 as u32, wanted.ceil() as u32)
            })
            .collect();
        strips.sort();
        strips
    }

    /// The height of the page shown, in the window's pixels.
    #[cfg(test)]
    pub(super) fn shown_height(&self) -> Option<f32> {
        self.layout.map(|layout| layout.height * layout.zoom)
    }

    /// The BGRA pixel at (`x`, `y`) device pixels of the top strip, once painted.
    #[cfg(test)]
    pub(super) fn top_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let strip = self.strips.get(&0)?;
        let width = strip.size(0).width.0 as u32;
        let at = ((y * width + x) * 4) as usize;
        strip.as_bytes(0)?.get(at..at + 4)?.try_into().ok()
    }

    fn document(&self) -> Option<&Document> {
        self.opened.as_ref()?.document.as_ref()
    }

    /// From the frame's geometry: opens, lays out and paints what changed.
    fn measure(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.bounds = bounds;
        let scale = window.scale_factor();
        let zoom = f32::from(window.rem_size()) / BROWSER_TEXT_SIZE;
        let look = Look::of(self.colors, cx.theme());
        let current = self.opened.as_ref().is_some_and(|opened| {
            opened.scale == scale && opened.zoom == zoom && opened.look == look
        });
        if !current {
            // Opened again at another scale or text size, or in another theme.
            let appearance = look.map(Look::appearance).unwrap_or_default();
            let opening =
                Document::open(self.engine, self.source.clone(), appearance, scale * zoom);
            let document = match opening {
                Ok(document) => Some(document),
                Err(e) => {
                    let error = format!("the {} thread couldn't start: {e}", self.engine);
                    self.error = Some(error.into());
                    None
                }
            };
            self.opened = Some(Opened {
                document,
                scale,
                zoom,
                look,
                width: None,
                height: None,
            });
            self.painting.clear();
            // The answer coming is the last document's.
            self._layout = Task::ready(());
        }
        // Whole pixels: a fraction would relayout on every subpixel change.
        let width = (f32::from(bounds.size.width) / zoom).floor();
        let Some(opened) = self.opened.as_mut() else {
            return;
        };
        let Some(document) = &opened.document else {
            return;
        };
        if width < 1.0 {
            return;
        }
        if opened.width != Some(width) {
            opened.width = Some(width);
            opened.height = None;
            let laid_out = document.layout(width);
            self.painting.clear();
            self.lay_out(
                laid_out,
                Layout {
                    width,
                    height: 0.0,
                    zoom,
                },
                window,
                cx,
            );
            return;
        }
        // Nothing is painted before the document is laid out at this width: the strips
        // shown until then are of the last layout, and their heights too.
        let Some(height) = opened.height else {
            return;
        };
        let visible = window.content_mask().bounds;
        self.wanted = strips_on_screen(bounds, visible, height, zoom);
        let ahead = self.wanted.start().saturating_sub(AHEAD)..=*self.wanted.end() + AHEAD;
        for index in ahead.filter(|i| i * (STRIP as u32) < height.ceil() as u32) {
            if self.strips.contains_key(&index) || self.painting.contains_key(&index) {
                continue;
            }
            let Some(document) = self.document() else {
                return;
            };
            let top = index as f32 * STRIP;
            let painted = document.paint(width, top, STRIP.min(height - top));
            self.paint(index, painted, window, cx);
        }
    }

    /// Waits for the document's `layout`, which has no height yet.
    fn lay_out(
        &mut self,
        laid_out: impl Future<Output = Result<f32, Stopped>> + 'static,
        layout: Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._layout = cx.spawn_in(window, async move |this, cx| {
            let height = laid_out
                .with_timeout(PATIENCE, cx.background_executor())
                .await;
            this.update_in(cx, |this, window, cx| {
                if let Some(height) = this.answered(height) {
                    // Of the document opened: a new one, or a new width, replaces this task.
                    if let Some(opened) = &mut this.opened {
                        opened.height = Some(height);
                    }
                    // The strips were painted at another width, or of another document.
                    this.drop_strips(window);
                    this.layout = Some(Layout { height, ..layout });
                    this.error = None;
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn paint(
        &mut self,
        index: u32,
        painted: impl Future<Output = Result<Tile, Stopped>> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = cx.spawn_in(window, async move |this, cx| {
            let tile = painted
                .with_timeout(PATIENCE, cx.background_executor())
                .await;
            this.update_in(cx, |this, window, cx| {
                this.painting.remove(&index);
                // Of this layout still: a new one dropped the tasks of the last.
                if let Some(image) = this.answered(tile).and_then(render_image) {
                    this.strips.insert(index, image);
                    this.evict(window);
                }
                cx.notify();
            })
            .ok();
        });
        self.painting.insert(index, task);
    }

    /// The value the engine answered; otherwise says why there is none, and gives the
    /// document up: its thread ends once done, and nothing is asked of it again.
    fn answered<T>(&mut self, answer: Answer<T>) -> Option<T> {
        let error = match answer {
            Ok(Ok(value)) => return Some(value),
            Ok(Err(stopped)) => stopped.to_string(),
            Err(Timeout) => format!("{} took too long to lay out the message", self.engine),
        };
        if let Some(opened) = &mut self.opened {
            opened.document = None;
        }
        self.painting.clear();
        self.error = Some(error.into());
        None
    }

    /// Drops the strips far from the screen. Not while painting a frame: one dropped
    /// from the atlas then would be put back by the image element already built for it.
    fn evict(&mut self, window: &mut Window) {
        let kept = self.wanted.start().saturating_sub(KEPT)..=*self.wanted.end() + KEPT;
        let far: Vec<u32> = self
            .strips
            .keys()
            .copied()
            .filter(|i| !kept.contains(i))
            .collect();
        for index in far {
            if let Some(image) = self.strips.remove(&index) {
                window.drop_image(image).ok();
            }
        }
    }

    fn drop_strips(&mut self, window: &mut Window) {
        for (_, image) in self.strips.drain() {
            window.drop_image(image).ok();
        }
    }

    /// The link at the pointer's position, if it is on the document's page laid out.
    fn link_at(
        &self,
        position: Point<Pixels>,
    ) -> Option<impl Future<Output = Result<Option<String>, Stopped>> + use<>> {
        let opened = self.opened.as_ref()?;
        opened.height?;
        let at = position - self.bounds.origin;
        let (x, y) = (f32::from(at.x), f32::from(at.y));
        let document = opened.document.as_ref()?;
        Some(document.link_at(opened.width?, x / opened.zoom, y / opened.zoom))
    }

    fn hover(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.link_at(position) else {
            return;
        };
        // Replacing the task drops the question for where the pointer was before.
        self._hover = cx.spawn_in(window, async move |this, cx| {
            let Ok(link) = link.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let link = link.map(SharedString::from);
                if this.hovered != link {
                    this.hovered = link;
                    cx.notify();
                }
            })
            .ok();
        });
    }

    fn click(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.link_at(event.position()) else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Some(url)) = link.await
                && html::is_openable_link(&url)
            {
                cx.update(|_, cx| cx.open_url(&url)).ok();
            }
        })
        .detach();
    }
}

/// The strips the visible part of the page falls in, by index from the top. `height` is
/// the page's in CSS pixels, each `zoom` of the window's.
fn strips_on_screen(
    page: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    height: f32,
    zoom: f32,
) -> RangeInclusive<u32> {
    let top = (f32::from(visible.top() - page.top()) / zoom).clamp(0.0, height);
    let bottom = (f32::from(visible.bottom() - page.top()) / zoom).clamp(top, height);
    let last = (bottom - 1.0).max(top);
    (top / STRIP) as u32..=(last / STRIP) as u32
}

/// A strip as GPUI draws it. The engine already wrote the BGRA it wants.
fn render_image(tile: Tile) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(tile.width, tile.height, tile.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl Render for PageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let measure = canvas(
            move |bounds, window, cx: &mut App| {
                view.update(cx, |this, cx| this.measure(bounds, window, cx));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        // A page given up on says why instead.
        let Some(layout) = self.layout.filter(|_| self.error.is_none()) else {
            return div()
                .id("page")
                .relative()
                .w_full()
                .h_32()
                .child(measure)
                .child(match &self.error {
                    Some(error) => div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Couldn't show the message: {error}"))
                        .into_any_element(),
                    None => h_flex()
                        .size_full()
                        .justify_center()
                        .child(Spinner::new())
                        .into_any_element(),
                });
        };
        let Layout {
            width,
            height,
            zoom,
        } = layout;
        let strips = self.strips.iter().map(|(&index, image)| {
            let top = index as f32 * STRIP;
            img(image.clone())
                .absolute()
                .left_0()
                .top(px(top * zoom))
                .w(px(width * zoom))
                .h(px(STRIP.min(height - top) * zoom))
                // A device pixel's rounding of the width must not letterbox the strip.
                .object_fit(ObjectFit::Fill)
        });
        div()
            .id("page")
            .relative()
            .w_full()
            .h(px(height * zoom))
            .child(measure)
            .children(strips)
            .when(self.hovered.is_some(), |this| this.cursor_pointer())
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.hover(event.position, window, cx)
            }))
            .on_click(cx.listener(|this, event, window, cx| this.click(event, window, cx)))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, TestAppContext, WindowBounds, WindowOptions,
        point, size,
    };

    use super::*;
    use crate::settings::Preferences;
    use crate::test_support::wait_until;

    /// A page `height` CSS pixels tall.
    fn source(height: u32) -> Source {
        Source {
            html: format!(r#"<body style="margin:0"><div style="height:{height}px"></div>"#).into(),
            images: Arc::new(|_| None),
        }
    }

    /// A window 800 px tall showing the page of `source` whole.
    fn open(cx: &mut TestAppContext, source: Source) -> (AnyWindowHandle, Entity<PageView>) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::init_app(Preferences::load_nowhere(), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(600.), px(800.)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| PageView::new(Engine::Blitz, source, ColorUsage::Unset, window, cx))
            })
            .unwrap()
        })
    }

    /// Draws a frame, then lets the tasks it started run if `settle`.
    fn frame(cx: &mut TestAppContext, window: AnyWindowHandle, settle: bool) -> f32 {
        let scale = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.scale_factor()
            })
            .unwrap();
        if settle {
            cx.run_until_parked();
        }
        scale
    }

    #[gpui_kit::test]
    fn a_new_source_is_painted_once_it_is_laid_out(cx: &mut TestAppContext) {
        // 700 px: a whole strip and one of 188 px.
        let (window, view) = open(cx, source(700));
        wait_until(|| {
            frame(cx, window, true);
            cx.read(|cx| view.read(cx).layout.is_some())
        });
        // The strips are asked for, and the source changes before they come: at the same
        // width, the layout of the last one is no answer for the new one.
        frame(cx, window, false);
        cx.update(|cx| view.update(cx, |view, cx| view.set_source(source(1500), cx)));
        frame(cx, window, false);
        wait_until(|| {
            let scale = frame(cx, window, true);
            let strips = cx.read(|cx| view.read(cx).strip_heights(scale));
            if strips.len() < 3 {
                return false;
            }
            for (index, height, wanted) in strips {
                assert_eq!(height, wanted, "strip {index}");
            }
            true
        });
    }

    #[gpui_kit::test]
    fn the_page_zooms_with_the_text_size(cx: &mut TestAppContext) {
        let (window, view) = open(cx, source(700));
        let shown = |cx: &mut TestAppContext| {
            frame(cx, window, true);
            cx.read(|cx| view.read(cx).shown_height())
        };
        wait_until(|| shown(cx) == Some(700.));
        cx.update(|cx| Preferences::update(cx, |s| s.text_size = 18.));
        wait_until(|| shown(cx) == Some(700. * 18. / 16.));
        let scale = frame(cx, window, true);
        wait_until(|| {
            frame(cx, window, true);
            cx.read(|cx| view.read(cx).painted_strips()) == 2
        });
        for (index, height, wanted) in cx.read(|cx| view.read(cx).strip_heights(scale)) {
            assert_eq!(height, wanted, "strip {index}");
        }
    }

    fn bounds(top: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(top)), size(px(600.), px(height)))
    }

    #[test]
    fn a_page_is_dark_only_if_its_message_allows_it() {
        for colors in [ColorUsage::Unset, ColorUsage::Light, ColorUsage::Adaptive] {
            assert!(!shows_dark(colors, false), "{colors:?}");
        }
        assert!(shows_dark(ColorUsage::Unset, true));
        assert!(shows_dark(ColorUsage::Adaptive, true));
        assert!(!shows_dark(ColorUsage::Light, true));
    }

    #[test]
    fn asks_for_the_strips_on_screen() {
        // The page starts 200 px down a 700 px pane.
        let page = bounds(200., 5000.);
        assert_eq!(strips_on_screen(page, bounds(0., 700.), 5000., 1.), 0..=0);
        // Scrolled 1000 px: 800..1500 of the page is on screen.
        let page = bounds(-800., 5000.);
        assert_eq!(strips_on_screen(page, bounds(0., 700.), 5000., 1.), 1..=2);
        // Zoomed twice: 400..750 of the page.
        assert_eq!(strips_on_screen(page, bounds(0., 700.), 5000., 2.), 0..=1);
        // A short page ends inside the first strip, whatever the pane's height.
        let page = bounds(0., 300.);
        assert_eq!(strips_on_screen(page, bounds(0., 2000.), 300., 1.), 0..=0);
    }
}
