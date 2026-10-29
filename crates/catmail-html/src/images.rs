//! The images a message's HTML refers to: which are remote, and inline parts embedded
//! for a renderer that can't resolve `cid:`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::sanitize::{clean, sanitizer};

/// The image URLs in HTML that [`sanitize_html`](crate::sanitize_html) produced, in document order.
pub(crate) fn image_sources(html: &str) -> Vec<String> {
    let found = Arc::new(Mutex::new(Vec::new()));
    let sink = found.clone();
    sanitizer(
        Some(Box::new(move |src| {
            sink.lock().unwrap().push(src.to_owned());
            None
        })),
        None,
    )
    .clean(html);
    Arc::try_unwrap(found).unwrap().into_inner().unwrap()
}

/// More remote images than any message needs to show; the rest of a longer list is left
/// out.
pub const MAX_REMOTE_IMAGES: usize = 200;

/// The remote images in HTML that [`sanitize_html`](crate::sanitize_html) produced, as [`remote_image_url`]
/// writes them, each once, in document order, at most [`MAX_REMOTE_IMAGES`]: what a
/// reader fetches when the user asks for them. Other URLs that aren't the message's own
/// (`file:`, relative) never load.
pub fn remote_image_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for url in image_sources(html) {
        if let Some(url) = remote_image_url(&url)
            && !urls.contains(&url)
        {
            urls.push(url);
            if urls.len() == MAX_REMOTE_IMAGES {
                break;
            }
        }
    }
    urls
}

/// The URL of a remote (`http`, `https`) image, normalized as a URL parser writes it, or
/// `None` for anything else. Renderers ask for an image by its URL as they found it
/// (Blitz parses it first, others don't), so remote images are fetched, cached and
/// looked up by this one spelling.
pub fn remote_image_url(url: &str) -> Option<String> {
    let url = url::Url::parse(url.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

/// An inline part's content, to embed in place of its `cid:` URL.
pub struct InlineImage {
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// HTML that [`sanitize_html`](crate::sanitize_html) produced, with each `cid:` image whose Content-ID is in
/// `inline` replaced by a `data:` URL of the part, for a renderer that can't resolve
/// `cid:` (a browser). The values ammonia's filter returns are not checked again, so the
/// URL is built here rather than taken from the caller.
pub fn embed_inline_images(html: &str, inline: HashMap<String, InlineImage>) -> String {
    let embedding = sanitizer(
        Some(Box::new(move |src| {
            let image = content_id_from_url(src).and_then(|id| inline.get(&id))?;
            Some(format!(
                "data:{};base64,{}",
                media_type(&image.mime_type),
                STANDARD.encode(&image.bytes)
            ))
        })),
        None,
    );
    clean(&embedding, html)
}

/// The MIME type as it may appear in a `data:` URL; it comes from the message, so anything
/// unusual is replaced.
fn media_type(mime_type: &str) -> &str {
    let valid = mime_type.split_once('/').is_some_and(|(t, s)| {
        !t.is_empty()
            && !s.is_empty()
            && mime_type
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/.+-".contains(&b))
    });
    if valid {
        mime_type
    } else {
        "application/octet-stream"
    }
}

/// The Content-ID a `cid:` URL refers to (RFC 2392: the scheme in any case, the rest
/// percent-encoded), or `None` for any other URL. Renderers resolve inline images with
/// this, so they agree with what the parser counts as an inline part.
pub fn content_id_from_url(url: &str) -> Option<String> {
    let scheme = url.get(..4)?;
    if !scheme.eq_ignore_ascii_case("cid:") {
        return None;
    }
    let id = percent_decode(&url[4..]);
    (!id.is_empty()).then_some(id)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize_html;

    #[test]
    fn content_id_from_url_accepts_only_cid_urls() {
        assert_eq!(content_id_from_url("CID:a%40b").as_deref(), Some("a@b"));
        assert_eq!(content_id_from_url("cid:logo").as_deref(), Some("logo"));
        assert_eq!(content_id_from_url("cid:"), None);
        assert_eq!(content_id_from_url("https://x/cid:logo"), None);
        assert_eq!(content_id_from_url("ci"), None);
        // Multi-byte input shorter than the scheme must not panic on a char boundary.
        assert_eq!(content_id_from_url("图片"), None);
    }

    #[test]
    fn the_browser_page_keeps_the_style_sheet() {
        let html = sanitize_html("<style>.a{color:red}</style><img src=\"cid:logo@x\">");
        let inline = HashMap::from([(
            "logo@x".to_owned(),
            InlineImage {
                mime_type: "image/png".into(),
                bytes: b"hi".to_vec(),
            },
        )]);
        let page = embed_inline_images(&html, inline);
        assert!(page.starts_with("<style>.a{color:red;}</style>"), "{page}");
        assert!(page.contains("data:image/png;base64,aGk="), "{page}");
    }

    #[test]
    fn finds_and_embeds_images_by_parsing() {
        let clean = sanitize_html(
            r#"<p>src="cid:in-text"</p><img src="cid:logo" alt="a"><img data-src="https://x/a.gif">
            <img src="https://tracker.example/p.gif"><a href="cid:logo">link</a>"#,
        );
        assert_eq!(
            image_sources(&clean),
            ["cid:logo", "https://tracker.example/p.gif"]
        );

        let inline = HashMap::from([
            (
                "logo".to_owned(),
                InlineImage {
                    mime_type: "image/png".into(),
                    bytes: b"hi".to_vec(),
                },
            ),
            (
                "odd".to_owned(),
                InlineImage {
                    mime_type: "image/gif\"><script>".into(),
                    bytes: b"gif".to_vec(),
                },
            ),
        ]);
        let clean = format!("{clean}<img src=\"cid:odd\"><img src=\"cid:missing\">");
        let embedded = embed_inline_images(&clean, inline);
        assert!(embedded.contains(r#"<img src="data:image/png;base64,aGk=" alt="a">"#));
        assert!(embedded.contains(r#"<img src="data:application/octet-stream;base64,Z2lm">"#));
        assert!(embedded.contains(r#"<img src="cid:missing">"#));
        assert!(
            embedded.contains(r#"src="cid:in-text""#),
            "text is not an attribute"
        );
        assert!(
            embedded.contains(r#"href="cid:logo""#),
            "only images are replaced"
        );
        assert!(embedded.contains("https://tracker.example/p.gif"));
        assert!(!embedded.contains("<script>"));
    }

    #[test]
    fn finds_remote_images() {
        assert_eq!(
            remote_image_urls(
                r#"<p>hi</p><img src="https://Tracker.example/p.gif?a=1&amp;b=2" alt="">
                <img src="cid:logo"><img src="data:image/png;base64,aGk"><a href="https://x">x</a>
                <img src="http://x.example/a b.png"><img src="file:///etc/passwd"><img src="a.png">
                <img src="https://tracker.example/p.gif?a=1&b=2">"#
            ),
            [
                "https://tracker.example/p.gif?a=1&b=2",
                "http://x.example/a%20b.png"
            ]
        );
        assert!(remote_image_urls(r#"<img src="cid:logo"><img src="//x/a.png">"#).is_empty());
        let many: String = (0..MAX_REMOTE_IMAGES + 5)
            .map(|i| format!(r#"<img src="https://x.example/{i}.png">"#))
            .collect();
        assert_eq!(remote_image_urls(&many).len(), MAX_REMOTE_IMAGES);
        assert_eq!(remote_image_url("cid:logo"), None);
        assert_eq!(
            remote_image_url("HTTPS://x.example/a b.png").as_deref(),
            Some("https://x.example/a%20b.png")
        );
    }
}
