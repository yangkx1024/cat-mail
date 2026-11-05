//! The page a message body is shown as in the web view.
//!
//! Unlike Blitz in `catmail-render`, WebKit has a network of its own, and nsh only
//! checks top-level navigations, not a page's subresources. So the page shuts the
//! network out itself: a Content-Security-Policy that allows nothing but `data:`
//! images and inline styles, with every image the reader shows embedded as a `data:`
//! URL first (inline parts here; remote ones the user asked for once that is tried).
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

/// What the page tells the reader, through `window.nsh.postMessage`: its height
/// (`h:<px>`), so the web view can be as tall as the page, and the links clicked
/// (`o:<url>`), which open in the system's browser instead of in the view.
pub const BRIDGE: &str = r#"(function () {
  var last = -1;
  function report() {
    var height = Math.ceil(document.body ? document.body.scrollHeight : 0);
    if (height !== last) {
      last = height;
      window.nsh.postMessage("h:" + height);
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
})();"#;

/// What a message from [`BRIDGE`] says.
#[derive(Debug, PartialEq)]
pub enum FromPage<'a> {
    Height(f32),
    Open(&'a str),
}

impl<'a> FromPage<'a> {
    pub fn parse(message: &'a str) -> Option<Self> {
        if let Some(height) = message.strip_prefix("h:") {
            return height.parse().ok().map(FromPage::Height);
        }
        message.strip_prefix("o:").map(FromPage::Open)
    }
}

/// The page of a body: its sanitized HTML (`MessageBody::html`) with `inline` (by
/// Content-ID) embedded, or the plain text when there is no HTML part.
pub fn body_page(
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
         <style>{BASE_STYLE} html {{ color: {}; background: {}; }} a[href] {{ color: {}; }}</style>\
         </head>\n<body>\n{content}\n</body></html>\n",
        colors.foreground, colors.background, colors.link,
    )
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
        let page = body_page(None, Some("<b>&"), HashMap::new(), &colors());
        assert!(page.contains("&lt;b&gt;&amp;"));
    }

    #[test]
    fn reads_what_the_page_says() {
        assert_eq!(FromPage::parse("h:640"), Some(FromPage::Height(640.0)));
        assert_eq!(
            FromPage::parse("o:https://example.com/"),
            Some(FromPage::Open("https://example.com/"))
        );
        assert_eq!(FromPage::parse("h:tall"), None);
        assert_eq!(FromPage::parse("x"), None);
    }
}
