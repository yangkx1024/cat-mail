//! The page a message body is shown as in the web view.
//!
//! WebKit has a network of its own, and nsh only checks top-level navigations, not a
//! page's subresources. So the page shuts the network out itself: a Content-Security-Policy that allows nothing but `data:`
//! images and inline styles, with every image the reader shows embedded as a `data:`
//! URL first: the inline parts loaded when the page is made in it, and the rest of them
//! and remote ones (cached, or that the user asked for) sent to the page as they come,
//! which puts them in place of the blocked URLs, so the page doesn't load again and lose
//! its place.
//! The page's own scripts are off (`java_script_enabled(false)`); the bridge below is
//! an injected user script, which WebKit still runs.

use std::collections::HashMap;

use catmail_core::html::{InlineImage, InlineUrl, embed_inline_images};

use super::escape;

/// Nothing may load except the images embedded in the page and its inline styles.
/// `form-action` and `base-uri` don't fall back to `default-src`.
const CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; \
                   font-src data:; form-action 'none'; base-uri 'none'";

/// Ours, under the message's own styles: the page has no margins of its own (the
/// reader pads it) and doesn't scroll (the reader's scroll view does), but a message
/// wider than the pane scrolls sideways inside it.
/// The page itself never scrolls: on macOS nsh's `scrollable(false)` only sends the
/// wheel to the reader, and WebKit shows a scroller of its own for a page a fraction of
/// a pixel taller than the view, or one a sideways scroller makes taller. With the
/// root's overflow hidden, the body's no longer goes to the viewport: the body is what
/// scrolls sideways.
const BASE_STYLE: &str = "html { overflow: hidden !important; } \
                          html, body { margin: 0; } \
                          body { padding: 16px; overflow-x: auto; overflow-wrap: break-word; \
                          font-family: sans-serif; }";

/// What the page tells the reader, through `window.nsh.postMessage`, with the page's
/// number ([`body_page`]: one web view shows page after page, and what an earlier one
/// says may come late): that it loaded (`r:<n>`), its height (`h:<n>:<px>`), so the web
/// view can be as tall as the page (the root's box, which holds the body's margins and
/// its sideways scroller, rounded up: the root's overflow is hidden, so what the view
/// is short of is cut off; not `scrollHeight`, which is rounded, nor the root's, which
/// is never less than the view's height and so wouldn't shrink for a shorter message), that it has painted at the web view's height
/// (`p:<n>:<px>`, two frames after it loaded and after each resize: WebKit has shown it
/// at that size), the links clicked (`o:<url>`), which open in the system's browser
/// instead of in the view, and that it has taken the image it was sent last (`d:<n>`),
/// so the next can come ([`PageImages`](super::PageImages)).
/// What the reader tells the page ([`image_message`]): the `data:` URL of an image shown,
/// remote or inline, for the `<img>` elements that refer to it (as `img.src` normalizes
/// it, which is `remote_image_url`'s spelling too). Only those: the sanitizer keeps no
/// `background` attribute, and no `url()` in styles.
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
  document.addEventListener("click", function (e) {
    var a = e.target.closest ? e.target.closest("a[href]") : null;
    if (a) {
      e.preventDefault();
      window.nsh.postMessage("o:" + a.href);
    }
  }, true);
  function absolute(url) {
    try { return new URL(url, document.baseURI).href; } catch (e) { return null; }
  }
  // The images by the URL they refer to, found once: parsing each image's URL for each
  // message, once dozens are data: URLs, keeps the page busy for seconds, and the next
  // page waits behind it.
  var images = null;
  function imagesOf(url) {
    if (!images) {
      images = new Map();
      document.querySelectorAll("img[src]").forEach(function (img) {
        var src = img.getAttribute("src");
        if (src.trim().slice(0, 5).toLowerCase() === "data:") return;
        src = absolute(src);
        if (!src) return;
        if (images.has(src)) images.get(src).push(img); else images.set(src, [img]);
      });
    }
    return images.get(url) || [];
  }
  window.addEventListener("message", function (e) {
    var d = String(e.data);
    if (d.slice(0, 2) !== "i:") return;
    try {
      var end = d.indexOf("\n");
      var url = d.slice(2, end), data = d.slice(end + 1);
      if (data.slice(0, 5) === "data:") imagesOf(url).forEach(function (img) { img.src = data; });
    } finally {
      // Whatever became of it: no other image comes until this is said.
      window.nsh.postMessage("d:" + page);
    }
  });
  window.nsh.postMessage("r:" + page);
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

/// The message that puts an image's `data:` URL in place of `url` in the page.
pub(super) fn image_message(url: &str, data_url: &str) -> String {
    format!("i:{url}\n{data_url}")
}

/// What a message from [`BRIDGE`] says, and of which page (its number).
#[derive(Debug, PartialEq)]
pub enum FromPage<'a> {
    Ready(u64),
    Height(u64, f32),
    /// At the web view's height then, in points.
    Painted(u64, f32),
    /// It has taken the image sent last.
    Taken(u64),
    Open(&'a str),
}

impl<'a> FromPage<'a> {
    pub fn parse(message: &'a str) -> Option<Self> {
        if let Some(url) = message.strip_prefix("o:") {
            return Some(FromPage::Open(url));
        }
        let (kind, rest) = message.split_once(':')?;
        let (page, height) = match rest.split_once(':') {
            Some((page, height)) => (page.parse().ok()?, Some(height.parse().ok()?)),
            None => (rest.parse().ok()?, None),
        };
        match (kind, height) {
            ("r", None) => Some(FromPage::Ready(page)),
            ("d", None) => Some(FromPage::Taken(page)),
            ("h", Some(height)) => Some(FromPage::Height(page, height)),
            ("p", Some(height)) => Some(FromPage::Painted(page, height)),
            _ => None,
        }
    }
}

/// A page made by [`body_page`].
pub(super) struct BodyPage {
    pub html: String,
    /// The `cid:` images whose parts it isn't made with: what [`image_message`] puts each
    /// part in place of once it has loaded.
    pub inline_urls: Vec<InlineUrl>,
}

/// The page of a body: its sanitized HTML (`MessageBody::html`) with `inline` (by
/// Content-ID) embedded, or the plain text when there is no HTML part. `number` is in
/// what the page says ([`FromPage`]).
pub(super) fn body_page(
    number: u64,
    html: Option<&str>,
    text: Option<&str>,
    inline: HashMap<String, InlineImage>,
    colors: &Colors,
) -> BodyPage {
    let (content, inline_urls) = match html {
        Some(html) => {
            let embedded = embed_inline_images(html, inline);
            (embedded.html, embedded.inline_urls)
        }
        None => (
            format!(
                "<pre style=\"white-space: pre-wrap; font-family: inherit; margin: 0\">{}</pre>",
                escape(text.unwrap_or_default())
            ),
            Vec::new(),
        ),
    };
    let html = format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta name=\"catmail-page\" content=\"{number}\">\
         <style>{BASE_STYLE} html {{ color: {}; background: {}; }} a[href] {{ color: {}; }}</style>\
         </head>\n<body>\n{content}\n</body></html>\n",
        colors.foreground, colors.background, colors.link,
    );
    BodyPage { html, inline_urls }
}

/// A page with nothing in it, in `colors`: what the web view shows while no message is,
/// so that its web process is running when one opens.
pub fn blank_page(number: u64, colors: &Colors) -> String {
    body_page(number, None, None, HashMap::new(), colors).html
}

/// The reader's colors under the message's own, as CSS colors.
pub struct Colors {
    pub foreground: String,
    pub background: String,
    pub link: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        Colors {
            foreground: "#111".into(),
            background: "#fff".into(),
            link: "#06c".into(),
        }
    }

    #[test]
    fn the_policy_comes_before_the_message() {
        let page = body_page(
            1,
            Some("<p>hi <img src=\"https://tracker.example/p.gif\"></p>"),
            None,
            HashMap::new(),
            &colors(),
        )
        .html;
        let policy = page.find("Content-Security-Policy").unwrap();
        let body = page.find("<body>").unwrap();
        assert!(policy < body);
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("img-src data:;"));
    }

    #[test]
    fn inline_images_are_embedded_or_sent_as_they_load() {
        let inline = HashMap::from([(
            "logo@example".to_owned(),
            InlineImage::new("image/png", &[1, 2, 3]),
        )]);
        let page = body_page(
            1,
            Some("<img src=\"cid:logo@example\"><img src=\"CID:chart%40example\">"),
            None,
            inline,
            &colors(),
        );
        assert!(
            page.html.contains("src=\"data:image/png;base64,AQID\""),
            "{}",
            page.html
        );
        assert_eq!(
            page.inline_urls,
            [InlineUrl {
                content_id: "chart@example".to_owned(),
                url: "cid:chart%40example".to_owned(),
            }]
        );
    }

    #[test]
    fn the_page_itself_never_scrolls() {
        // WebKit on macOS would show a scroller of its own next to the reader's.
        let page = blank_page(1, &colors());
        assert!(
            page.contains("html { overflow: hidden !important; }"),
            "{page}"
        );
        assert!(!BRIDGE.contains("scrollHeight"));
    }

    #[test]
    fn the_page_finds_its_images_once() {
        // Not on each image sent: by then those shown are data: URLs, megabytes to parse
        // again for every image that follows.
        let handler = &BRIDGE[BRIDGE.find("addEventListener(\"message\"").unwrap()..];
        let handler = &handler[..handler.find("});").unwrap()];
        assert!(!handler.contains("querySelectorAll"), "{handler}");
        assert!(handler.contains("imagesOf(url)"), "{handler}");
    }

    #[test]
    fn the_page_says_it_has_taken_each_image() {
        // Or no other image comes.
        let handler = &BRIDGE[BRIDGE.find("addEventListener(\"message\"").unwrap()..];
        let handler = &handler[..handler.find("\n  });").unwrap()];
        let taken = handler.find("postMessage(\"d:\" + page)").expect(handler);
        assert!(handler[..taken].contains("finally"), "{handler}");
    }

    #[test]
    fn plain_text_is_escaped() {
        let page = body_page(1, None, Some("<b>&"), HashMap::new(), &colors());
        assert!(page.html.contains("&lt;b&gt;&amp;"));
        assert!(page.inline_urls.is_empty());
    }

    #[test]
    fn reads_what_the_page_says() {
        assert_eq!(FromPage::parse("h:3:640"), Some(FromPage::Height(3, 640.0)));
        assert_eq!(
            FromPage::parse("o:https://example.com/"),
            Some(FromPage::Open("https://example.com/"))
        );
        assert_eq!(FromPage::parse("r:3"), Some(FromPage::Ready(3)));
        assert_eq!(
            FromPage::parse("p:3:480"),
            Some(FromPage::Painted(3, 480.0))
        );
        assert_eq!(FromPage::parse("d:3"), Some(FromPage::Taken(3)));
        for unknown in [
            "h:3:tall", "h:640", "r", "r:3:1", "p:3", "d:3:1", "x:3", "x",
        ] {
            assert_eq!(FromPage::parse(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn the_page_says_its_number() {
        let page = blank_page(7, &colors());
        assert!(
            page.contains("<meta name=\"catmail-page\" content=\"7\">"),
            "{page}"
        );
    }
}
