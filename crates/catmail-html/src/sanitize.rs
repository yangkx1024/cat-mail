//! The sanitizer: what of a message's HTML is kept, and its colors for a dark page.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use ammonia::{Builder, UrlRelative};
use html5ever::tokenizer::{TagKind, Token, TokenSink, TokenSinkResult};

use crate::dark::{DarkPalette, Role};
use crate::{css, tokens};

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
pub(crate) type ImageHook = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

static SANITIZER: LazyLock<Builder<'static>> = LazyLock::new(|| sanitizer(None, None));

/// Colors moved for a dark page as the sanitizer goes.
pub(crate) struct Recolor {
    palette: DarkPalette,
    /// Set when a `style` attribute sets colors with custom properties (`var()`), which
    /// can't be moved.
    colors_from_variables: Arc<AtomicBool>,
}

/// The one sanitizer configuration; `on_image` lets a renderer see or replace image
/// sources during the same parse, instead of scanning the HTML text afterwards. With
/// `recolor`, colors in attributes are moved for a dark page.
pub(crate) fn sanitizer(on_image: Option<ImageHook>, recolor: Option<Recolor>) -> Builder<'static> {
    let palette = recolor.as_ref().map(|r| r.palette);
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
            } else if attribute == "style" {
                // Written again from its tokens, as style sheets are: remote images must
                // stay blockable, also as CSS backgrounds.
                let style = css::sanitize_style(value, palette);
                if style.colors_from_variables
                    && let Some(recolor) = &recolor
                {
                    recolor.colors_from_variables.store(true, Ordering::Relaxed);
                }
                (!style.css.is_empty()).then_some(Cow::Owned(style.css))
            } else if let Some(replaced) =
                on_image.as_ref().filter(|_| image).and_then(|f| f(value))
            {
                Some(Cow::Owned(replaced))
            } else if let Some(recolored) =
                palette.and_then(|palette| recolor_attribute(palette, attribute, value))
            {
                Some(Cow::Owned(recolored))
            } else {
                Some(Cow::Borrowed(value))
            }
        });
    builder
}

/// An attribute's value with its colors moved for a dark page, or `None` for one that
/// has no colors. `style` is recolored where the sanitizer rewrites it.
fn recolor_attribute(palette: DarkPalette, attribute: &str, value: &str) -> Option<String> {
    match attribute {
        "bgcolor" => palette.recolor_attribute(value, Role::Surface),
        "color" => palette.recolor_attribute(value, Role::Text),
        _ => None,
    }
}

/// Whether `value` is a `data:` URL, as judged by the parser ammonia checks schemes with
/// (which ignores tabs and line breaks anywhere and leading control characters, as in
/// `&#1;da&#9;ta:`), so the two can't disagree.
fn is_data_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| u.scheme() == "data")
}

/// Makes HTML from a message safe to render: no scripts, forms, frames or event
/// handlers, links only to web, mail and `cid:` targets. Inline `style` attributes and
/// table layout attributes stay, since most email is laid out with them.
/// Style sheets are rewritten rule by rule (`css.rs`) and put back as one `<style>` at the
/// start: what could fetch is dropped, and the sheet can't end its element. `style`
/// attributes are rewritten as a rule's declarations.
/// Image sources (remote URLs, `cid:`, `data:`) are kept: whether remote images load
/// is the renderer's decision, and it should block them by default.
pub fn sanitize_html(html: &str) -> String {
    clean(&SANITIZER, html)
}

/// HTML that [`sanitize_html`] produced, with its own colors moved for a dark page with
/// `palette`'s colors, as Gmail and Outlook show mail written for white (`dark.rs`): in
/// its style sheets, `style` attributes, and `bgcolor` and `color` attributes. A message
/// with dark styles of its own is left as it is (borrowed, not copied): those apply.
///
/// `None` for a message that sets colors with custom properties (`var()`): what a
/// variable holds where it is used is up to the cascade, which only the engine knows, so
/// no move where it is set keeps every pair it ends up in readable. Such a message is to
/// be shown as written, on a light page. Mail clients rarely support them, so mail rarely
/// uses them.
pub fn recolor_html(html: &str, palette: DarkPalette) -> Option<Cow<'_, str>> {
    let sheet = css::sanitize(&style_sheets(html), Some(palette));
    if sheet.adapts_to_dark {
        return Some(Cow::Borrowed(html));
    }
    if sheet.colors_from_variables {
        return None;
    }
    // Known only once the attributes are rewritten; the rare message that uses them is
    // then rewritten for nothing.
    let colors_from_variables = Arc::new(AtomicBool::new(false));
    let recolor = Recolor {
        palette,
        colors_from_variables: colors_from_variables.clone(),
    };
    let html = with_sheet(&sheet.css, &sanitizer(None, Some(recolor)), html);
    (!colors_from_variables.load(Ordering::Relaxed)).then_some(Cow::Owned(html))
}

/// The sanitizer's output with the message's style sheets in front. ammonia drops every
/// `<style>` with its content: it doesn't look inside, and writes raw text back as it is.
pub(crate) fn clean(builder: &Builder<'_>, html: &str) -> String {
    let sheet = css::sanitize(&style_sheets(html), None);
    with_sheet(&sheet.css, builder, html)
}

fn with_sheet(sheet: &str, builder: &Builder<'_>, html: &str) -> String {
    let mut out = Vec::new();
    if !sheet.is_empty() {
        out.extend_from_slice(b"<style>");
        out.extend_from_slice(sheet.as_bytes());
        out.extend_from_slice(b"</style>");
    }
    // Serialized after the sheet: the document's `Display` would build a string of its own
    // that is then copied here.
    builder
        .clean(html)
        .write_to(&mut out)
        .expect("writing to a Vec doesn't fail");
    String::from_utf8(out).expect("html5ever writes UTF-8")
}

/// Finds the style sheets of HTML by tokenizing it as the tree builder would switch to raw
/// text (so `<style>` inside `<script>` or `<textarea>` is no style sheet). Style sheets
/// in SVG or MathML are skipped: the sanitizer drops that content whole, and so are those
/// of a `type` other than CSS (templates), which a browser doesn't apply either.
fn style_sheets(html: &str) -> Vec<css::Element> {
    struct Sink {
        sheets: RefCell<Vec<css::Element>>,
        /// The text taken so far.
        len: Cell<usize>,
        /// `<svg>` and `<math>` elements open.
        foreign: Cell<usize>,
        in_style: Cell<bool>,
    }

    impl TokenSink for Sink {
        type Handle = ();

        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
            match token {
                Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
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
        foreign: Cell::new(0),
        in_style: Cell::new(false),
    };
    tokens::tokenize(html, sink).sheets.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: DarkPalette = DarkPalette {
        background: [0x20; 3],
        foreground: [0xe0; 3],
    };

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
        // A sheet's media decide whether it is a message's own dark styles.
        let adaptive = sanitize_html(
            r#"<style media="(prefers-color-scheme: dark)">.a{color:#eee}</style><p class=a style="color:#333">x</p>"#,
        );
        let kept = recolor_html(&adaptive, PALETTE);
        assert!(matches!(kept, Some(Cow::Borrowed(h)) if h == adaptive));
    }

    #[test]
    fn moves_colors_for_a_dark_page() {
        let clean = sanitize_html(
            r##"<style>.a{color:#000}</style><table bgcolor="#ffffff"><tr>
            <td style="background:white;padding:4px"><font color="black">x</font>
            <img src="cid:logo@x" width="10"></td></tr></table>"##,
        );
        let dark = recolor_html(&clean, PALETTE).unwrap();
        assert!(
            dark.starts_with("<style>.a{color:#e0e0e0;}</style>"),
            "{dark}"
        );
        for moved in [
            r##"bgcolor="#202020""##,
            r#"style="background:#202020;padding:4px;""#,
            r##"color="#e0e0e0""##,
            r#"src="cid:logo@x" width="10""#,
        ] {
            assert!(dark.contains(moved), "{moved}: {dark}");
        }
        // Colors set with custom properties, in a sheet or an attribute, can't be moved.
        for custom in [
            r#"<style>.a{--fg:#000}</style><p class=a style="color:var(--fg)">x</p>"#,
            r#"<style>.a{color:var(--fg)}</style><p class=a style="--fg:#000">x</p>"#,
            r#"<p style="background:var(--bg, #fff)">x</p><p style="margin:0">y</p>"#,
        ] {
            assert_eq!(
                recolor_html(&sanitize_html(custom), PALETTE),
                None,
                "{custom}"
            );
        }
        // A message without colors is left as it was, and one with dark styles of its own
        // keeps its colors for them.
        let plain = sanitize_html("<style>p{margin:0;}</style><p align=\"center\">x</p>");
        assert_eq!(recolor_html(&plain, PALETTE).as_deref(), Some(&*plain));
        let adaptive = sanitize_html(
            "<style>@media (prefers-color-scheme: dark){.a{color:#eee}}</style>\
             <p class=a style=\"color:#333\">x</p>",
        );
        // Not copied: the reader keeps what it has.
        let kept = recolor_html(&adaptive, PALETTE);
        assert!(matches!(kept, Some(Cow::Borrowed(h)) if h == adaptive));
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
            r#"style="padding:4px;""#,
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
            "background:URL( 'https://t.example' )",
            "background-image:image-set(\"https://t.example\" 1x)",
            "background:u\\72l(https://t.example)",
        ] {
            let clean = sanitize_html(&format!("<p style=\"{style}\">x</p>"));
            assert!(
                clean.contains("<p>x</p>") && !clean.contains("style"),
                "{style} → {clean}"
            );
        }
    }

    #[test]
    fn rewrites_style_attributes_as_declarations() {
        let style = |style: &str| {
            let clean = sanitize_html(&format!("<p style=\"{style}\">x</p>"));
            assert_eq!(sanitize_html(&clean), clean, "{style}");
            clean
        };
        // What mail uses stays: `!important`, vendor prefixes, Outlook's own properties.
        assert_eq!(
            style(
                " padding : 4px 8px ; color:red !important;-webkit-text-size-adjust:100%;mso-line-height-rule:exactly "
            ),
            "<p style=\"padding:4px 8px;color:red !important;-webkit-text-size-adjust:100%;mso-line-height-rule:exactly;\">x</p>"
        );
        // A declaration that fetches, runs script or doesn't parse goes alone.
        assert_eq!(
            style("color:red;background:url(https://t.example/p.gif);margin:0"),
            "<p style=\"color:red;margin:0;\">x</p>"
        );
        assert_eq!(
            style("width:expression(alert(1));}color:blue;height:1px"),
            "<p style=\"height:1px;\">x</p>"
        );
        // `url (…)` is no function: nothing loads.
        assert_eq!(
            style("background:url (p.gif)"),
            "<p style=\"background:url (p.gif);\">x</p>"
        );
        // The rewritten value is still only an attribute, also for a dark page.
        let dirty = r#"<p style="color:#000;font-family:'&quot;><script>alert(1)</script>'">x</p>"#;
        let clean = sanitize_html(dirty);
        for html in [recolor_html(&clean, PALETTE).unwrap().into_owned(), clean] {
            assert!(html.starts_with("<p style=\"color:#"), "{html}");
            assert!(html.ends_with("\">x</p>"), "{html}");
            assert_eq!(html.matches('"').count(), 2, "{html}");
        }
    }
}
