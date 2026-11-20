//! The composer's formatted text, without the editor: the HTML a plain draft opens as,
//! the plain text sent with the HTML, and the images, which the editor shows from files
//! and the draft refers to by Content-ID. Remote images it doesn't show at all.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use catmail_core::html::{escape, quoted_text_html, remote_image_url, without_sheet};
use catmail_core::{FORWARD_SEPARATOR, Quote, QuoteHead};

/// The images of a draft's HTML: the file each one's content is in
/// ([`DraftEditor::images`](catmail_core::DraftEditor::images)), by Content-ID. The
/// editor's document has the draft's URLs, as they are sent (`cid:` ones for these);
/// what it shows of each is [`Self::shown`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Images {
    paths: HashMap<String, PathBuf>,
}

impl Images {
    pub fn insert(&mut self, content_id: String, path: PathBuf) {
        self.paths.insert(content_id, path);
    }

    pub fn path(&self, content_id: &str) -> Option<&Path> {
        self.paths.get(content_id).map(PathBuf::as_path)
    }

    /// What the editor shows an image of `src` from: the file of one of the draft's
    /// images (`cid:`), or a `data:` URL. Nothing else: the editor would load it from
    /// where it points, outside the reader's rules, and a quote answered in (or a draft
    /// that has one, or what is pasted from a web page) would tell its sender, through a
    /// tracking pixel, that it was read. Those show as an empty box, and are sent as they
    /// are.
    pub fn shown(&self, src: &str) -> Option<String> {
        if let Some(id) = src.strip_prefix("cid:") {
            return Some(self.path(id)?.to_string_lossy().into_owned());
        }
        src.starts_with("data:").then(|| src.to_owned())
    }

    /// Takes out of content pasted into the editor (ProseMirror's JSON of a slice) the
    /// images the draft can't send: `cid:` ones of another message's parts, the reader's
    /// [`IMAGE_SCHEME`](crate::html::IMAGE_SCHEME) ones, files on this computer. The
    /// recipient would see a broken image, or this computer's paths. The draft's own
    /// images stay, `data:` ones, and web ones (not shown, [`Self::shown`]).
    pub fn drop_unsendable(&self, slice: &mut Value) {
        retain_images(slice, &mut |src| match src.strip_prefix("cid:") {
            Some(id) => self.paths.contains_key(id),
            None => src.starts_with("data:") || remote_image_url(src).is_some(),
        });
    }
}

/// Takes out of `doc`'s content (ProseMirror's JSON) the images whose `src` `keep` says
/// no to.
fn retain_images(doc: &mut Value, keep: &mut dyn FnMut(&str) -> bool) {
    let Some(Value::Array(content)) = doc.get_mut("content") else {
        return;
    };
    content.retain_mut(|child| {
        if child.get("type").and_then(Value::as_str) != Some("image") {
            retain_images(child, keep);
            return true;
        }
        match child.pointer("/attrs/src").and_then(Value::as_str) {
            Some(src) => keep(src),
            None => true,
        }
    });
}

/// The HTML a plain text opens as in the editor: a paragraph per line. A `>` the user
/// typed is theirs, not a quote.
pub fn text_to_html(text: &str) -> String {
    text.lines()
        .map(|line| format!("<p>{}</p>", escape(line)))
        .collect()
}

/// The HTML a quote goes into the editor as, when the user answers in it: its head (a
/// forward's headers a paragraph each, their names bold), then the message quoted (in a
/// quote, for a reply), what of its HTML the editor can hold (without its style sheet,
/// which the editor has no place for), or its text, a paragraph a line, nested as it
/// quotes in turn.
pub fn quote_to_html(quote: &Quote) -> String {
    let mut html = String::from("<p></p>");
    let (open, close) = match &quote.head {
        QuoteHead::Reply(attribution) => {
            html.push_str(&text_to_html(&attribution.to_string()));
            ("<blockquote>", "</blockquote>")
        }
        QuoteHead::Forward(headers) => {
            html.push_str(&text_to_html(FORWARD_SEPARATOR));
            for (field, value) in headers {
                let _ = write!(html, "<p><b>{}:</b> {}</p>", field.label(), escape(value));
            }
            ("<p></p>", "")
        }
    };
    html.push_str(open);
    match &quote.html {
        Some(original) => html.push_str(without_sheet(original)),
        None => html.push_str(&quoted_text_html(&quote.text, "", "<p></p>")),
    }
    html.push_str(close);
    html
}

/// The plain text sent with the editor's document (ProseMirror's JSON), for readers
/// that show only text: a line per paragraph, `> ` before quoted lines. List items start
/// with `* ` or their number, a link's address follows its text, images are left out.
pub fn doc_to_text(doc: &Value) -> String {
    let mut lines = Vec::new();
    for block in children(doc) {
        block_lines(block, &mut lines);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn children(node: &Value) -> &[Value] {
    node.get("content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn block_lines(block: &Value, lines: &mut Vec<String>) {
    match node_type(block) {
        "blockquote" => {
            let mut inner = Vec::new();
            for child in children(block) {
                block_lines(child, &mut inner);
            }
            lines.extend(inner.into_iter().map(|line| {
                if line.is_empty() || line.starts_with('>') {
                    format!(">{line}")
                } else {
                    format!("> {line}")
                }
            }));
        }
        "bulletList" | "orderedList" | "taskList" => {
            let start = block
                .get("attrs")
                .and_then(|a| a.get("start"))
                .and_then(Value::as_u64)
                .unwrap_or(1);
            for (i, item) in children(block).iter().enumerate() {
                let marker = match node_type(block) {
                    "orderedList" => format!("{}. ", start + i as u64),
                    _ => "* ".to_owned(),
                };
                let mut inner = Vec::new();
                for child in children(item) {
                    block_lines(child, &mut inner);
                }
                let indent = " ".repeat(marker.chars().count());
                for (j, line) in inner.into_iter().enumerate() {
                    lines.push(match j {
                        0 => format!("{marker}{line}"),
                        _ if line.is_empty() => line,
                        _ => format!("{indent}{line}"),
                    });
                }
            }
        }
        "horizontalRule" => lines.push("---".to_owned()),
        // A textblock (paragraph, heading, code), or one this doesn't know: its text.
        _ if children(block).iter().all(|c| !is_block(c)) => {
            let mut text = String::new();
            for inline in children(block) {
                inline_text(inline, &mut text);
            }
            lines.extend(text.split('\n').map(str::to_owned));
        }
        _ => {
            for child in children(block) {
                block_lines(child, lines);
            }
        }
    }
}

fn is_block(node: &Value) -> bool {
    !matches!(node_type(node), "text" | "hardBreak" | "image")
}

fn inline_text(node: &Value, out: &mut String) {
    match node_type(node) {
        "text" => {
            let text = node.get("text").and_then(Value::as_str).unwrap_or_default();
            out.push_str(text);
            let href = node
                .get("marks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|m| node_type(m) == "link")
                .find_map(|m| m.get("attrs")?.get("href")?.as_str());
            if let Some(href) = href.filter(|href| *href != text) {
                out.push_str(&format!(" <{href}>"));
            }
        }
        "hardBreak" => out.push('\n'),
        _ => {}
    }
}

/// The address a link goes to, as typed in the link dialog: `None` for nothing (the
/// link comes off). An address without a scheme gets one, as browsers' address bars
/// do: `mailto:` for an email address, else `https://`. Whether a scheme may be a link
/// at all (`javascript:` may not) is the editor's to say.
pub fn link_href(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    let scheme = typed.split_once(':').map(|(scheme, _)| scheme);
    let has_scheme = scheme.is_some_and(|s| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    // `localhost:8080`, `intranet:8080/help` and `example.com:443/x` are a host and a
    // port, not a scheme: up to five digits, no more than 65535, ending the address or
    // its host. The numbers of `tel:5551234` and `tel:110` are not ports.
    let port = typed.split_once(':').is_some_and(|(host, rest)| {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let (number, after) = rest.split_at(digits);
        !["tel", "sms", "fax"]
            .iter()
            .any(|s| host.eq_ignore_ascii_case(s))
            && (1..=5).contains(&number.len())
            && number.parse::<u32>().is_ok_and(|n| n <= 65535)
            && (after.is_empty() || after.starts_with(['/', '?', '#']))
    });
    Some(if has_scheme && !port {
        typed.to_owned()
    } else if typed.contains('@') && !typed.contains('/') {
        format!("mailto:{typed}")
    } else {
        format!("https://{typed}")
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use catmail_core::{Attribution, ForwardField};

    use super::*;

    #[test]
    fn plain_text_opens_as_paragraphs_its_marks_as_typed() {
        assert_eq!(text_to_html(""), "");
        assert_eq!(
            text_to_html("Hi <you> & co\n\n> not a quote"),
            "<p>Hi &lt;you&gt; &amp; co</p><p></p><p>&gt; not a quote</p>"
        );
    }

    #[test]
    fn a_quote_answered_in_goes_into_the_text_whole() {
        let mut quote = Quote {
            head: QuoteHead::Reply(Attribution {
                from: Some("A".into()),
                date: None,
            }),
            text: "first\n> older\n".into(),
            html: None,
        };
        assert_eq!(
            quote_to_html(&quote),
            "<p></p><p>A wrote:</p><blockquote><p>first</p>\
             <blockquote><p>older</p></blockquote></blockquote>"
        );
        quote.head = QuoteHead::Forward(vec![(ForwardField::From, "A <a@example.com>".into())]);
        quote.html = Some("<style>p { color: red }</style><p><b>Hi</b></p>".into());
        assert_eq!(
            quote_to_html(&quote),
            "<p></p><p>---------- Forwarded message ---------</p>\
             <p><b>From:</b> A &lt;a@example.com&gt;</p><p></p><p><b>Hi</b></p>"
        );
    }

    #[test]
    fn the_text_keeps_lines_and_quotes() {
        let doc = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "Thanks!"}]},
            {"type": "paragraph"},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "See "},
                {"type": "text", "text": "this", "marks": [
                    {"type": "link", "attrs": {"href": "https://example.com/"}},
                    {"type": "bold"},
                ]},
                {"type": "hardBreak"},
                {"type": "image", "attrs": {"src": "cid:a@catmail"}},
                {"type": "text", "text": "and that"},
            ]},
            {"type": "orderedList", "attrs": {"start": 3}, "content": [
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "one"}]},
                    {"type": "bulletList", "content": [{"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "inner"}]},
                    ]}]},
                ]},
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "two"}]},
                ]},
            ]},
            {"type": "blockquote", "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "first"}]},
                {"type": "blockquote", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "older"}]},
                ]},
            ]},
            {"type": "paragraph"},
        ]});
        assert_eq!(
            doc_to_text(&doc),
            "Thanks!\n\nSee this <https://example.com/>\nand that\n3. one\n   * inner\n\
             4. two\n> first\n>> older\n"
        );
        assert_eq!(
            doc_to_text(&json!({"type": "doc", "content": [{"type": "paragraph"}]})),
            ""
        );
    }

    #[test]
    fn typed_addresses_become_links() {
        assert_eq!(link_href("  "), None);
        for (typed, href) in [
            ("https://example.com/a", "https://example.com/a"),
            (" example.com ", "https://example.com"),
            ("example.com:8443/x", "https://example.com:8443/x"),
            ("a@example.com", "mailto:a@example.com"),
            ("mailto:a@example.com", "mailto:a@example.com"),
            ("tel:+861234", "tel:+861234"),
            ("tel:5551234", "tel:5551234"),
            ("tel:110", "tel:110"),
            ("TEL:110", "TEL:110"),
            ("localhost:8080", "https://localhost:8080"),
            ("LOCALHOST:8080", "https://LOCALHOST:8080"),
            ("intranet:8080/help", "https://intranet:8080/help"),
            ("intranet:8080?q=1", "https://intranet:8080?q=1"),
            ("geo:37.78,-122.4", "geo:37.78,-122.4"),
            ("javascript:alert(1)", "javascript:alert(1)"),
        ] {
            assert_eq!(link_href(typed).as_deref(), Some(href), "{typed}");
        }
    }

    #[test]
    fn only_the_drafts_images_and_data_urls_are_shown() {
        let mut images = Images::default();
        images.insert("a@catmail".into(), PathBuf::from("/blobs/aa/aaaa"));
        assert_eq!(images.path("a@catmail"), Some(Path::new("/blobs/aa/aaaa")));
        assert_eq!(
            images.shown("cid:a@catmail").as_deref(),
            Some("/blobs/aa/aaaa")
        );
        let data = "data:image/png;base64,aGk=";
        assert_eq!(images.shown(data).as_deref(), Some(data));
        // Loaded from nowhere, not even a file the editor shows another image from.
        for src in [
            "cid:other@x",
            "https://example.com/x.png",
            "/blobs/aa/aaaa",
            "file:///blobs/aa/aaaa",
            "catmail-image:3/cid/a%40b",
            "",
        ] {
            assert_eq!(images.shown(src), None, "{src}");
        }
    }

    #[test]
    fn pasted_images_keep_only_what_the_draft_can_send() {
        let mut images = Images::default();
        images.insert("a@catmail".into(), PathBuf::from("/blobs/aa/aaaa"));
        let image = |src: &str| json!({"type": "image", "attrs": {"src": src}});
        let sendable = [
            "cid:a@catmail",
            "data:image/png;base64,aGk=",
            "https://t.example/open.gif?u=1",
            "HTTP://t.example/b.gif",
        ];
        let unsendable = [
            // Another message's part, the reader's, files.
            "cid:other@x",
            "catmail-image:3/cid/a%40b",
            "catmail-image:3/remote/https%3A%2F%2Fx%2Fa.gif",
            "/blobs/aa/aaaa",
            "/Users/me/secret.png",
            "file:///Users/me/secret.png",
            "images/a.png",
            "javascript:alert(1)",
        ];
        let mut slice = json!({"content": [{"type": "paragraph", "content":
            unsendable.iter().chain(&sendable).map(|src| image(src)).collect::<Vec<_>>()}],
            "openStart": 1});
        images.drop_unsendable(&mut slice);
        let srcs: Vec<&str> = slice["content"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["attrs"]["src"].as_str().unwrap())
            .collect();
        assert_eq!(srcs, sendable);
        assert_eq!(slice["openStart"], 1);
    }
}
