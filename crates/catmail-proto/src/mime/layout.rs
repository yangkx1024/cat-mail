//! Layout tables, for renderers that can't lay them out.

use std::cell::RefCell;

use html5ever::serialize::{HtmlSerializer, SerializeOpts, Serializer as _};
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
/// has at most one cell per row; other tables, which hold data, stay. `center` becomes a
/// `div` too: it is a block, and newsletters center their layout tables with it.
pub fn unwrap_layout_tables(html: &str) -> String {
    let items = tokenize(html);
    let layout = layout_tables(&items);
    // Only element names change, but the result goes through the sanitizer again so this
    // pass can't hand a renderer anything the sanitizer wouldn't.
    sanitize_html(&rewrite(&items, &layout))
}

enum Item {
    Start(Tag),
    End(LocalName),
    Text(String),
}

/// The sanitizer's output is serialized from a DOM, so its tags nest properly and a
/// token stream is enough: no tree builder, which would move content around again.
fn tokenize(html: &str) -> Vec<Item> {
    #[derive(Default)]
    struct Collect(RefCell<Vec<Item>>);

    impl TokenSink for Collect {
        type Handle = ();

        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
            let mut items = self.0.borrow_mut();
            match token {
                Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                    items.push(Item::Start(tag));
                }
                Token::TagToken(tag) => items.push(Item::End(tag.name)),
                Token::CharacterTokens(text) => match items.last_mut() {
                    Some(Item::Text(last)) => last.push_str(&text),
                    _ => items.push(Item::Text(text.to_string())),
                },
                // The sanitizer leaves no comments or doctypes.
                _ => {}
            }
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

fn rewrite(items: &[Item], layout: &[bool]) -> String {
    let html = |name: LocalName| QualName::new(None, ns!(html), name);
    let mut out = Vec::new();
    let mut serializer = HtmlSerializer::new(&mut out, SerializeOpts::default());
    // Open elements as written (`None` for one left out), and whether each is a table.
    let mut open: Vec<(LocalName, Option<LocalName>, bool)> = Vec::new();
    // Whether each open table is a layout table, innermost last.
    let mut tables: Vec<bool> = Vec::new();
    let mut next_table = 0;
    // Writing to a `Vec` can't fail.
    for item in items {
        match item {
            Item::Start(tag) => {
                let is_table = tag.name == local_name!("table");
                if is_table {
                    tables.push(layout.get(next_table).copied().unwrap_or(false));
                    next_table += 1;
                }
                let in_layout = tables.last().copied().unwrap_or(false);
                let written = match tag.name {
                    local_name!("center") => Some(local_name!("div")),
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
                    serializer.start_elem(html(name.clone()), attrs).unwrap();
                }
                if is_void(&tag.name) {
                    // The serializer pairs every start, even of a void element, with an end.
                    if let Some(name) = written {
                        serializer.end_elem(html(name)).unwrap();
                    }
                    if is_table {
                        tables.pop();
                    }
                } else {
                    open.push((tag.name.clone(), written, is_table));
                }
            }
            Item::End(name) => {
                let Some(at) = open.iter().rposition(|(open, ..)| open == name) else {
                    continue;
                };
                for (_, written, is_table) in open.drain(at..).rev() {
                    if let Some(name) = written {
                        serializer.end_elem(html(name)).unwrap();
                    }
                    if is_table {
                        tables.pop();
                    }
                }
            }
            Item::Text(text) => serializer.write_text(text).unwrap(),
        }
    }
    for (_, written, _) in open.into_iter().rev() {
        if let Some(name) = written {
            serializer.end_elem(html(name)).unwrap();
        }
    }
    drop(serializer);
    String::from_utf8(out).expect("the serializer writes UTF-8")
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
        assert_eq!(unwrap_layout_tables(&data), data);
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
    fn html_without_tables_is_unchanged() {
        let html = sanitize_html("<p>你好 <b>world</b></p><ul><li>x</li></ul><img src=\"cid:a\">");
        assert_eq!(unwrap_layout_tables(&html), html);
    }
}
