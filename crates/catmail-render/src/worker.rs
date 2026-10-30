//! A document on a thread of its own, and the requests it answers.
//!
//! An engine's document isn't `Send` (Blitz's holds providers that aren't; litehtml's,
//! when it was compared, raw pointers), so it lives and dies on the thread that opened
//! it: one per document. Requests are answered in order; one whose caller stopped
//! waiting (the UI replaced the task) is skipped, so a resize or a quick move between
//! messages doesn't queue up work nobody will see.
//!
//! A thread can't be stopped. A message built to be slow to lay out keeps its thread
//! busy until done, but no other document waits for it; the caller decides how long to
//! wait, and drops the [`Document`] to let the thread end.

use std::borrow::Cow;
use std::future::Future;
use std::sync::mpsc;
use std::thread;

use futures_channel::oneshot;

use crate::{Appearance, Engine, Hit, Resolver, Selection, Source, Span, Stopped, Tile, pixels};

/// The height that viewport units and full-height pages resolve against. The pane's own
/// height would relayout the page whenever the window is resized; a page is shown whole,
/// scrolled by the pane, so a fixed one is enough.
pub(crate) const VIEWPORT_HEIGHT: f32 = 800.0;

/// The tallest page shown, in CSS pixels (some two hundred screens); the rest is cut off.
/// One rule can make every element of a page enormous.
pub(crate) const MAX_HEIGHT: f32 = 200_000.0;

/// An engine's page, as its thread has it: laid out at a width, painted at the scale it
/// was opened with.
pub(crate) trait Page {
    /// Asks `images` from now on, and now for the images still waiting for an answer;
    /// returns whether it answered any. They show from the next layout.
    fn set_images(&mut self, images: Resolver) -> bool;

    /// Lays the document out `width` CSS pixels wide; returns its height.
    fn layout(&mut self, width: f32) -> f32;

    /// Paints the rows from `top` to `top + height` of the last layout.
    fn paint(&mut self, top: f32, height: f32) -> Pixmap;

    /// What is at a point of the last layout.
    fn hit(&mut self, x: f32, y: f32) -> Hit;

    /// What `span` selects in the last layout.
    fn select(&mut self, span: Span) -> Selection;
}

/// The page as it opens once its appearance moved its colors ([`Appearance::recolor`]):
/// in the default appearance, as written, if they can't be.
fn recolored(mut source: Source, appearance: Appearance) -> (Source, Appearance) {
    let Some(recolor) = &appearance.recolor else {
        return (source, appearance);
    };
    match recolor(&source.html) {
        Some(Cow::Owned(html)) => source.html = html.into(),
        Some(Cow::Borrowed(_)) => {}
        None => return (source, Appearance::default()),
    }
    (source, appearance)
}

/// Opens an engine's page, on the thread that keeps it.
pub(crate) type Opener = fn(&Source, &Appearance, f32) -> Box<dyn Page>;

/// Premultiplied RGBA, as vello_cpu produces it.
pub(crate) struct Pixmap {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// An open page. Dropping it ends its thread once the request in hand is answered.
pub struct Document {
    jobs: mpsc::Sender<Job>,
}

impl Document {
    /// Opens `source` with `engine`, painted at `scale` device pixels per CSS pixel.
    pub fn open(
        engine: Engine,
        source: Source,
        appearance: Appearance,
        scale: f32,
    ) -> std::io::Result<Self> {
        let opener = match engine {
            Engine::Blitz => crate::blitz::open,
        };
        Self::open_with(engine, opener, source, appearance, scale)
    }

    fn open_with(
        engine: Engine,
        opener: Opener,
        source: Source,
        appearance: Appearance,
        scale: f32,
    ) -> std::io::Result<Self> {
        let (jobs, receiver) = mpsc::channel();
        thread::Builder::new()
            .name(format!("render-{}", engine.name()))
            .spawn(move || {
                let (source, appearance) = recolored(source, appearance);
                let page = opener(&source, &appearance, scale);
                serve(page, appearance.canvas, &receiver);
            })?;
        Ok(Self { jobs })
    }

    /// Asks `images` instead of the source's resolver from now on, and now for each image
    /// the page refers to that had no answer yet: images that came after the document
    /// opened (inline parts downloaded, remote images fetched) show from the next
    /// [`layout`](Self::layout), without laying the page out from scratch. `images`
    /// should answer what the last resolver did.
    pub fn set_images(&self, images: Resolver) {
        // A thread that stopped has nothing to show them on.
        let _ = self.jobs.send(Job::Images(images));
    }

    /// The document's height when laid out `width` CSS pixels wide.
    pub fn layout(&self, width: f32) -> impl Future<Output = Result<f32, Stopped>> + use<> {
        self.ask(width, Ask::Layout)
    }

    /// The rows from `top` to `top + height` of the document laid out `width` CSS pixels
    /// wide.
    pub fn paint(
        &self,
        width: f32,
        top: f32,
        height: f32,
    ) -> impl Future<Output = Result<Tile, Stopped>> + use<> {
        self.ask(width, move |reply| Ask::Paint { top, height, reply })
    }

    /// What is at (`x`, `y`), in CSS pixels from the top left of the document laid out
    /// `width` CSS pixels wide.
    pub fn hit(
        &self,
        width: f32,
        x: f32,
        y: f32,
    ) -> impl Future<Output = Result<Hit, Stopped>> + use<> {
        self.ask(width, move |reply| Ask::Hit { x, y, reply })
    }

    /// What `span` selects in the document laid out `width` CSS pixels wide. Nothing is
    /// kept: the caller keeps the span, and asks again after another layout.
    pub fn select(
        &self,
        width: f32,
        span: Span,
    ) -> impl Future<Output = Result<Selection, Stopped>> + use<> {
        self.ask(width, move |reply| Ask::Select { span, reply })
    }

    fn ask<T, F: FnOnce(oneshot::Sender<T>) -> Ask>(
        &self,
        width: f32,
        ask: F,
    ) -> impl Future<Output = Result<T, Stopped>> + use<T, F> {
        let (reply, answer) = oneshot::channel();
        // A thread that stopped (it panicked) drops the request, and so the answer.
        let _ = self.jobs.send(Job::At {
            width,
            ask: ask(reply),
        });
        async move { answer.await.map_err(|_| Stopped) }
    }
}

enum Job {
    /// A request, at the width the document should be laid out at for it.
    At {
        width: f32,
        ask: Ask,
    },
    Images(Resolver),
}

enum Ask {
    Layout(oneshot::Sender<f32>),
    Paint {
        top: f32,
        height: f32,
        reply: oneshot::Sender<Tile>,
    },
    Hit {
        x: f32,
        y: f32,
        reply: oneshot::Sender<Hit>,
    },
    Select {
        span: Span,
        reply: oneshot::Sender<Selection>,
    },
}

impl Ask {
    fn is_canceled(&self) -> bool {
        match self {
            Ask::Layout(reply) => reply.is_canceled(),
            Ask::Paint { reply, .. } => reply.is_canceled(),
            Ask::Hit { reply, .. } => reply.is_canceled(),
            Ask::Select { reply, .. } => reply.is_canceled(),
        }
    }
}

/// Answers requests until the [`Document`] is dropped.
fn serve(mut page: Box<dyn Page>, canvas: [u8; 3], jobs: &mpsc::Receiver<Job>) {
    // The width and height of the last layout.
    let mut laid_out: Option<(f32, f32)> = None;
    while let Ok(job) = jobs.recv() {
        let (width, ask) = match job {
            Job::At { width, ask } => (width, ask),
            Job::Images(images) => {
                if page.set_images(images) {
                    laid_out = None;
                }
                continue;
            }
        };
        if ask.is_canceled() {
            continue;
        }
        let height = match laid_out {
            Some((at, height)) if at == width => height,
            _ => {
                let height = page.layout(width).min(MAX_HEIGHT);
                laid_out = Some((width, height));
                height
            }
        };
        match ask {
            Ask::Layout(reply) => drop(reply.send(height)),
            Ask::Paint { top, height, reply } => {
                drop(reply.send(pixels::tile(page.paint(top, height), canvas)))
            }
            Ask::Hit { x, y, reply } => drop(reply.send(page.hit(x, y))),
            Ask::Select { span, reply } => drop(reply.send(page.select(span))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;

    /// Pages whose layout takes as many milliseconds as their HTML says, and that panic
    /// when asked what is at a point.
    struct Slow(u64);

    impl Page for Slow {
        fn set_images(&mut self, _: Resolver) -> bool {
            false
        }

        fn layout(&mut self, _: f32) -> f32 {
            thread::sleep(Duration::from_millis(self.0));
            1e9
        }

        fn paint(&mut self, _: f32, _: f32) -> Pixmap {
            unreachable!()
        }

        fn hit(&mut self, _: f32, _: f32) -> Hit {
            panic!("nothing here")
        }

        fn select(&mut self, _: Span) -> Selection {
            unreachable!()
        }
    }

    fn open(html: &str) -> Document {
        let opener: Opener = |source, _, _| Box::new(Slow(source.html.parse().unwrap()));
        let source = Source {
            html: html.into(),
            images: Arc::new(|_| None),
        };
        Document::open_with(Engine::Blitz, opener, source, Appearance::default(), 1.0).unwrap()
    }

    #[test]
    fn a_slow_document_keeps_no_other_waiting() {
        let slow = open("5000");
        let _busy = slow.layout(300.0);
        let started = Instant::now();
        let height = pollster::block_on(open("0").layout(300.0)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(height, MAX_HEIGHT, "a page is cut off");
    }

    #[test]
    fn a_panic_stops_only_its_document() {
        let document = open("0");
        assert!(pollster::block_on(document.hit(300.0, 0.0, 0.0)).is_err());
        assert!(pollster::block_on(document.layout(300.0)).is_err());
        assert!(pollster::block_on(open("0").layout(300.0)).is_ok());
    }

    #[test]
    fn a_page_kept_as_it_is_is_not_copied() {
        let source = || Source {
            html: "<p>x</p>".into(),
            images: Arc::new(|_| None),
        };
        let appearance = |recolor: crate::Recolor| Appearance {
            dark: true,
            recolor: Some(recolor),
            ..Appearance::default()
        };
        let kept = source();
        let html = kept.html.clone();
        let (kept, look) = recolored(kept, appearance(Arc::new(|html| Some(html.into()))));
        assert!(Arc::ptr_eq(&kept.html, &html) && look.dark);
        let moved = appearance(Arc::new(|html| Some(html.replace('x', "y").into())));
        let (moved, look) = recolored(source(), moved);
        assert_eq!((&*moved.html, look.dark), ("<p>y</p>", true));
        let (declined, look) = recolored(source(), appearance(Arc::new(|_| None)));
        assert_eq!((&*declined.html, look.dark), ("<p>x</p>", false));
    }
}
