use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use ammonia::{Builder, UrlRelative};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::content_id_from_url;

/// Presentation attributes that table-based email layouts depend on.
const LAYOUT_ATTRIBUTES: &[&str] = &[
    "align",
    "bgcolor",
    "border",
    "cellpadding",
    "cellspacing",
    "class",
    "color",
    "dir",
    "face",
    "height",
    "size",
    "style",
    "valign",
    "width",
];

/// Rewrites an `img src` value, or keeps it (`None`).
type ImageHook = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

static SANITIZER: LazyLock<Builder<'static>> = LazyLock::new(|| sanitizer(None));

/// The one sanitizer configuration; `on_image` lets a renderer see or replace image
/// sources during the same parse, instead of scanning the HTML text afterwards.
fn sanitizer(on_image: Option<ImageHook>) -> Builder<'static> {
    let mut builder = Builder::default();
    builder
        .add_tags(["font"])
        .add_generic_attributes(LAYOUT_ATTRIBUTES)
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "data"]))
        // Relative URLs have no base in an email.
        .url_relative(UrlRelative::Deny)
        .attribute_filter(move |element, attribute, value| {
            let url_attribute = matches!(attribute, "href" | "src" | "cite" | "action");
            let image = element == "img" && attribute == "src";
            if url_attribute && is_data_url(value) && !image {
                // Inline images may be data: URLs; a data: link could carry a whole page.
                None
            } else if attribute == "style" && loads_resources(value) {
                // Remote images must stay blockable, also as CSS backgrounds.
                None
            } else if let Some(replaced) =
                on_image.as_ref().filter(|_| image).and_then(|f| f(value))
            {
                Some(Cow::Owned(replaced))
            } else {
                Some(Cow::Borrowed(value))
            }
        });
    builder
}

/// Whether `value` is a `data:` URL, as judged by the parser ammonia checks schemes with
/// (which ignores tabs and line breaks anywhere and leading control characters, as in
/// `&#1;da&#9;ta:`), so the two can't disagree.
fn is_data_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| u.scheme() == "data")
}

/// Whether CSS could fetch something: `url(…)`, `image-set(…)`, `@import`, or escapes
/// that might spell them.
fn loads_resources(css: &str) -> bool {
    let css: String = css
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    css.contains('\\')
        || ["url(", "image(", "image-set(", "src(", "@import"]
            .iter()
            .any(|f| css.contains(f))
}

/// Makes HTML from a message safe to render: no scripts, style sheets, forms, frames or
/// event handlers, links only to web, mail and `cid:` targets. Inline `style`
/// attributes and table layout attributes stay, since most email is laid out with them,
/// unless the style could load something (`url(…)`).
/// Image sources (remote URLs, `cid:`, `data:`) are kept: whether remote images load
/// is the renderer's decision, and it should block them by default.
pub fn sanitize_html(html: &str) -> String {
    SANITIZER.clean(html).to_string()
}

/// The image URLs in HTML that [`sanitize_html`] produced, in document order.
pub fn image_sources(html: &str) -> Vec<String> {
    let found = Arc::new(Mutex::new(Vec::new()));
    let sink = found.clone();
    sanitizer(Some(Box::new(move |src| {
        sink.lock().unwrap().push(src.to_owned());
        None
    })))
    .clean(html);
    Arc::try_unwrap(found).unwrap().into_inner().unwrap()
}

/// An inline part's content, to embed in place of its `cid:` URL.
pub struct InlineImage {
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// HTML that [`sanitize_html`] produced, with each `cid:` image whose Content-ID is in
/// `inline` replaced by a `data:` URL of the part, for a renderer that can't resolve
/// `cid:` (a browser). The values ammonia's filter returns are not checked again, so the
/// URL is built here rather than taken from the caller.
pub fn embed_inline_images(html: &str, inline: HashMap<String, InlineImage>) -> String {
    sanitizer(Some(Box::new(move |src| {
        let image = content_id_from_url(src).and_then(|id| inline.get(&id))?;
        Some(format!(
            "data:{};base64,{}",
            media_type(&image.mime_type),
            STANDARD.encode(&image.bytes)
        ))
    })))
    .clean(html)
    .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_active_content_and_keeps_layout() {
        let dirty = r##"<html><head><style>p{}</style><script>alert(1)</script></head>
            <body onload="x()"><table width="600" bgcolor="#fff"><tr>
            <td style="padding:4px" onclick="y()"><font color="red">你好</font>
            <img src="https://tracker.example/p.gif"><img src="cid:logo@x">
            <img src="data:image/png;base64,AAAA"><a href="javascript:z()">j</a>
            <a href="data:text/html,<script>">d</a><a href="https://example.com">ok</a>
            <iframe src="https://example.com"></iframe><form><input></form>
            </td></tr></table></body></html>"##;
        let clean = sanitize_html(dirty);
        for gone in [
            "<script",
            "alert",
            "<style",
            "onload",
            "onclick",
            "javascript",
            "<iframe",
            "<form",
            "<input",
            "data:text",
        ] {
            assert!(!clean.contains(gone), "{gone} survived: {clean}");
        }
        for kept in [
            r##"<table width="600" bgcolor="#fff">"##,
            r#"style="padding:4px""#,
            r#"<font color="red">你好</font>"#,
            r#"src="https://tracker.example/p.gif""#,
            r#"src="cid:logo@x""#,
            r#"src="data:image/png;base64,AAAA""#,
            r#"href="https://example.com""#,
        ] {
            assert!(clean.contains(kept), "{kept} lost: {clean}");
        }
    }

    #[test]
    fn obfuscated_data_links_and_css_fetches_are_removed() {
        for dirty in [
            "<a href=\"&#1;data:text/html,x\">a</a>",
            "<a href=\"da&#9;ta:text/html,x\">a</a>",
            "<a href=\"d&#10;ata:text/html,x\">a</a>",
            "<a href=\" DATA:text/html,x\">a</a>",
        ] {
            let clean = sanitize_html(dirty);
            assert!(
                clean.contains("<a") && !clean.contains("href"),
                "{dirty} → {clean}"
            );
        }
        for style in [
            "background:url(https://t.example/p.gif)",
            "background: URL ( 'https://t.example' )",
            "background-image:image-set(\"https://t.example\" 1x)",
            "background:u\\72l(https://t.example)",
        ] {
            let clean = sanitize_html(&format!("<p style=\"{style}\">x</p>"));
            assert!(
                clean.contains("<p>x</p>") && !clean.contains("style"),
                "{style} → {clean}"
            );
        }
        assert!(sanitize_html("<p style=\"color:red\">x</p>").contains("style=\"color:red\""));
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
}
