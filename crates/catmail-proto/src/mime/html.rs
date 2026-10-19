use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

use ammonia::{Builder, UrlRelative};

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

static SANITIZER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut builder = Builder::default();
    builder
        .add_tags(["font"])
        .add_generic_attributes(LAYOUT_ATTRIBUTES)
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "data"]))
        // Relative URLs have no base in an email.
        .url_relative(UrlRelative::Deny)
        .attribute_filter(|element, attribute, value| {
            let url_attribute = matches!(attribute, "href" | "src" | "cite" | "action");
            if url_attribute && is_data_url(value) && !(element == "img" && attribute == "src") {
                // Inline images may be data: URLs; a data: link could carry a whole page.
                None
            } else if attribute == "style" && loads_resources(value) {
                // Remote images must stay blockable, also as CSS backgrounds.
                None
            } else {
                Some(Cow::Borrowed(value))
            }
        });
    builder
});

/// Whether `value` is a `data:` URL as a URL parser sees it: tabs and line breaks
/// anywhere, and leading control characters and spaces, are ignored (`&#1;da&#9;ta:`).
fn is_data_url(value: &str) -> bool {
    let scheme: String = value
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .skip_while(|c| *c <= ' ')
        .take(5)
        .collect();
    scheme.eq_ignore_ascii_case("data:")
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
}
