//! What the reader does with a message's sanitized HTML (`MessageBody::html`): which
//! images it shows, which links it opens, and the page "Open in browser" writes.
//!
//! The engine has already removed scripts, styles that load resources and dangerous
//! links, but leaves image URLs alone. Loading a remote image tells the sender the
//! message was read (and from where), so only images carried by the message itself are
//! shown (inline parts, `cid:`, and `data:` URLs) until the user asks for the remote
//! ones of a message (`catmail_core::html::remote_image_urls`). The page the reader shows
//! a body as is [`page`]'s, and the images sent to it after it is [`PageImages`]'.

mod images;
mod page;

pub use images::PageImages;
pub use page::{BRIDGE, Colors, FromPage, blank_page};

use std::collections::HashMap;

use catmail_core::html::{InlineImage, embed_inline_images};

/// Content-Security-Policy of the browser page: nothing may load except the images
/// embedded in the page and its inline styles. Browsers ignore `cid:`, so inline parts are
/// embedded as `data:` URLs first.
const BROWSER_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";

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
        embed_inline_images(html, inline).html,
    )
}

/// `text` as HTML text or an attribute's value.
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
    fn opens_only_web_and_mail_links() {
        assert!(is_openable_link("https://example.com/a?b"));
        assert!(is_openable_link("HTTP://example.com"));
        assert!(is_openable_link("mailto:a@example.com"));
        assert!(!is_openable_link("cid:part1"));
        assert!(!is_openable_link("file:///etc/passwd"));
        assert!(!is_openable_link("example.com"));
    }

    #[test]
    fn browser_page_embeds_inline_images_and_forbids_remote_loads() {
        let inline = HashMap::from([("logo@x".to_owned(), InlineImage::new("image/png", b"hi"))]);
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
