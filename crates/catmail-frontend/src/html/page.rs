//! The page a message body is shown as in the web view.
//!
//! WebKit has a network of its own, and nsh only checks top-level navigations, not a
//! page's subresources. So the page shuts the network out itself: a
//! Content-Security-Policy that allows nothing but inline styles and images that are
//! `data:` URLs or the reader's own ([`IMAGE_SCHEME`](super::IMAGE_SCHEME)). The
//! message's inline parts and remote images are pointed at the reader's URLs, which the
//! reader serves ([`BodyPages`](super::BodyPages)): a remote image only once it is
//! cached or the user asked for it, so the page asks for it, but the network never sees
//! the request.
//! The page's own scripts are off (`java_script_enabled(false)`); the bridge below is
//! an injected user script, which WebKit still runs.

use catmail_core::html::{DarkPalette, Served, escape, serve_images};

use super::pages::image_base;
use super::sideways::Sideways;

/// Nothing may load except the message's `data:` images, those the reader serves, and
/// the inline styles. `form-action` and `base-uri` don't fall back to `default-src`.
const CSP: &str = "default-src 'none'; img-src data: catmail-image:; \
                   style-src 'unsafe-inline'; font-src data:; form-action 'none'; \
                   base-uri 'none'";

/// Ours, under the message's own styles: the page has no margins of its own (the
/// reader pads it). Nor has the body a sideways scroller ([`SIZE`]): here for the
/// WebKits that don't know its `scrollbar-width`.
const BASE_STYLE: &str = "html, body { margin: 0; } \
                          body { padding: 16px; overflow-wrap: break-word; \
                          font-family: sans-serif; } \
                          body::-webkit-scrollbar { display: none !important; }";

/// How tall the root and the body are, in their style attributes (with how they
/// overflow, [`body_page`]): those win over the message's style sheets, `!important` or
/// not.
///
/// They are as tall as what is in them. A message's `html, body { height: 100% }`
/// (common in newsletters) would make them as tall as the web view, which is as tall as
/// the root says, so the view never grew and the body scrolled what didn't fit. Nor are
/// they positioned: an absolute body would leave the root empty, and with `top: 0;
/// bottom: 0` be as tall as the view.
///
/// The page itself never scrolls: on macOS nsh's `scrollable(false)` only sends the
/// wheel to the reader, and WebKit shows a scroller of its own for a page a fraction of
/// a pixel taller than the view, or one a sideways scroller makes taller. With the
/// root's overflow hidden, the body's no longer goes to the viewport: the body is what
/// scrolls a message wider than the pane, and only sideways. It shows no scroller of its
/// own, which would be at the end of the message: it says how it is scrolled
/// ([`FromPage::Sideways`]) and the reader shows a bar for it at the bottom of the pane
/// (`crate::html::Sideways`).
const SIZE: &str = "height: auto !important; min-height: 0 !important; \
                    max-height: none !important; position: static !important;";

/// What the page tells the reader, through `window.nsh.postMessage`, with the page's
/// number ([`body_page`]: one web view shows page after page, and what an earlier one
/// says may come late): its height (`h:<n>:<px>`), so the web
/// view can be as tall as the page (the root's box, which holds the body's margins,
/// rounded up: the root's overflow is hidden, so what the view
/// is short of is cut off; not `scrollHeight`, which is rounded, nor the root's, which
/// is never less than the view's height and so wouldn't shrink for a shorter message), that it has painted at the web view's height
/// (`p:<n>:<px>`, two frames after it loaded and after each resize: WebKit has shown it
/// at that size), how the body is scrolled sideways (`x:<n>:<offset>:<clientWidth>:
/// <scrollWidth>`, when it changes: scrolled, resized, an image come; the offset from the
/// left edge whatever the page's direction, see [`scroll_sideways`]), the links clicked
/// (`o:<url>`), which open in the system's browser
/// instead of in the view. Nothing else leads the view away from the page: a link's
/// menu, whose "Open Link" would load it in the view and "Download Linked File" save it,
/// isn't shown (WebKit's menu elsewhere, with Copy, is), and nothing dropped on the page
/// is taken (a link dropped on a web view loads in it; links still drag out of it). Its
/// images are [`WATCH_IMAGES`]'s.
pub const BRIDGE: &str = r#"(function () {
  var number = document.querySelector('meta[name="catmail-page"]');
  var page = number ? number.content : "0";
  var last = -1;
  var root = document.documentElement;
  function report() {
    var style = getComputedStyle(root);
    var height = Math.ceil(root.getBoundingClientRect().height +
      parseFloat(style.marginTop) + parseFloat(style.marginBottom));
    if (height !== last) {
      last = height;
      window.nsh.postMessage("h:" + page + ":" + height);
    }
  }
  new ResizeObserver(report).observe(root, { box: "border-box" });
  window.addEventListener("load", report);
  report();
  var body = document.body;
  var across = "";
  function sideways() {
    // A right-to-left body (the message's sheet may say so) starts at the right edge,
    // its scrollLeft 0 there and negative leftwards.
    var offset = body.scrollLeft;
    if (getComputedStyle(body).direction === "rtl") {
      offset += body.scrollWidth - body.clientWidth;
    }
    var now = offset + ":" + body.clientWidth + ":" + body.scrollWidth;
    if (now !== across) {
      across = now;
      window.nsh.postMessage("x:" + page + ":" + now);
    }
  }
  // Once a frame, as many images may come in one: reading the widths lays the page out.
  var queued = false;
  function soon() {
    if (!queued) {
      queued = true;
      requestAnimationFrame(function () {
        queued = false;
        sideways();
      });
    }
  }
  body.addEventListener("scroll", sideways, { passive: true });
  // What is in the body grows wider without the body changing: its children resized,
  // an image come.
  var widths = new ResizeObserver(soon);
  widths.observe(body);
  for (var child = body.firstElementChild; child; child = child.nextElementSibling) {
    widths.observe(child);
  }
  document.addEventListener("load", soon, true);
  window.addEventListener("load", soon);
  sideways();
  function link(e) {
    return e.target.closest ? e.target.closest("a[href]") : null;
  }
  document.addEventListener("click", function (e) {
    var a = link(e);
    if (a) {
      e.preventDefault();
      window.nsh.postMessage("o:" + a.href);
    }
  }, true);
  document.addEventListener("contextmenu", function (e) {
    if (link(e)) {
      e.preventDefault();
    }
  }, true);
  function refuse(e) {
    e.preventDefault();
  }
  document.addEventListener("dragover", refuse, true);
  document.addEventListener("drop", refuse, true);
  function painted() {
    requestAnimationFrame(function () {
      requestAnimationFrame(function () {
        window.nsh.postMessage("p:" + page + ":" + window.innerHeight);
      });
    });
  }
  window.addEventListener("resize", painted);
  painted();
})();"#;

/// Tells the reader each of the page's images it serves that has come or failed
/// (`l:<url>`, the URL carrying the page's number), so the next can go
/// ([`BodyPages`](super::BodyPages)); others (`data:` ones, megabytes long, and those
/// blocked) aren't its business. Injected before the page's content, so no image comes
/// before it listens. An image's load and error don't bubble: they are caught on their
/// way down.
pub const WATCH_IMAGES: &str = r#"(function () {
  function loaded(e) {
    var src = e.target.tagName === "IMG" ? e.target.src : "";
    if (src.lastIndexOf("catmail-image:", 0) === 0) window.nsh.postMessage("l:" + src);
  }
  document.addEventListener("load", loaded, true);
  document.addEventListener("error", loaded, true);
})();"#;

/// What a message from [`BRIDGE`] or [`WATCH_IMAGES`] says, and of which page (its number).
#[derive(Debug, PartialEq)]
pub enum FromPage<'a> {
    Height(u64, f32),
    /// At the web view's height then, in points.
    Painted(u64, f32),
    /// An image has come, or failed: by its URL, which says the page.
    Loaded(&'a str),
    Open(&'a str),
    Sideways(u64, Sideways),
}

impl<'a> FromPage<'a> {
    pub fn parse(message: &'a str) -> Option<Self> {
        if let Some(url) = message.strip_prefix("o:") {
            return Some(FromPage::Open(url));
        }
        if let Some(url) = message.strip_prefix("l:") {
            return Some(FromPage::Loaded(url));
        }
        let (kind, rest) = message.split_once(':')?;
        let (page, rest) = rest.split_once(':')?;
        let page = page.parse().ok()?;
        let numbers = rest
            .split(':')
            .map(|n| n.parse().ok().filter(|n: &f32| n.is_finite()))
            .collect::<Option<Vec<f32>>>()?;
        match (kind, numbers.as_slice()) {
            ("h", &[height]) => Some(FromPage::Height(page, height)),
            ("p", &[height]) => Some(FromPage::Painted(page, height)),
            ("x", &[offset, view, content]) => Some(FromPage::Sideways(
                page,
                Sideways {
                    offset,
                    view,
                    content,
                },
            )),
            _ => None,
        }
    }
}

/// The web view's height before the page says its own, in points.
pub const MIN_HEIGHT: f32 = 40.0;

/// The script that scrolls the body sideways to `offset` from its left edge, as
/// [`BRIDGE`] says it is (`x`), whatever the page's direction. On its own: it may not
/// run where [`BRIDGE`]'s functions are.
pub fn scroll_sideways(offset: f32) -> String {
    format!(
        "(function (b) {{ b.scrollLeft = getComputedStyle(b).direction === \"rtl\" \
         ? {offset} - (b.scrollWidth - b.clientWidth) : {offset}; }})(document.body);"
    )
}

/// Whether a web view `height` points tall shows all of a page `page_height` points tall
/// (as tall as it is made, [`MIN_HEIGHT`] at least; to the pixel): what
/// [`FromPage::Painted`] says against the last [`FromPage::Height`].
pub fn tall_enough(height: f32, page_height: f32) -> bool {
    height + 1.0 >= page_height.max(MIN_HEIGHT)
}

/// The page of a body: its sanitized HTML (`MessageBody::html`), its images pointed at
/// the reader's URLs, or the plain text when there is no HTML part; with the remote
/// images it refers to, and whether it is dark. `number` is in what the page says
/// ([`FromPage`]) and in its images' URLs.
///
/// A dark reader's page is dark when the message's colors could be moved for it, or it
/// is plain text, which has no colors of its own. HTML whose colors couldn't be moved
/// (they come from CSS variables) is shown as written, on a light page.
pub(super) fn body_page(
    number: u64,
    html: Option<&str>,
    text: Option<&str>,
    appearance: &Appearance,
) -> Served {
    let palette = appearance.dark.as_ref().map(|dark| dark.palette);
    let served = match html {
        Some(html) => serve_images(html, &image_base(number), palette),
        None => Served {
            html: format!(
                "<pre style=\"white-space: pre-wrap; font-family: inherit; margin: 0\">{}</pre>",
                escape(text.unwrap_or_default())
            ),
            remote_urls: Vec::new(),
            dark: palette.is_some(),
        },
    };
    let colors = match &appearance.dark {
        Some(dark) if served.dark => &dark.colors,
        _ => &appearance.light,
    };
    let html = format!(
        "<!DOCTYPE html>\n<html style=\"{SIZE} overflow: hidden !important\"><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta name=\"catmail-page\" content=\"{number}\">\
         <style>{BASE_STYLE} html {{ color: {}; background: {}; }} a[href] {{ color: {}; }} \
         ::selection {{ background: {}; }}</style>\
         </head>\n<body style=\"{SIZE} overflow-x: auto !important; \
         overflow-y: hidden !important; scrollbar-width: none !important\">\n{}\n</body></html>\n",
        colors.foreground, colors.background, colors.link, colors.selection, served.html,
    );
    Served { html, ..served }
}

/// A page with nothing in it, in the reader's colors: what the web view shows while no
/// message is, so that its web process is running when one opens.
pub(super) fn blank_page(number: u64, appearance: &Appearance) -> String {
    body_page(number, None, None, appearance).html
}

/// What the reader shows a page in.
pub struct Appearance {
    /// Its colors on a light page.
    pub light: Colors,
    /// When it is dark.
    pub dark: Option<DarkAppearance>,
}

/// What a dark reader shows a page in.
pub struct DarkAppearance {
    /// What a message's colors move toward.
    pub palette: DarkPalette,
    /// Its colors on a dark page.
    pub colors: Colors,
}

/// The reader's colors under the message's own, as CSS colors.
pub struct Colors {
    pub foreground: String,
    pub background: String,
    pub link: String,
    /// The highlight of a selection, as in the app's texts: without it WebKit's own.
    pub selection: String,
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::html) fn colors() -> Colors {
        Colors {
            foreground: "#111".into(),
            background: "#fff".into(),
            link: "#06c".into(),
            selection: "#264F784D".into(),
        }
    }

    pub(in crate::html) fn light() -> Appearance {
        Appearance {
            light: colors(),
            dark: None,
        }
    }

    #[test]
    fn the_policy_comes_before_the_message() {
        let page = body_page(
            1,
            Some("<p>hi <img src=\"https://tracker.example/p.gif\"></p>"),
            None,
            &light(),
        )
        .html;
        let policy = page.find("Content-Security-Policy").unwrap();
        let body = page.find("<body").unwrap();
        assert!(policy < body);
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains(&format!("img-src data: {}:;", super::super::IMAGE_SCHEME)));
        assert!(!page.contains("src=\"https:"), "{page}");
    }

    #[test]
    fn the_page_itself_never_scrolls() {
        // WebKit on macOS would show a scroller of its own next to the reader's, and a
        // newsletter's `html, body { height: 100% !important }` held the view at its
        // height and scrolled the body: style attributes outrank any style sheet.
        let page = blank_page(1, &light());
        assert!(
            page.contains(&format!(
                "<html style=\"{SIZE} overflow: hidden !important\">"
            )),
            "{page}"
        );
        assert!(
            page.contains(&format!(
                "<body style=\"{SIZE} overflow-x: auto !important; overflow-y: hidden !important; scrollbar-width: none !important\">"
            )),
            "{page}"
        );
        for tag in ["<html style=\"", "<body style=\""] {
            let style = &page[page.find(tag).unwrap() + tag.len()..];
            let style = &style[..style.find('"').unwrap()];
            // Out of the flow, the body would leave the root empty, or as tall as the view.
            assert!(style.contains("position: static !important"), "{style}");
        }
        assert!(!BRIDGE.contains("scrollHeight"));
    }

    #[test]
    fn the_body_shows_no_sideways_scroller_and_says_how_it_is_scrolled() {
        // Its scroller would be at the end of the message, under the reader's view of
        // it: the reader shows a bar for it at the bottom of the pane instead.
        let page = blank_page(1, &light());
        assert!(
            page.contains("body::-webkit-scrollbar { display: none !important; }"),
            "{page}"
        );
        assert!(BRIDGE.contains(r#""x:" + page + ":" + now"#), "{BRIDGE}");
        assert!(
            BRIDGE.contains("offset + \":\" + body.clientWidth + \":\" + body.scrollWidth"),
            "{BRIDGE}"
        );
        assert!(BRIDGE.contains(r#"body.addEventListener("scroll", sideways"#));
    }

    #[test]
    fn a_right_to_left_body_is_scrolled_from_its_left_edge_too() {
        // Its scrollLeft is 0 at the right edge and negative leftwards: the page says,
        // and is scrolled to, the offset from the left edge, as the bar's thumb goes.
        assert!(
            BRIDGE.contains("offset += body.scrollWidth - body.clientWidth"),
            "{BRIDGE}"
        );
        assert_eq!(
            scroll_sideways(300.0),
            "(function (b) { b.scrollLeft = getComputedStyle(b).direction === \"rtl\" \
             ? 300 - (b.scrollWidth - b.clientWidth) : 300; })(document.body);"
        );
    }

    #[test]
    fn a_selection_on_the_page_is_highlighted_as_in_the_app() {
        // Or WebKit draws its own blue, whatever the theme.
        let page = blank_page(1, &light());
        assert!(
            page.contains("::selection { background: #264F784D; }"),
            "{page}"
        );
    }

    #[test]
    fn the_page_says_each_image_that_came_or_failed() {
        // Or the images after it wait for the reader to give up on it.
        for event in ["load", "error"] {
            let listens = format!("document.addEventListener(\"{event}\", loaded, true)");
            assert!(WATCH_IMAGES.contains(&listens), "{event}");
        }
        assert!(WATCH_IMAGES.contains(r#""l:" + src"#));
        let only_ours = format!(
            "src.lastIndexOf(\"{}:\", 0) === 0",
            super::super::IMAGE_SCHEME
        );
        assert!(WATCH_IMAGES.contains(&only_ours), "{WATCH_IMAGES}");
    }

    #[test]
    fn nothing_but_a_click_leads_the_page_to_a_link() {
        // A link's menu opens it in the view, and a link dropped on the view loads there:
        // a remote page, with none of the reader's CSP.
        for event in ["contextmenu", "dragover", "drop"] {
            assert!(BRIDGE.contains(&format!("\"{event}\"")), "{event}");
        }
    }

    #[test]
    fn plain_text_is_escaped() {
        let page = body_page(1, None, Some("<b>&"), &light());
        assert!(page.html.contains("&lt;b&gt;&amp;"));
        assert!(page.remote_urls.is_empty());
    }

    #[test]
    fn a_dark_page_unless_the_messages_colors_cant_be_moved() {
        let appearance = Appearance {
            light: colors(),
            dark: Some(DarkAppearance {
                palette: DarkPalette {
                    background: [0x20; 3],
                    foreground: [0xe0; 3],
                },
                colors: Colors {
                    foreground: "#eee".into(),
                    background: "#222".into(),
                    link: "#6af".into(),
                    selection: "#264F784D".into(),
                },
            }),
        };
        let shown_in = |page: &Served| {
            let dark = page
                .html
                .contains("html { color: #eee; background: #222; }");
            let light = page
                .html
                .contains("html { color: #111; background: #fff; }");
            assert_ne!(dark, light, "{}", page.html);
            assert_eq!(page.dark, dark, "{}", page.html);
            dark
        };
        let html = "<p bgcolor=\"white\">x</p>";
        let page = body_page(1, Some(html), None, &appearance);
        assert!(shown_in(&page), "recolored HTML");
        assert!(page.html.contains("bgcolor=\"#202020\""), "{}", page.html);
        let custom = "<p style=\"color:var(--fg)\">x</p>";
        let page = body_page(1, Some(custom), None, &appearance);
        assert!(!shown_in(&page), "HTML that couldn't be recolored");
        assert!(
            shown_in(&body_page(1, None, Some("x"), &appearance)),
            "plain text"
        );
        assert!(
            !shown_in(&body_page(1, Some(html), None, &light())),
            "light"
        );
    }

    #[test]
    fn reads_what_the_page_says() {
        assert_eq!(FromPage::parse("h:3:640"), Some(FromPage::Height(3, 640.0)));
        assert_eq!(
            FromPage::parse("o:https://example.com/"),
            Some(FromPage::Open("https://example.com/"))
        );
        assert_eq!(
            FromPage::parse("p:3:480"),
            Some(FromPage::Painted(3, 480.0))
        );
        assert_eq!(
            FromPage::parse("l:catmail-image:3/cid/a%40b"),
            Some(FromPage::Loaded("catmail-image:3/cid/a%40b"))
        );
        assert_eq!(
            FromPage::parse("x:3:120.5:400:1600"),
            Some(FromPage::Sideways(
                3,
                Sideways {
                    offset: 120.5,
                    view: 400.0,
                    content: 1600.0
                }
            ))
        );
        for unknown in [
            "h:3:tall",
            "h:640",
            "h:3:640:1",
            "h:3:NaN",
            "r:3",
            "d:3",
            "p:3",
            "x:3:1",
            "x:3:1:400",
            "x:3:1:400:1600:2",
            "x:3:1:wide:1600",
            "x",
        ] {
            assert_eq!(FromPage::parse(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn the_page_says_its_number() {
        let page = blank_page(7, &light());
        assert!(
            page.contains("<meta name=\"catmail-page\" content=\"7\">"),
            "{page}"
        );
    }

    #[test]
    fn a_page_shows_once_the_web_view_has_grown_to_it() {
        assert!(!tall_enough(MIN_HEIGHT, 620.0), "not grown yet");
        assert!(tall_enough(620.0, 620.0));
        assert!(tall_enough(700.0, 620.0), "taller than the page");
        // To the pixel: the sizes are rounded, and the page's to the next point up.
        assert!(tall_enough(619.5, 620.2), "rounded");
        assert!(tall_enough(619.0, 620.0), "a point short");
        assert!(!tall_enough(618.9, 620.0), "more than a point short");
        // A page shorter than the least height is shown at that height.
        assert!(
            tall_enough(MIN_HEIGHT, 12.0),
            "shorter than the least height"
        );
        assert!(tall_enough(MIN_HEIGHT - 1.0, 12.0));
        assert!(
            !tall_enough(MIN_HEIGHT - 2.0, 12.0),
            "not yet at the least height"
        );
    }
}
