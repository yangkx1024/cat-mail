//! What the reader does with a message's sanitized HTML (`MessageBody::html`): which
//! images it shows, which links it opens, and the page "Open in browser" writes.
//!
//! The engine has already removed scripts, styles that load resources and dangerous
//! links, but leaves image URLs alone. Loading a remote image tells the sender the
//! message was read (and from where), so only images carried by the message itself are
//! shown: inline parts (`cid:`) and `data:` URLs.

use std::collections::HashMap;

use catmail_core::html::{InlineImage, content_id_from_url, embed_inline_images, image_sources};
use data_url::DataUrl;

/// Content-Security-Policy of the browser page: nothing may load except the images
/// embedded in the page and its inline styles. Browsers ignore `cid:`, so inline parts are
/// embedded as `data:` URLs first.
const BROWSER_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";

/// A `data:` URL's image: its MIME type and content.
pub fn embedded_image(url: &str) -> Option<(String, Vec<u8>)> {
    let data = DataUrl::process(url).ok()?;
    let mime = data.mime_type();
    if mime.type_ != "image" {
        return None;
    }
    let mime_type = format!("{}/{}", mime.type_, mime.subtype);
    Some((mime_type, data.decode_to_vec().ok()?.0))
}

/// Whether the image at `url` is not shown: anything but an inline part or a `data:`
/// image.
fn is_blocked(url: &str) -> bool {
    content_id_from_url(url).is_none()
        && !DataUrl::process(url).is_ok_and(|data| data.mime_type().type_ == "image")
}

/// Whether the HTML refers to images that are not shown.
pub fn has_blocked_images(html: &str) -> bool {
    image_sources(html).iter().any(|url| is_blocked(url))
}

/// Links that are handed to the system: web pages and new mail. The sanitizer also lets
/// `cid:` links through, which point into the message and lead nowhere outside it.
pub fn is_openable_link(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
    })
}

/// A standalone HTML page showing the message in a browser: inline images embedded, and
/// a CSP so the browser doesn't load the remote content the reader blocks.
pub fn browser_page(title: &str, html: &str, inline: HashMap<String, InlineImage>) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{BROWSER_CSP}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <title>{}</title></head>\n<body>\n{}\n</body></html>\n",
        escape(title),
        embed_inline_images(html, inline),
    )
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

    #[test]
    fn only_images_the_message_carries_are_shown() {
        assert_eq!(
            embedded_image("data:image/png;base64,aGk"),
            Some(("image/png".into(), b"hi".to_vec()))
        );
        assert_eq!(
            embedded_image("DATA:image/svg+xml;charset=utf-8,%3Csvg%3E"),
            Some(("image/svg+xml".into(), b"<svg>".to_vec()))
        );
        assert_eq!(embedded_image("data:text/html,<script>"), None);
        assert_eq!(embedded_image("https://x/a.png"), None);

        assert!(!is_blocked("cid:logo@x"));
        assert!(!is_blocked("data:image/png;base64,aGk"));
        for blocked in [
            "https://tracker.example/p.gif",
            "http://x/a.png",
            "//x/a.png",
            "file:///etc/passwd",
            "a.png",
            "data:text/html,<script>",
            "",
        ] {
            assert!(is_blocked(blocked), "{blocked}");
        }
    }

    #[test]
    fn opens_only_web_and_mail_links() {
        assert!(is_openable_link("https://example.com/a?b"));
        assert!(is_openable_link("HTTP://example.com"));
        assert!(is_openable_link("mailto:a@example.com"));
        assert!(!is_openable_link("cid:part1"));
        assert!(!is_openable_link("file:///etc/passwd"));
        assert!(!is_openable_link("example.com"));
    }

    #[test]
    fn finds_blocked_images() {
        assert!(has_blocked_images(
            r#"<p>hi</p><img src="https://tracker.example/p.gif" alt="">"#
        ));
        assert!(!has_blocked_images(
            r#"<img src="cid:logo"><img src="data:image/png;base64,aGk"><a href="https://x">x</a>"#
        ));
    }

    #[test]
    fn browser_page_embeds_inline_images_and_forbids_remote_loads() {
        let inline = HashMap::from([(
            "logo@x".to_owned(),
            InlineImage {
                mime_type: "image/png".into(),
                bytes: b"hi".to_vec(),
            },
        )]);
        let html = r#"<img src="cid:logo@x"> <img src="https://x/p.gif">"#;
        let page = browser_page("报告 <Q3>", html, inline);

        assert!(page.contains(&format!(
            r#"<meta http-equiv="Content-Security-Policy" content="{BROWSER_CSP}">"#
        )));
        assert!(page.contains("<title>报告 &lt;Q3&gt;</title>"));
        assert!(page.contains(r#"<img src="data:image/png;base64,aGk=">"#));
        // Left for the CSP to block, as the reader does.
        assert!(page.contains(r#"<img src="https://x/p.gif">"#));
    }
}
