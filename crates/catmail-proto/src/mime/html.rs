use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use ammonia::{Builder, UrlRelative};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use html5ever::tokenizer::{TagKind, Token, TokenSink, TokenSinkResult};

use super::content_id_from_url;
use super::{css, tokens};

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
            } else if attribute == "style" && css::fetches(value) {
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

/// Makes HTML from a message safe to render: no scripts, forms, frames or event
/// handlers, links only to web, mail and `cid:` targets. Inline `style` attributes and
/// table layout attributes stay, since most email is laid out with them, unless the
/// style could load something (`url(…)`).
/// Style sheets are rewritten rule by rule ([`css`]) and put back as one `<style>` at the
/// start: what could fetch is dropped, and the sheet can't end its element.
/// Image sources (remote URLs, `cid:`, `data:`) are kept: whether remote images load
/// is the renderer's decision, and it should block them by default.
pub fn sanitize_html(html: &str) -> String {
    clean(&SANITIZER, html)
}

/// The sanitizer's output with the message's style sheets in front. ammonia drops every
/// `<style>` with its content: it doesn't look inside, and writes raw text back as it is.
fn clean(builder: &Builder<'_>, html: &str) -> String {
    let sheet = css::sanitize(&scan(html, false).sheets).css;
    let mut out = String::new();
    if !sheet.is_empty() {
        out.push_str("<style>");
        out.push_str(&sheet);
        out.push_str("</style>");
    }
    out.push_str(&builder.clean(html).to_string());
    out
}

/// How a message's HTML colors itself, which decides whether it can be shown dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorUsage {
    /// It sets no colors: the reader's own may apply.
    Unset,
    /// It colors text or backgrounds for a light page.
    Light,
    /// It has styles of its own for a dark appearance.
    Adaptive,
}

/// How HTML that [`sanitize_html`] produced colors itself.
pub fn color_usage(html: &str) -> ColorUsage {
    let scanned = scan(html, true);
    let sheet = css::sanitize(&scanned.sheets);
    if sheet.adapts_to_dark {
        ColorUsage::Adaptive
    } else if sheet.sets_colors || scanned.colors {
        ColorUsage::Light
    } else {
        ColorUsage::Unset
    }
}

/// What a tokenizer pass finds in HTML: its style sheets, and whether its attributes set
/// colors.
struct Scan {
    sheets: Vec<css::Element>,
    colors: bool,
}

/// Tokenizes HTML for [`Scan`], as the tree builder would switch to raw text (so
/// `<style>` inside `<script>` or `<textarea>` is no style sheet). Style sheets in SVG or
/// MathML are skipped: the sanitizer drops that content whole, and so are those of a
/// `type` other than CSS (templates), which a browser doesn't apply either. Attributes are
/// looked at for colors only if `colors`: that parses every `style` attribute.
fn scan(html: &str, colors: bool) -> Scan {
    struct Sink {
        sheets: RefCell<Vec<css::Element>>,
        /// The text taken so far.
        len: Cell<usize>,
        looks_at_colors: bool,
        colors: Cell<bool>,
        /// `<svg>` and `<math>` elements open.
        foreign: Cell<usize>,
        in_style: Cell<bool>,
    }

    impl TokenSink for Sink {
        type Handle = ();

        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
            match token {
                Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                    if self.looks_at_colors && !self.colors.get() {
                        let colored = tag.attrs.iter().any(|attr| match &*attr.name.local {
                            "bgcolor" | "color" | "text" | "link" => true,
                            "style" => css::sets_colors(&attr.value),
                            _ => false,
                        });
                        self.colors.set(colored);
                    }
                    let name = &*tag.name;
                    if matches!(name, "svg" | "math") {
                        if !tag.self_closing {
                            self.foreign.set(self.foreign.get() + 1);
                        }
                    } else if self.foreign.get() == 0 {
                        let attribute = |name: &str| {
                            let attr = tag.attrs.iter().find(|a| &*a.name.local == name);
                            attr.map(|a| a.value.to_string())
                        };
                        let css = attribute("type")
                            .is_none_or(|t| t.is_empty() || t.eq_ignore_ascii_case("text/css"));
                        let sheet = name == "style" && css;
                        if sheet {
                            self.sheets.borrow_mut().push(css::Element {
                                media: attribute("media"),
                                css: String::new(),
                            });
                        }
                        self.in_style.set(sheet);
                        return tokens::after_start_tag(name);
                    }
                }
                Token::TagToken(tag) => {
                    if matches!(&*tag.name, "svg" | "math") {
                        self.foreign.set(self.foreign.get().saturating_sub(1));
                    }
                    self.in_style.set(false);
                }
                Token::CharacterTokens(text) if self.in_style.get() => {
                    // The sanitizer would keep no more than this anyway.
                    if self.len.get() < 4 * css::MAX_LEN
                        && let Some(sheet) = self.sheets.borrow_mut().last_mut()
                    {
                        sheet.css.push_str(&text);
                        self.len.set(self.len.get() + text.len());
                    }
                }
                _ => {}
            }
            TokenSinkResult::Continue
        }
    }

    let sink = Sink {
        sheets: RefCell::default(),
        len: Cell::new(0),
        looks_at_colors: colors,
        colors: Cell::new(false),
        foreign: Cell::new(0),
        in_style: Cell::new(false),
    };
    let sink = tokens::tokenize(html, sink);
    Scan {
        sheets: sink.sheets.into_inner(),
        colors: sink.colors.get(),
    }
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

/// More remote images than any message needs to show; the rest of a longer list is left
/// out.
pub const MAX_REMOTE_IMAGES: usize = 200;

/// The remote images in HTML that [`sanitize_html`] produced, as [`remote_image_url`]
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

/// HTML that [`sanitize_html`] produced, with each `cid:` image whose Content-ID is in
/// `inline` replaced by a `data:` URL of the part, for a renderer that can't resolve
/// `cid:` (a browser). The values ammonia's filter returns are not checked again, so the
/// URL is built here rather than taken from the caller.
pub fn embed_inline_images(html: &str, inline: HashMap<String, InlineImage>) -> String {
    let embedding = sanitizer(Some(Box::new(move |src| {
        let image = content_id_from_url(src).and_then(|id| inline.get(&id))?;
        Some(format!(
            "data:{};base64,{}",
            media_type(&image.mime_type),
            STANDARD.encode(&image.bytes)
        ))
    })));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_style_sheets_rewritten_in_front() {
        let dirty = r#"<html><head><style>.a{color:red;background:url(https://t.example/p.gif)}</style>
            <style>@import "https://t.example/s.css";@media (max-width:600px){.a{width:100%}}</style>
            </head><body><p class="a">x</p></body></html>"#;
        let clean = sanitize_html(dirty);
        assert!(
            clean.starts_with(
                "<style>.a{color:red;}@media (max-width:600px){.a{width:100%;}}</style>"
            ),
            "{clean}"
        );
        assert_eq!(clean.matches("<style").count(), 1, "{clean}");
        assert!(!clean.contains("t.example"), "{clean}");
        // Sanitizing again gives the same: the reader's passes run it on stored HTML.
        assert_eq!(sanitize_html(&clean), clean);
    }

    #[test]
    fn a_style_sheet_cannot_end_its_element() {
        for dirty in [
            r#"<style>.a::after{content:"</style><img src=x onerror=alert(1)>"}</style>"#,
            "<style>.a{color:red}</style><script>alert(1)</script>",
            "<svg><style>&lt;/style&gt;&lt;img src=x onerror=alert(1)&gt;</style></svg>",
            "<math><mtext><table><mglyph><style>&lt;/style&gt;&lt;img src=x onerror=alert(1)&gt;</style>",
            "<noscript><style></noscript><img src=x onerror=alert(1)></style>",
            "<style>.a{color:red}<!--</style><img src=x onerror=alert(1)>-->",
        ] {
            let clean = sanitize_html(dirty);
            for gone in ["onerror", "<script", "alert(1)<"] {
                assert!(!clean.contains(gone), "{gone} survived {dirty}: {clean}");
            }
            if let Some(sheet) = clean.strip_prefix("<style>") {
                let (css, _) = sheet.split_once("</style>").unwrap();
                assert!(!css.contains('<'), "{dirty}: {clean}");
            }
        }
    }

    #[test]
    fn style_sheets_keep_their_media_and_type() {
        let clean = sanitize_html(
            r#"<style media="print">p{display:none}</style><style media="screen and (max-width:600px)">.a{width:100%}</style><p>Hello</p>"#,
        );
        assert!(
            clean.starts_with(
                "<style>@media print{p{display:none;}}@media screen and (max-width:600px){.a{width:100%;}}</style>"
            ),
            "{clean}"
        );
        assert_eq!(sanitize_html(&clean), clean);
        // A template is no style sheet; `text/css` in any case is one.
        let clean = sanitize_html(
            r#"<style type="text/x-template">p{display:none}</style><style type="TEXT/CSS">.a{color:red}</style>"#,
        );
        assert!(
            clean.starts_with("<style>.a{color:red;}</style>"),
            "{clean}"
        );
        assert_eq!(
            color_usage(&sanitize_html(
                r#"<style media="(prefers-color-scheme: dark)">.a{color:#eee}</style><p class=a style="color:#333">x</p>"#
            )),
            ColorUsage::Adaptive
        );
    }

    #[test]
    fn only_real_style_sheets_are_read() {
        // In SVG the sanitizer drops it all; in a textarea or a script it is text.
        for not_a_sheet in [
            "<svg><style>.a{color:red}</style></svg>",
            "<textarea><style>.a{color:red}</style></textarea>",
            "<script>var s = '<style>.a{color:red}</style>';</script>",
        ] {
            assert!(
                !sanitize_html(not_a_sheet).contains("<style"),
                "{not_a_sheet}"
            );
        }
    }

    #[test]
    fn tells_how_a_message_colors_itself() {
        let usage = |html: &str| color_usage(&sanitize_html(html));
        assert_eq!(
            usage("<p>Hello</p><table><tr><td>x</td></tr></table>"),
            ColorUsage::Unset
        );
        assert_eq!(usage("<p style=\"padding:4px\">x</p>"), ColorUsage::Unset);
        assert_eq!(usage("<p style=\"color:#333\">x</p>"), ColorUsage::Light);
        assert_eq!(
            usage("<table bgcolor=\"#ffffff\"><tr><td>x</td></tr></table>"),
            ColorUsage::Light
        );
        assert_eq!(usage("<font color=\"red\">x</font>"), ColorUsage::Light);
        assert_eq!(
            usage("<style>.a{background:#fff}</style><p class=a>x</p>"),
            ColorUsage::Light
        );
        assert_eq!(
            usage(
                "<style>@media (prefers-color-scheme: dark){.a{color:#eee}}</style><p class=a style=\"color:#333\">x</p>"
            ),
            ColorUsage::Adaptive
        );
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
