//! Layout tables and other blocks, for renderers that can't lay them out.

use std::borrow::Cow;
use std::cell::RefCell;
use std::io::Write;

use html5ever::serialize::{HtmlSerializer, SerializeOpts, Serializer as _};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::{LocalName, QualName, local_name, ns};

use super::html::sanitize_html;

/// Elements that start a block: inside a table cell, they mean the table lays out a page
/// rather than holding data.
const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
];

/// Blocks that a simple view lays out as inline text, so they become `div`s wherever they
/// are. Checked against GPUI Kit 0.7.3's text view, whose block list has `div` but not
/// these; newsletters center their layout tables with `center`.
const INLINE_BLOCKS: &[&str] = &["center", "dd", "dt", "hgroup"];

/// Elements without an end tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// HTML that [`sanitize_html`](super::sanitize_html) produced, rewritten for a renderer
/// that lays a table cell out as one run of inline text (a simple HTML view, not a
/// browser). Most newsletters wrap the whole body in tables whose cells hold headings,
/// paragraphs, lists and more tables; such a renderer flattens all of it into one
/// paragraph. A table becomes nested `div`s when one of its cells holds a block or it
/// has at most one cell per row; other tables, which hold data, stay. The blocks such a
/// renderer takes for inline elements (`center`, `dd`, `dt`, `hgroup`) become `div`s too.
/// Borrows `html` when nothing changes, which is most mail that isn't a newsletter.
pub fn unwrap_layout_tables(html: &str) -> Cow<'_, str> {
    // The sanitizer writes tag names in lower case.
    let mentions = |name: &str| html.contains(&format!("<{name}"));
    if !mentions("table") && !INLINE_BLOCKS.iter().any(|name| mentions(name)) {
        return Cow::Borrowed(html);
    }
    let items = tokenize(html);
    let layout = layout_tables(&items);
    let renames = layout.contains(&true)
        || items
            .iter()
            .any(|item| matches!(item, Item::Start(tag) if is_inline_block(&tag.name)));
    if !renames {
        return Cow::Borrowed(html);
    }
    // Only element names change, but the result goes through the sanitizer again so this
    // pass can't hand a renderer anything the sanitizer wouldn't.
    Cow::Owned(sanitize_html(&rewrite(&items, &layout)))
}

enum Item {
    Start(Tag),
    End(LocalName),
    Text(StrTendril),
}

/// The sanitizer's output is serialized from a DOM, so its tags nest properly and a
/// token stream is enough: no tree builder, which would move content around again.
fn tokenize(html: &str) -> Vec<Item> {
    #[derive(Default)]
    struct Collect(RefCell<Vec<Item>>);

    impl TokenSink for Collect {
        type Handle = ();

        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
            let item = match token {
                Token::TagToken(tag) if tag.kind == TagKind::StartTag => Item::Start(tag),
                Token::TagToken(tag) => Item::End(tag.name),
                Token::CharacterTokens(text) => Item::Text(text),
                // The sanitizer leaves no comments or doctypes.
                _ => return TokenSinkResult::Continue,
            };
            self.0.borrow_mut().push(item);
            TokenSinkResult::Continue
        }
    }

    let input = BufferQueue::default();
    input.push_back(html.into());
    let tokenizer = Tokenizer::new(Collect::default(), TokenizerOpts::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}

fn is_void(name: &LocalName) -> bool {
    VOID_ELEMENTS.contains(&&**name)
}

fn is_inline_block(name: &LocalName) -> bool {
    INLINE_BLOCKS.contains(&&**name)
}

/// Whether each table, in document order, is a layout table.
fn layout_tables(items: &[Item]) -> Vec<bool> {
    #[derive(Default)]
    struct Table {
        has_block: bool,
        cells_in_row: usize,
        max_cells: usize,
    }

    let mut tables: Vec<Table> = Vec::new();
    // Open elements, each with its table's index if it is a table.
    let mut open: Vec<(LocalName, Option<usize>)> = Vec::new();
    for item in items {
        match item {
            Item::Start(tag) => {
                // The innermost open table, and whether a cell of it is open.
                let mut owner = None;
                let mut in_cell = false;
                for (name, table) in open.iter().rev() {
                    if let Some(table) = table {
                        owner = Some(*table);
                        break;
                    }
                    in_cell |= matches!(*name, local_name!("td") | local_name!("th"));
                }
                if let Some(table) = owner.map(|t| &mut tables[t]) {
                    match tag.name {
                        _ if in_cell && BLOCK_ELEMENTS.contains(&&*tag.name) => {
                            table.has_block = true;
                        }
                        local_name!("tr") => table.cells_in_row = 0,
                        local_name!("td") | local_name!("th") => {
                            table.cells_in_row += 1;
                            table.max_cells = table.max_cells.max(table.cells_in_row);
                        }
                        _ => {}
                    }
                }
                let index = (tag.name == local_name!("table")).then(|| {
                    tables.push(Table::default());
                    tables.len() - 1
                });
                if !is_void(&tag.name) {
                    open.push((tag.name.clone(), index));
                }
            }
            Item::End(name) => {
                if let Some(at) = open.iter().rposition(|(open, _)| open == name) {
                    open.truncate(at);
                }
            }
            Item::Text(_) => {}
        }
    }
    tables
        .into_iter()
        .map(|t| t.has_block || t.max_cells <= 1)
        .collect()
}

/// An element `rewrite` has opened.
struct Open {
    name: LocalName,
    /// The name it was written with, `None` if it was left out.
    written: Option<LocalName>,
    /// For a table, whether it is a layout table.
    layout_table: Option<bool>,
}

fn rewrite(items: &[Item], layout: &[bool]) -> String {
    let mut out = Vec::new();
    let mut serializer = HtmlSerializer::new(&mut out, SerializeOpts::default());
    let mut open: Vec<Open> = Vec::new();
    let mut layout = layout.iter().copied();
    for item in items {
        match item {
            Item::Start(tag) => {
                let layout_table = (tag.name == local_name!("table"))
                    .then(|| layout.next().expect("one flag per table"));
                let in_layout = layout_table
                    .or_else(|| open.iter().rev().find_map(|o| o.layout_table))
                    .unwrap_or(false);
                let written = match tag.name {
                    _ if is_inline_block(&tag.name) => Some(local_name!("div")),
                    local_name!("table")
                    | local_name!("caption")
                    | local_name!("thead")
                    | local_name!("tbody")
                    | local_name!("tfoot")
                    | local_name!("tr")
                    | local_name!("td")
                    | local_name!("th")
                        if in_layout =>
                    {
                        Some(local_name!("div"))
                    }
                    // Column widths of a table that is gone.
                    local_name!("colgroup") | local_name!("col") if in_layout => None,
                    _ => Some(tag.name.clone()),
                };
                if let Some(name) = &written {
                    let attrs = tag.attrs.iter().map(|a| (&a.name, &*a.value));
                    serializer.start_elem(html_name(name), attrs).unwrap();
                }
                open.push(Open {
                    name: tag.name.clone(),
                    written,
                    layout_table,
                });
                // The serializer pairs every start, even of a void element, with an end.
                if is_void(&tag.name) {
                    let last = open.len() - 1;
                    close(&mut serializer, &mut open, last);
                }
            }
            Item::End(name) => {
                if let Some(at) = open.iter().rposition(|o| o.name == *name) {
                    close(&mut serializer, &mut open, at);
                }
            }
            Item::Text(text) => serializer.write_text(text).unwrap(),
        }
    }
    close(&mut serializer, &mut open, 0);
    drop(serializer);
    String::from_utf8(out).expect("the serializer writes UTF-8")
}

/// Ends the open elements from `at` on, innermost first. Writing to a `Vec` can't fail.
fn close(serializer: &mut HtmlSerializer<impl Write>, open: &mut Vec<Open>, at: usize) {
    for element in open.drain(at..).rev() {
        if let Some(name) = element.written {
            serializer.end_elem(html_name(&name)).unwrap();
        }
    }
}

fn html_name(name: &LocalName) -> QualName {
    QualName::new(None, ns!(html), name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_tables_become_blocks_and_data_tables_stay() {
        let html = sanitize_html(
            r##"<center><table width="600" bgcolor="#fff"><tr><td align="center">
            <h1>标题</h1><p>第一段</p>
            <table><tr><th>功能</th><th>状态</th></tr><tr><td>搜索</td><td>已发布</td></tr></table>
            <ul><li>一</li></ul></td></tr></table></center>"##,
        );
        let unwrapped = unwrap_layout_tables(&html);
        assert!(!unwrapped.contains("<center"), "{unwrapped}");
        assert!(
            unwrapped.contains(r##"<div width="600" bgcolor="#fff">"##),
            "attributes stay: {unwrapped}"
        );
        assert!(unwrapped.contains(r#"<div align="center">"#), "{unwrapped}");
        assert!(
            unwrapped.contains("<h1>标题</h1><p>第一段</p>"),
            "{unwrapped}"
        );
        assert!(unwrapped.contains("<ul><li>一</li></ul>"), "{unwrapped}");
        // The data table inside is the only table left, intact.
        assert_eq!(unwrapped.matches("<table").count(), 1, "{unwrapped}");
        assert!(
            unwrapped.contains(
                "<table><tbody><tr><th>功能</th><th>状态</th></tr>\
                 <tr><td>搜索</td><td>已发布</td></tr></tbody></table>"
            ),
            "{unwrapped}"
        );
    }

    #[test]
    fn one_column_tables_are_layout_but_inline_cells_are_data() {
        let one_column =
            sanitize_html("<table><tr><td>a<br>b</td></tr><tr><td>c</td></tr></table>");
        assert_eq!(
            unwrap_layout_tables(&one_column),
            "<div><div><div><div>a<br>b</div></div><div><div>c</div></div></div></div>"
        );
        let data = sanitize_html(
            "<table><colgroup><col width=\"50%\"></colgroup>\
             <tr><td><b>a</b> <a href=\"https://example.com\">x</a></td><td>b</td></tr></table>",
        );
        assert!(matches!(unwrap_layout_tables(&data), Cow::Borrowed(_)));
        let layout = sanitize_html(
            "<table><colgroup><col width=\"50%\"></colgroup><tr><td><p>a</p></td><td>b</td></tr></table>",
        );
        assert_eq!(
            unwrap_layout_tables(&layout),
            "<div><div><div><div><p>a</p></div><div>b</div></div></div></div>"
        );
    }

    #[test]
    fn text_and_attributes_stay_escaped() {
        let html = sanitize_html(
            "<table><tr><td><p title=\"&quot;&gt;<script>\">1 &lt; 2 &amp;&amp; \
             &lt;script&gt;alert(1)&lt;/script&gt;</p>\
             <img src=\"cid:a&quot;b\" alt=\"x\"></td></tr></table>",
        );
        let unwrapped = unwrap_layout_tables(&html);
        assert!(!unwrapped.contains("<script"), "{unwrapped}");
        assert!(
            unwrapped.contains("1 &lt; 2 &amp;&amp; &lt;script&gt;alert(1)&lt;/script&gt;"),
            "{unwrapped}"
        );
        assert!(
            unwrapped.contains(r#"title="&quot;&gt;&lt;script&gt;""#),
            "{unwrapped}"
        );
        assert!(unwrapped.contains(r#"src="cid:a&quot;b""#), "{unwrapped}");
    }

    #[test]
    fn html_without_tables_is_borrowed() {
        let html = sanitize_html("<p>你好 <b>world</b></p><ul><li>x</li></ul><img src=\"cid:a\">");
        assert!(matches!(unwrap_layout_tables(&html), Cow::Borrowed(_)));
    }

    #[test]
    fn blocks_taken_for_inline_become_divs() {
        let html = sanitize_html(
            "<hgroup><h1>a</h1></hgroup><dl><dt>b</dt><dd class=\"x\">c</dd></dl><center>d</center>",
        );
        assert_eq!(
            unwrap_layout_tables(&html),
            "<div><h1>a</h1></div><dl><div>b</div><div class=\"x\">c</div></dl><div>d</div>"
        );
    }
}
