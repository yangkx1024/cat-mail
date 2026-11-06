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

use catmail_core::html::{InlineImage, embed_inline_images};

/// Nothing may load except the images embedded in the page and its inline styles.
/// `form-action` and `base-uri` don't fall back to `default-src`.
const CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; \
                   font-src data:; form-action 'none'; base-uri 'none'";

/// Ours, under the message's own styles: the page has no margins of its own (the
/// reader pads it) and doesn't scroll (the reader's scroll view does), but a message
/// wider than the pane scrolls sideways inside it.
const BASE_STYLE: &str = "html, body { margin: 0; } \
                          body { padding: 16px; overflow-x: auto; overflow-wrap: break-word; \
                          font-family: sans-serif; }";

/// What the page tells the reader, through `window.nsh.postMessage`, with the page's
/// number ([`body_page`]: one web view shows page after page, and what an earlier one
/// says may come late): that it loaded (`r:<n>`), its height (`h:<n>:<px>`), so the web
/// view can be as tall as the page, that it has painted at the web view's height
/// (`p:<n>:<px>`, two frames after it loaded and after each resize: WebKit has shown it
/// at that size), the links clicked (`o:<url>`), which open in the system's browser
/// instead of in the view, and once, before `r`, each `cid:` URL of its images
/// (`c:<n>:<url>`, as [`BRIDGE`]'s image message spells it), so the inline parts can go in
/// as they load.
/// What the reader tells the page ([`image_message`]): the `data:` URL of an image shown,
/// remote or inline, for the elements that refer to it (as `img.src` normalizes it, which
/// is `remote_image_url`'s spelling too).
pub const BRIDGE: &str = r#"(function () {
  var number = document.querySelector('meta[name="catmail-page"]');
  var page = number ? number.content : "0";
  var last = -1;
  function report() {
    var height = Math.ceil(document.body ? document.body.scrollHeight : 0);
    if (height !== last) {
      last = height;
      window.nsh.postMessage("h:" + page + ":" + height);
    }
  }
  if (document.body) new ResizeObserver(report).observe(document.body);
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
  window.addEventListener("message", function (e) {
    var d = String(e.data);
    if (d.slice(0, 2) !== "i:") return;
    var end = d.indexOf("\n");
    var url = d.slice(2, end), data = d.slice(end + 1);
    if (data.slice(0, 5) !== "data:") return;
    document.querySelectorAll("img[src]").forEach(function (img) {
      if (absolute(img.getAttribute("src")) === url) img.src = data;
    });
    document.querySelectorAll("[background]").forEach(function (el) {
      if (absolute(el.getAttribute("background")) === url) el.setAttribute("background", data);
    });
  });
  var cids = {};
  document.querySelectorAll("img[src]").forEach(function (img) {
    var url = absolute(img.getAttribute("src"));
    if (url && url.slice(0, 4).toLowerCase() === "cid:" && !cids[url]) {
      cids[url] = true;
      window.nsh.postMessage("c:" + page + ":" + url);
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

/// The message that puts a remote image's `data:` URL in place of `url` in the page.
pub fn image_message(url: &str, data_url: &str) -> String {
    format!("i:{url}\n{data_url}")
}

/// What a message from [`BRIDGE`] says, and of which page (its number).
#[derive(Debug, PartialEq)]
pub enum FromPage<'a> {
    Ready(u64),
    Height(u64, f32),
    /// At the web view's height then, in points.
    Painted(u64, f32),
    /// A `cid:` URL its images refer to.
    Inline(u64, &'a str),
    Open(&'a str),
}

impl<'a> FromPage<'a> {
    pub fn parse(message: &'a str) -> Option<Self> {
        if let Some(url) = message.strip_prefix("o:") {
            return Some(FromPage::Open(url));
        }
        if let Some(rest) = message.strip_prefix("c:") {
            let (page, url) = rest.split_once(':')?;
            return Some(FromPage::Inline(page.parse().ok()?, url));
        }
        let (kind, rest) = message.split_once(':')?;
        let (page, height) = match rest.split_once(':') {
            Some((page, height)) => (page.parse().ok()?, Some(height.parse().ok()?)),
            None => (rest.parse().ok()?, None),
        };
        match (kind, height) {
            ("r", None) => Some(FromPage::Ready(page)),
            ("h", Some(height)) => Some(FromPage::Height(page, height)),
            ("p", Some(height)) => Some(FromPage::Painted(page, height)),
            _ => None,
        }
    }
}

/// The page of a body: its sanitized HTML (`MessageBody::html`) with `inline` (by
/// Content-ID) embedded, or the plain text when there is no HTML part. `number` is in
/// what the page says ([`FromPage`]).
pub fn body_page(
    number: u64,
    html: Option<&str>,
    text: Option<&str>,
    inline: HashMap<String, InlineImage>,
    colors: &Colors,
) -> String {
    let content = match html {
        Some(html) => embed_inline_images(html, inline),
        None => format!(
            "<pre style=\"white-space: pre-wrap; font-family: inherit; margin: 0\">{}</pre>",
            escape(text.unwrap_or_default())
        ),
    };
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta name=\"catmail-page\" content=\"{number}\">\
         <style>{BASE_STYLE} html {{ color: {}; background: {}; }} a[href] {{ color: {}; }}</style>\
         </head>\n<body>\n{content}\n</body></html>\n",
        colors.foreground, colors.background, colors.link,
    )
}

/// A page with nothing in it, in `colors`: what the web view shows while no message is,
/// so that its web process is running when one opens.
pub fn blank_page(number: u64, colors: &Colors) -> String {
    body_page(number, None, None, HashMap::new(), colors)
}

/// The reader's colors under the message's own, as CSS colors.
pub struct Colors {
    pub foreground: String,
    pub background: String,
    pub link: String,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
        );
        let policy = page.find("Content-Security-Policy").unwrap();
        let body = page.find("<body>").unwrap();
        assert!(policy < body);
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("img-src data:;"));
    }

    #[test]
    fn inline_images_are_embedded() {
        let inline = HashMap::from([(
            "logo@example".to_owned(),
            InlineImage {
                mime_type: "image/png".into(),
                bytes: vec![1, 2, 3],
            },
        )]);
        let page = body_page(
            1,
            Some("<img src=\"cid:logo@example\">"),
            None,
            inline,
            &colors(),
        );
        assert!(
            page.contains("src=\"data:image/png;base64,AQID\""),
            "{page}"
        );
    }

    #[test]
    fn plain_text_is_escaped() {
        let page = body_page(1, None, Some("<b>&"), HashMap::new(), &colors());
        assert!(page.contains("&lt;b&gt;&amp;"));
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
        assert_eq!(
            FromPage::parse("c:3:cid:logo@example"),
            Some(FromPage::Inline(3, "cid:logo@example"))
        );
        for unknown in [
            "h:3:tall",
            "h:640",
            "r",
            "r:3:1",
            "p:3",
            "x:3",
            "x",
            "c:3",
            "c:x:cid:a",
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
