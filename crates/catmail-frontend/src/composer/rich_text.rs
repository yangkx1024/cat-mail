//! The composer's formatted text, without the editor: the HTML a plain draft opens as,
//! the plain text sent with the HTML, and the images, which the editor shows from files
//! and the draft refers to by Content-ID.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::html::escape;

/// The images of a draft's HTML: the file each one's content is in
/// ([`DraftEditor::images`](catmail_core::DraftEditor::images)), by Content-ID. The
/// editor shows the files; the draft keeps `cid:` URLs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Images {
    paths: HashMap<String, PathBuf>,
    /// The other way: Content-IDs by the path the editor shows.
    ids: HashMap<String, String>,
}

impl Images {
    pub fn insert(&mut self, content_id: String, path: PathBuf) {
        self.ids
            .insert(path.to_string_lossy().into_owned(), content_id.clone());
        self.paths.insert(content_id, path);
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The images of `doc` (ProseMirror's JSON of a document) by their files, for the
    /// editor: `cid:` URLs of the draft's images become their paths. Others stay.
    pub fn to_files(&self, doc: &mut Value) {
        map_image_sources(doc, &|src| {
            let id = src.strip_prefix("cid:")?;
            Some(self.paths.get(id)?.to_string_lossy().into_owned())
        });
    }

    /// The images of `doc` by Content-ID, for the draft: the paths [`Self::to_files`]
    /// gave become `cid:` URLs again.
    pub fn to_content_ids(&self, doc: &mut Value) {
        map_image_sources(doc, &|src| Some(format!("cid:{}", self.ids.get(src)?)));
    }
}

/// Replaces the `src` of every image node of `doc` that `f` has a replacement for.
fn map_image_sources(doc: &mut Value, f: &dyn Fn(&str) -> Option<String>) {
    let Value::Object(node) = doc else {
        return;
    };
    if node.get("type").and_then(Value::as_str) == Some("image")
        && let Some(Value::Object(attrs)) = node.get_mut("attrs")
        && let Some(new) = attrs.get("src").and_then(Value::as_str).and_then(f)
    {
        attrs.insert("src".into(), Value::String(new));
    }
    if let Some(Value::Array(content)) = node.get_mut("content") {
        for child in content {
            map_image_sources(child, f);
        }
    }
}

/// The HTML a plain text opens as in the editor: a paragraph per line, and the lines
/// quoted with `>` (a reply's original) in a quote, nested as they are.
pub fn text_to_html(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut html = String::new();
    blocks(&lines, &mut html);
    html
}

fn blocks(lines: &[&str], html: &mut String) {
    let mut i = 0;
    while i < lines.len() {
        if quoted(lines[i]).is_some() {
            let inner: Vec<&str> = lines[i..].iter().map_while(|line| quoted(line)).collect();
            i += inner.len();
            html.push_str("<blockquote>");
            blocks(&inner, html);
            html.push_str("</blockquote>");
        } else {
            html.push_str("<p>");
            html.push_str(&escape(lines[i]));
            html.push_str("</p>");
            i += 1;
        }
    }
}

/// The line without its quote mark (`>` and the space after it), if it is quoted.
fn quoted(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// The plain text sent with the editor's document (ProseMirror's JSON), for readers
/// that show only text; as [`text_to_html`] reads it: a line per paragraph, `> ` before
/// quoted lines. List items start with `* ` or their number, a link's address follows
/// its text, images are left out.
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

/// The lines of a block, added to `lines`.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn plain_text_opens_as_paragraphs_and_quotes() {
        assert_eq!(text_to_html(""), "");
        assert_eq!(
            text_to_html("Hi <you> & co\n\nOn Monday, A wrote:\n> first\n>> older\n> last\nbye"),
            "<p>Hi &lt;you&gt; &amp; co</p><p></p><p>On Monday, A wrote:</p>\
             <blockquote><p>first</p><blockquote><p>older</p></blockquote><p>last</p>\
             </blockquote><p>bye</p>"
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
    fn images_go_between_content_ids_and_files() {
        let mut images = Images::default();
        images.insert("a@catmail".into(), PathBuf::from("/blobs/aa/aaaa"));
        let draft = json!({"type": "doc", "content": [{"type": "paragraph", "content": [
            {"type": "image", "attrs": {"src": "cid:a@catmail", "alt": null}},
            {"type": "image", "attrs": {"src": "cid:other@x"}},
            {"type": "image", "attrs": {"src": "https://example.com/x.png"}},
        ]}]});
        let mut doc = draft.clone();
        images.to_files(&mut doc);
        assert_eq!(
            doc["content"][0]["content"][0]["attrs"]["src"],
            "/blobs/aa/aaaa"
        );
        assert_eq!(
            doc["content"][0]["content"][1]["attrs"]["src"],
            "cid:other@x"
        );
        images.to_content_ids(&mut doc);
        assert_eq!(doc, draft);
    }
}
