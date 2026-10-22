//! A message's HTML as an engine of `catmail-render` lays it out: the page painted in
//! strips on the engine's thread and shown as images.
//!
//! The view measures itself while painting (the pane's width decides the layout, the
//! scroll position which strips are needed) and asks the engine only when one of those
//! changed. Strips are dropped from the window's sprite atlas when they are evicted or
//! the view is released; GPUI keeps an image there until told otherwise.
//!
//! A new layout (another width, theme or zoom, images that came) replaces the page shown
//! only once the strips on screen are painted for it, all in one frame, so the page never
//! goes blank or shows strips of two layouts at once.
//!
//! In a dark theme the page is dark too when the message allows it ([`Look`]): it sets
//! no colors, or has dark styles of its own. A message colored for a white page keeps
//! its white, as a browser would show it.
//!
//! The page zooms with the window's text size, as a browser page zooms: everything on it,
//! since most mail sets its own font sizes in pixels.
//!
//! Its text takes part in the window's text selection, as GPUI Kit's text views do: the
//! window keeps the two ends a drag left on the page, and copies what the page says is
//! between them. The engine finds that text, and where it is; the view draws the theme's
//! highlight over the strips, so a selection never paints the page again. A double or
//! triple click selects a word or a paragraph, which the window can't find in a page, and
//! Select all the whole page: the page keeps those selections itself.

use std::collections::HashMap;
use std::future::Future;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use catmail_core::html::ColorUsage;
use catmail_render::{
    Appearance, Document, Engine, Hit, Rect, Resolver, SelectionEnd, Source, Span, Stopped, Tile,
    Unit,
};
use gpui_kit::base::input::{Copy, SelectAll};
use gpui_kit::base::{
    TextSelection, TextSelectionCoverage, TextSelectionEndpoint, TextSelectionEvent,
    TextSelectionHandle, TextSelectionRegistration, TextSelectionSnapshot,
};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Theme, h_flex};
use gpui_kit::{
    App, Bounds, ClickEvent, ClipboardItem, Context, EntityId, FocusHandle, FutureExt as _,
    HitboxBehavior, Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ObjectFit, ParentElement as _, Pixels, Point, Render, RenderImage,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription,
    Task, TestSupportExt as _, Timeout, Window, canvas, div, fill, img, point,
    prelude::FluentBuilder as _, px, size,
};

use crate::theme::colors::rgb;
use crate::{actions, html};

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
    /// Images given since that layout was asked for, kept from the document until its
    /// next: given at once, they would reflow it under the strips of the layout coming.
    images: Option<Resolver>,
}

/// A layout waiting for its strips on screen, to replace the page shown at once.
struct Next {
    layout: Layout,
    strips: HashMap<u32, Arc<RenderImage>>,
}

/// A page laid out: its size in CSS pixels, and the window's pixels in one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    width: f32,
    height: f32,
    zoom: f32,
    /// Which of the view's layouts it is: two of the same size can still place the text
    /// differently (an image came, in a box of fixed height).
    number: u64,
}

/// What an engine answered, or why it didn't.
type Answer<T> = Result<Result<T, Stopped>, Timeout>;

/// The span of the window's selection on the page `me`, in the window's pixels from its
/// top left: a selection that starts or ends elsewhere takes the page from its start or
/// to its end.
fn span_of(snapshot: &TextSelectionSnapshot, me: EntityId) -> Option<Span> {
    let at = |end: TextSelectionEndpoint| {
        let point = end.content_point();
        (end.entity_id() == Some(me)).then(|| SelectionEnd::At(point.x.into(), point.y.into()))
    };
    let (anchor, cursor) = (at(snapshot.anchor()), at(snapshot.cursor()));
    let (from, to) = match snapshot.coverage() {
        TextSelectionCoverage::Bounded => (anchor?, cursor?),
        TextSelectionCoverage::FromStart => (SelectionEnd::Start, anchor.or(cursor)?),
        TextSelectionCoverage::ToEnd => (anchor.or(cursor)?, SelectionEnd::End),
        TextSelectionCoverage::Full => (SelectionEnd::Start, SelectionEnd::End),
    };
    Some(Span::Between(from, to))
}

/// What the page has selected: a drag's span (the window's selection), or a double or
/// triple click's, or all of it (the page's own).
struct Selecting {
    /// In the window's pixels from the page's top left.
    span: Span,
    /// The layout shown when the engine was last asked: another one asks again.
    asked: Option<Layout>,
    /// What the engine found.
    found: Option<Found>,
}

/// What the engine found selected, at a layout.
struct Found {
    layout: Option<Layout>,
    rects: Arc<[Rect]>,
    text: String,
}

pub(super) struct PageView {
    engine: Engine,
    source: Source,
    colors: ColorUsage,
    opened: Option<Opened>,
    /// The layout shown, and its strips: the document's, or the last one's until the
    /// strips on screen are painted for the next.
    layout: Option<Layout>,
    /// How many layouts the view asked for.
    layouts: u64,
    /// Where the view was last painted, for the pointer's position on the page.
    bounds: Bounds<Pixels>,
    /// The strips wanted on screen, as last measured.
    wanted: RangeInclusive<u32>,
    strips: HashMap<u32, Arc<RenderImage>>,
    next: Option<Next>,
    /// The strips being painted: the next layout's if there is one, else the shown one's.
    painting: HashMap<u32, Task<()>>,
    /// What is under the pointer.
    hovered: Hit,
    error: Option<SharedString>,
    /// The page's part in the window's text selection.
    selection: TextSelectionHandle,
    /// Taken when a selection starts on the page, so that the keys copy it.
    focus: FocusHandle,
    selecting: Option<Selecting>,
    _layout: Task<()>,
    _hover: Task<()>,
    _select: Task<()>,
    _selection_changes: Subscription,
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
        let selection = TextSelectionHandle::new("", cx);
        let focus = cx.focus_handle();
        selection.focus_with(
            {
                let focus = focus.clone();
                move |window, cx| focus.focus(window, cx)
            },
            cx,
        );
        let this = cx.weak_entity();
        selection.copy_with(
            {
                let this = this.clone();
                move |cx| {
                    let text = this
                        .upgrade()
                        .and_then(|this| this.read(cx).selected_text().map(str::to_owned));
                    text.unwrap_or_default()
                }
            },
            cx,
        );
        let selection_changes = selection.subscribe(
            move |event, cx| {
                this.update(cx, |this, cx| this.selection_changed(event, cx))
                    .ok();
            },
            cx,
        );
        Self {
            engine,
            source,
            colors,
            opened: None,
            layout: None,
            layouts: 0,
            bounds: Bounds::default(),
            wanted: 0..=0,
            strips: HashMap::new(),
            next: None,
            painting: HashMap::new(),
            hovered: Hit::default(),
            error: None,
            selection,
            focus,
            selecting: None,
            _layout: Task::ready(()),
            _hover: Task::ready(()),
            _select: Task::ready(()),
            _selection_changes: selection_changes,
        }
    }

    /// Resolves the page's images with `images` from now on: images that came (inline
    /// parts, remote images) go to the open document with its next layout, which it
    /// makes without starting over; the page as it is stays until then.
    pub(super) fn set_images(&mut self, images: Resolver, cx: &mut Context<Self>) {
        self.source.images = images.clone();
        if let Some(opened) = &mut self.opened
            && opened.document.is_some()
        {
            opened.images = Some(images);
            cx.notify();
        }
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

    /// The text selected on the page, once the engine found it for the layout shown.
    pub(super) fn selected_text(&self) -> Option<&str> {
        self.found().map(|found| found.text.as_str())
    }

    /// Where the selection is highlighted on the page, in CSS pixels.
    #[cfg(test)]
    pub(super) fn highlighted(&self) -> Option<&[Rect]> {
        self.found().map(|found| &*found.rects)
    }

    /// What the engine found selected, if it is for the layout shown.
    fn found(&self) -> Option<&Found> {
        let found = self.selecting.as_ref()?.found.as_ref()?;
        (found.layout == self.layout).then_some(found)
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
                images: None,
            });
            self.painting.clear();
            self.next = None;
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
        // Images that came are laid out once the last layout is shown, so a stream of
        // them can't keep the page from ever changing.
        let refresh = opened.images.is_some() && opened.height.is_some() && self.next.is_none();
        if opened.width != Some(width) || refresh {
            opened.width = Some(width);
            opened.height = None;
            if let Some(images) = opened.images.take() {
                document.set_images(images);
            }
            let laid_out = document.layout(width);
            self.painting.clear();
            self.next = None;
            self.layouts += 1;
            self.lay_out(
                laid_out,
                Layout {
                    width,
                    height: 0.0,
                    zoom,
                    number: self.layouts,
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
        // The next layout needs only the strips on screen before it is shown.
        let ahead = if self.next.is_some() { 0 } else { AHEAD };
        let ahead = self.wanted.start().saturating_sub(ahead)..=*self.wanted.end() + ahead;
        for index in ahead.filter(|i| i * (STRIP as u32) < height.ceil() as u32) {
            let strips = self.next.as_ref().map_or(&self.strips, |next| &next.strips);
            if strips.contains_key(&index) || self.painting.contains_key(&index) {
                continue;
            }
            let Some(document) = self.document() else {
                return;
            };
            let top = index as f32 * STRIP;
            let painted = document.paint(width, top, STRIP.min(height - top));
            self.paint(index, painted, window, cx);
        }
        // A selection is found again in a new layout once it is shown.
        if self.next.is_none()
            && let Some(selecting) = &self.selecting
            && selecting.asked != self.layout
        {
            self.select(cx);
        }
        // One with no strip to paint on screen (an empty page, or scrolled past its end)
        // is shown as it is, once the frame is drawn.
        if self.next_ready() {
            cx.defer_in(window, |this, window, cx| {
                this.show_next(window);
                cx.notify();
            });
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
            this.update(cx, |this, cx| {
                if let Some(height) = this.answered(height) {
                    // Of the document opened: a new one, or a new width, replaces this task.
                    if let Some(opened) = &mut this.opened {
                        opened.height = Some(height);
                    }
                    let layout = Layout { height, ..layout };
                    if this.layout.is_some() {
                        // Shown once painted; one that wasn't yet is replaced, never drawn.
                        this.next = Some(Next {
                            layout,
                            strips: HashMap::new(),
                        });
                    } else {
                        this.layout = Some(layout);
                    }
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
                    match &mut this.next {
                        Some(next) => {
                            next.strips.insert(index, image);
                            this.show_next(window);
                        }
                        None => {
                            this.strips.insert(index, image);
                            this.evict(window);
                        }
                    }
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
        self.next = None;
        self.error = Some(error.into());
        None
    }

    /// Shows the next layout in place of the page if the strips on screen are painted for
    /// it. Not while painting a frame, as for [`evict`](Self::evict).
    fn show_next(&mut self, window: &mut Window) {
        let ready = self.next_ready();
        let Some(next) = self.next.take_if(|_| ready) else {
            return;
        };
        self.drop_strips(window);
        self.strips = next.strips;
        self.layout = Some(next.layout);
    }

    /// Whether there is a next layout, with the strips on screen painted for it.
    fn next_ready(&self) -> bool {
        self.next.as_ref().is_some_and(|next| {
            let height = next.layout.height.ceil() as u32;
            (self.wanted.clone())
                .filter(|i| i * (STRIP as u32) < height)
                .all(|i| next.strips.contains_key(&i))
        })
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

    /// What is at the pointer's position, if it is on the document's page laid out.
    fn hit(
        &self,
        position: Point<Pixels>,
    ) -> Option<impl Future<Output = Result<Hit, Stopped>> + use<>> {
        let opened = self.opened.as_ref()?;
        opened.height?;
        let at = position - self.bounds.origin;
        let (x, y) = (f32::from(at.x), f32::from(at.y));
        let document = opened.document.as_ref()?;
        Some(document.hit(opened.width?, x / opened.zoom, y / opened.zoom))
    }

    fn hover(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.hit(position) else {
            return;
        };
        // Replacing the task drops the question for where the pointer was before.
        self._hover = cx.spawn_in(window, async move |this, cx| {
            let Ok(hit) = hit.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.hovered != hit {
                    this.hovered = hit;
                    cx.notify();
                }
            })
            .ok();
        });
    }

    fn click(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The end of a drag that selected text on the page, not a click on what it ended on.
        if self.selection.snapshot(cx).is_some() || self.selection.has_local_selection(cx) {
            return;
        }
        let Some(hit) = self.hit(event.position()) else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Hit {
                link: Some(url), ..
            }) = hit.await
                && html::is_openable_link(&url)
            {
                cx.update(|_, cx| cx.open_url(&url)).ok();
            }
        })
        .detach();
    }

    /// Follows the window's selection: the span it has on the page, or none.
    fn selection_changed(&mut self, event: &TextSelectionEvent, cx: &mut Context<Self>) {
        let span = match event {
            TextSelectionEvent::SelectionChanged(Some(snapshot)) => {
                span_of(snapshot, self.selection.entity_id())
            }
            TextSelectionEvent::SelectionChanged(None) | TextSelectionEvent::Cleared => None,
            _ => return,
        };
        let Some(span) = span else {
            // A click clears the selection before the page makes its own on it: the
            // news comes after, and is not about the page's own.
            if self.selection.has_local_selection(cx) {
                return;
            }
            if self.selecting.take().is_some() {
                self._select = Task::ready(());
                cx.notify();
            }
            return;
        };
        // A drag running on past the page moves only its other end.
        if let Some(selecting) = &self.selecting
            && selecting.span == span
            && selecting.asked == self.layout
        {
            return;
        }
        // The highlight stays until the engine answers for the new span.
        let found = self.selecting.take().and_then(|selecting| selecting.found);
        self.selecting = Some(Selecting {
            span,
            asked: None,
            found,
        });
        self.select(cx);
    }

    /// Selects the word under a double click, or the paragraph under a triple one: the
    /// window's selection has no words or paragraphs in a page.
    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let unit = match event.click_count {
            0 | 1 => return,
            2 => Unit::Word,
            _ => Unit::Paragraph,
        };
        let at = event.position - self.bounds.origin;
        self.select_locally(Span::Around(at.x.into(), at.y.into(), unit), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        // A drag's selection goes, as a click would clear it.
        TextSelection::clear(window, cx);
        let all = Span::Between(SelectionEnd::Start, SelectionEnd::End);
        self.select_locally(all, window, cx);
    }

    /// Makes `span` the page's own selection, which the window copies with the rest.
    fn select_locally(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = Some(Selecting {
            span,
            asked: None,
            found: None,
        });
        self.selection.set_local_selection(true, cx);
        self.focus.focus(window, cx);
        self.select(cx);
        cx.notify();
    }

    /// Asks the engine for what is selected, in the layout it has.
    fn select(&mut self, cx: &mut Context<Self>) {
        // With a layout coming the engine would answer for it, not for the one shown:
        // `measure` asks again once it is shown.
        if self.next.is_some() || self.opened.as_ref().is_none_or(|o| o.height.is_none()) {
            return;
        }
        let layout = self.layout;
        let (Some(selecting), Some(opened)) = (&mut self.selecting, &self.opened) else {
            return;
        };
        let (Some(document), Some(width)) = (&opened.document, opened.width) else {
            return;
        };
        selecting.asked = layout;
        let selection = document.select(width, selecting.span.scaled_down(opened.zoom));
        // Replacing the task drops the question for the span the selection had before.
        self._select = cx.spawn(async move |this, cx| {
            let selection = selection
                .with_timeout(PATIENCE, cx.background_executor())
                .await;
            this.update(cx, |this, cx| {
                if let Some(selection) = this.answered(selection)
                    && let Some(selecting) = &mut this.selecting
                {
                    selecting.found = Some(Found {
                        layout,
                        rects: selection.rects.into(),
                        text: selection.text,
                    });
                }
                cx.notify();
            })
            .ok();
        });
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

/// Paints the rectangles of a selection, in CSS pixels on a page drawn in `bounds`, that
/// are on screen.
fn paint_highlight(
    rects: &[Rect],
    zoom: f32,
    bounds: Bounds<Pixels>,
    color: Hsla,
    window: &mut Window,
) {
    let visible = window.content_mask().bounds;
    for rect in rects {
        let rect = Bounds::new(
            bounds.origin + point(px(rect.x * zoom), px(rect.y * zoom)),
            size(px(rect.width * zoom), px(rect.height * zoom)),
        );
        if rect.intersects(&visible) {
            window.paint_quad(fill(rect, color));
        }
    }
}

/// A strip as GPUI draws it. The engine already wrote the BGRA it wants.
fn render_image(tile: Tile) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(tile.width, tile.height, tile.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl Render for PageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.layout.filter(|_| self.error.is_none());
        let view = cx.entity();
        // Only a page shown takes part in the selection.
        let selection = shown.map(|_| self.selection.clone());
        let registered = selection.clone();
        let measure = canvas(
            move |bounds, window, cx: &mut App| {
                view.update(cx, |this, cx| this.measure(bounds, window, cx));
                let Some(selection) = &registered else {
                    return;
                };
                // All of the page takes part, its margins too: a drag there selects the
                // text nearest the pointer, as in a browser.
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                let registration = TextSelectionRegistration::new(hitbox, bounds)
                    .with_text_bounds(vec![bounds])
                    .with_rendered_element(selection, window, cx);
                selection.register(registration, window, cx);
            },
            move |_, _, window, _| {
                let Some(selection) = selection else {
                    return;
                };
                // A press anywhere clears the page's own selection, as it clears the
                // window's. The window doesn't, the press after a menu item: the item
                // keeps the release from it, and it waits for one before clearing again.
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase.capture()
                        && event.button == MouseButton::Left
                        && selection.has_local_selection(cx)
                    {
                        TextSelection::clear(window, cx);
                    }
                });
            },
        )
        .absolute()
        .size_full();

        // A page given up on says why instead.
        let Some(layout) = shown else {
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
                })
                .into_any_element();
        };
        let Layout {
            width,
            height,
            zoom,
            ..
        } = layout;
        // Over the strips, which are opaque: the theme's selection color lets them show.
        let highlight = self.found().map(|found| {
            let (rects, color) = (found.rects.clone(), cx.theme().selection);
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| paint_highlight(&rects, zoom, bounds, color, window),
            )
            .absolute()
            .size_full()
        });
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
            .test_support()
            .key_context(actions::READER_PAGE)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_all))
            .relative()
            .w_full()
            .h(px(height * zoom))
            .child(measure)
            .children(strips)
            .children(highlight)
            .map(|this| match &self.hovered {
                Hit { link: Some(_), .. } => this.cursor_pointer(),
                Hit { text: true, .. } => this.cursor_text(),
                _ => this,
            })
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.hover(event.position, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.press(event, window, cx)),
            )
            .on_click(cx.listener(|this, event, window, cx| this.click(event, window, cx)))
            .context_menu({
                let focus = self.focus.clone();
                move |menu, window, cx| {
                    // What the keys would copy, as the menu opens: pressing the menu clears
                    // the window's selection before the item runs.
                    let text = TextSelection::selected_text(window, cx).trim().to_owned();
                    let text = (!text.is_empty()).then_some(text);
                    // The action only shows the keys that copy.
                    let copy = PopupMenuItem::new("Copy")
                        .action(Box::new(Copy))
                        .disabled(text.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(text) = &text {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                            }
                        });
                    menu.action_context(focus.clone())
                        .item(copy)
                        .menu("Select all", Box::new(SelectAll))
                }
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::test::{ClickOptions, TestEventExt as _, TestWindowExt as _};
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, TestAppContext, WindowBounds, WindowOptions,
    };

    use super::*;
    use crate::settings::Preferences;
    use crate::test_support::wait_until;

    /// A page of `html`, with no images to show yet.
    fn page(html: &str) -> Source {
        Source {
            html: html.into(),
            images: Arc::new(|_| None),
        }
    }

    /// A page `height` CSS pixels tall.
    fn source(height: u32) -> Source {
        page(&format!(
            r#"<body style="margin:0"><div style="height:{height}px"></div>"#
        ))
    }

    /// Draws frames until the page shows `strips` strips.
    fn painted(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        view: &Entity<PageView>,
        strips: usize,
    ) {
        wait_until(|| {
            frame(cx, window, true);
            cx.read(|cx| view.read(cx).painted_strips()) == strips
        });
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

    const RED: [u8; 3] = [255, 0, 0];
    const BLUE: [u8; 3] = [0, 0, 255];

    /// An image 40 x 400 px.
    fn png(rgb: [u8; 3]) -> catmail_render::Bytes {
        crate::test_support::solid_png(40, 400, rgb).into()
    }

    /// The BGRA of `rgb`.
    fn bgra([r, g, b]: [u8; 3]) -> Option<[u8; 4]> {
        Some([b, g, r, 255])
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
    fn a_selection_follows_text_an_image_moves(cx: &mut TestAppContext) {
        // The image, once it comes, pushes the text down 100 px, in a box that keeps the
        // page's size.
        let html = r#"<body style="margin:0"><div style="height:300px">
            <img src="cid:image" style="display:block"><p style="margin:0">text</p></div>"#;
        let (window, view) = open(cx, page(html));
        painted(cx, window, &view, 1);
        let top = |cx: &mut TestAppContext| {
            frame(cx, window, true);
            cx.read(|cx| Some(view.read(cx).highlighted()?.first()?.y))
        };
        // The keys are the page's once it is clicked.
        click(cx, window, point(px(5.), px(5.)), MouseButton::Left, 1);
        cx.update_window(window, |_, window, cx| window.press("secondary-a", cx))
            .unwrap();
        wait_until(|| top(cx) == Some(0.));

        let image: catmail_render::Bytes = crate::test_support::solid_png(40, 100, RED).into();
        let images: Resolver = Arc::new(move |url| (url == "cid:image").then(|| image.clone()));
        cx.update(|cx| view.update(cx, |view, cx| view.set_images(images, cx)));
        wait_until(|| top(cx) == Some(100.));
        assert_eq!(cx.read(|cx| view.read(cx).shown_height()), Some(300.));
    }

    #[gpui_kit::test]
    fn images_that_come_replace_the_page_without_a_blank(cx: &mut TestAppContext) {
        // 700 px, and an image with no size of its own: 400 px more once it comes.
        let html = r#"<body style="margin:0"><img src="cid:tall" style="display:block">
            <div style="height:700px"></div>"#;
        let (window, view) = open(cx, page(html));
        painted(cx, window, &view, 2);

        let red = png(RED);
        let images: Resolver = Arc::new(move |url| (url == "cid:tall").then(|| red.clone()));
        cx.update(|cx| view.update(cx, |view, cx| view.set_images(images, cx)));
        // Every frame shows one layout whole, the last until the next is painted.
        wait_until(|| {
            let scale = frame(cx, window, true);
            let (strips, height) = cx.read(|cx| {
                let view = view.read(cx);
                (view.strip_heights(scale), view.shown_height())
            });
            assert!(!strips.is_empty(), "the page went blank");
            for (index, height, wanted) in strips {
                assert_eq!(height, wanted, "strip {index}");
            }
            height == Some(1100.)
        });
        let pixel = cx.read(|cx| view.read(cx).top_pixel(4, 4));
        assert_eq!(pixel, bgra(RED));
    }

    #[gpui_kit::test]
    fn images_that_come_while_a_layout_is_made_go_to_the_next(cx: &mut TestAppContext) {
        // Each image, once it comes, pushes the page 400 px down; the blue one goes on top.
        let html = r#"<body style="margin:0"><img src="cid:blue" style="display:block">
            <img src="cid:red" style="display:block"><div style="height:700px"></div>"#;
        let (window, view) = open(cx, page(html));
        painted(cx, window, &view, 2);

        let red = png(RED);
        let images: Resolver = Arc::new(move |url| (url == "cid:red").then(|| red.clone()));
        cx.update(|cx| view.update(cx, |view, cx| view.set_images(images, cx)));
        // Asks for the layout with the red one; the blue one comes before it is made.
        frame(cx, window, false);
        let (red, blue) = (png(RED), png(BLUE));
        let images: Resolver = Arc::new(move |url| match url {
            "cid:red" => Some(red.clone()),
            "cid:blue" => Some(blue.clone()),
            _ => None,
        });
        cx.update(|cx| view.update(cx, |view, cx| view.set_images(images, cx)));

        // The layout with the red one shows as it was laid out, then the next with both.
        let mut shown = Vec::new();
        wait_until(|| {
            frame(cx, window, true);
            let (height, pixel) = cx.read(|cx| {
                let view = view.read(cx);
                (view.shown_height(), view.top_pixel(4, 4))
            });
            if shown.last() != Some(&(height, pixel)) {
                shown.push((height, pixel));
            }
            height == Some(1500.) && pixel == bgra(BLUE)
        });
        let white = Some([255; 4]);
        assert_eq!(
            shown,
            [
                (Some(700.), white),
                (Some(1100.), bgra(RED)),
                (Some(1500.), bgra(BLUE))
            ]
        );
    }

    #[gpui_kit::test]
    fn a_page_with_nothing_to_paint_replaces_the_last(cx: &mut TestAppContext) {
        // Wider than the window's 600 px, as a smaller text size makes it, there is no page.
        let html = r#"<style>@media (min-width: 700px) { html { display: none } }</style>
            <body style="margin:0"><div style="height:700px"></div>"#;
        let (window, view) = open(cx, page(html));
        let shown = |cx: &mut TestAppContext| {
            frame(cx, window, true);
            cx.read(|cx| view.read(cx).shown_height())
        };
        wait_until(|| shown(cx) == Some(700.));
        cx.update(|cx| Preferences::update(cx, |s| s.text_size = 12.));
        wait_until(|| shown(cx) == Some(0.));
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
        painted(cx, window, &view, 2);
        for (index, height, wanted) in cx.read(|cx| view.read(cx).strip_heights(scale)) {
            assert_eq!(height, wanted, "strip {index}");
        }
    }

    /// Clicks the page `offset` from its top left with `button`, the `count`th click in a
    /// row.
    fn click(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        offset: Point<Pixels>,
        button: MouseButton,
        count: usize,
    ) {
        let options = ClickOptions::new()
            .with_offset(offset)
            .with_button(button)
            .with_count(count);
        cx.update_window(window, |_, window, cx| {
            window.click_with_options("page", options, cx)
        })
        .unwrap();
    }

    /// Opens the page's context menu and clicks its `item`th item.
    fn menu(cx: &mut TestAppContext, window: AnyWindowHandle, item: usize) {
        click(cx, window, point(px(40.), px(15.)), MouseButton::Right, 1);
        frame(cx, window, true);
        cx.update_window(window, |_, window, cx| {
            window.within("popup-menu").click(item, cx)
        })
        .unwrap();
        cx.run_until_parked();
    }

    /// Presses `keys`, and returns what is on the clipboard then.
    fn copied(cx: &mut TestAppContext, window: AnyWindowHandle, keys: &str) -> Option<String> {
        cx.update_window(window, |_, window, cx| window.press(keys, cx))
            .unwrap();
        cx.read_from_clipboard().and_then(|item| item.text())
    }

    #[gpui_kit::test]
    fn text_is_selected_and_copied(cx: &mut TestAppContext) {
        // Two paragraphs a line each, 30 px apart, the second a link. Clicks land at the
        // start of a line: where a word ends depends on the fonts installed.
        let html = r#"<body style="margin:0;font-size:20px;line-height:30px">
            <p style="margin:0 0 30px">alpha beta</p>
            <p style="margin:0"><a href="https://example.com/">gamma delta</a></p>"#;
        let (window, view) = open(cx, page(html));
        painted(cx, window, &view, 1);
        let selected = |cx: &mut TestAppContext| {
            frame(cx, window, true);
            cx.read(|cx| view.read(cx).selected_text().map(str::to_owned))
        };
        let all = Some("alpha beta\ngamma delta".to_owned());
        let left = MouseButton::Left;

        // A drag from the first letter to the link selects; it doesn't follow the link.
        cx.update_window(window, |_, window, cx| {
            window.drag(point(px(1.), px(15.)), point(px(500.), px(75.)), cx)
        })
        .unwrap();
        wait_until(|| selected(cx) == all);
        assert_eq!(cx.opened_url(), None);
        assert_eq!(copied(cx, window, "secondary-c"), all);

        // The context menu's Copy copies it too; the press on the menu clears it.
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        menu(cx, window, 0);
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), all);
        assert_eq!(selected(cx), None);

        // A double click selects a word, a triple click its paragraph, and a click clears.
        click(cx, window, point(px(5.), px(15.)), left, 2);
        wait_until(|| selected(cx).as_deref() == Some("alpha"));
        click(cx, window, point(px(5.), px(15.)), left, 3);
        wait_until(|| selected(cx).as_deref() == Some("alpha beta"));
        assert_eq!(
            copied(cx, window, "secondary-c").as_deref(),
            Some("alpha beta")
        );
        click(cx, window, point(px(5.), px(15.)), left, 1);
        assert_eq!(selected(cx), None);
        assert_eq!(cx.opened_url(), None);

        // Select all, from the context menu with nothing selected (Copy is there, off),
        // or with the keys. A press anywhere clears it, the first after the menu too.
        menu(cx, window, 1);
        wait_until(|| selected(cx) == all);
        let outside = point(px(5.), px(400.));
        cx.update_window(window, |_, window, cx| {
            window.pointer_down(outside, left, cx);
            window.pointer_up(outside, left, cx);
        })
        .unwrap();
        assert_eq!(selected(cx), None);
        cx.update_window(window, |_, window, cx| window.press("secondary-a", cx))
            .unwrap();
        wait_until(|| selected(cx) == all);
        assert_eq!(copied(cx, window, "secondary-c"), all);
        click(cx, window, point(px(5.), px(15.)), left, 1);
        assert_eq!(selected(cx), None);

        // A click on the link, with nothing selected, follows it.
        click(cx, window, point(px(5.), px(75.)), left, 1);
        wait_until(|| {
            cx.run_until_parked();
            cx.opened_url().is_some()
        });
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/"));
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
