//! The text of a message's HTML as a reader shows it, for the stored text, snippets,
//! search and agents: laid out by its DOM (a paragraph, table cell or list item to a
//! line), with what its style sheets hide (`display: none`) left out. And the text it
//! hides in front of that: the line that mail written for message lists puts there for
//! the list (a "preheader"), which readers don't show but other mail clients' lists do.

use std::rc::Rc;

use html2text::RcDom;
use html2text::config;
use html2text::render::{TaggedLine, TextDecorator};

/// Wider than any line of mail, so that none wraps: a line of the text is a line of the
/// reader's. Not `usize::MAX`, which html2text adds to.
const WIDTH: usize = 1 << 20;

/// The text a reader shows of `html`, which [`sanitize_html`](crate::sanitize_html)
/// produced. Laid out as mail clients write the plain text of their HTML: a line per
/// paragraph, table cell and list item, `* ` and `1. ` before list items, `> ` before
/// quoted lines, a link's address after its text (`text <url>`) unless the text is the
/// address; no marks for headings or emphasis, nothing for images. Empty if nothing
/// is shown, or if the HTML can't be laid out.
pub fn text(html: &str) -> String {
    parse(html).map_or_else(String::new, |dom| shown(&dom).0)
}

/// [`text`] of `html`, and the text `html` hides in front of it (see [`preheader`]),
/// parsed once.
pub fn text_and_preheader(html: &str) -> (String, Option<String>) {
    let Some(dom) = parse(html) else {
        return (String::new(), None);
    };
    let (shown, styled) = shown(&dom);
    // Without style that hides text (html2text hides on `display`, and on a height of
    // zero with `overflow`), the HTML shows all it has.
    if !styled
        || !["display", "overflow"]
            .iter()
            .any(|p| contains_ignoring_case(html, p))
    {
        return (shown, None);
    }
    let preheader = render(&dom, false)
        .ok()
        .and_then(|all| preheader(&all, &shown));
    (shown, preheader)
}

/// The lines of `all` (every text of the HTML, hidden or not) before the first that
/// `shown` has: what the HTML hides in front of what it shows. That is where mail for
/// lists puts its preheader, hidden from readers, followed by a run of invisible
/// characters that keeps the start of the body out of the list's line. On one line,
/// without those characters ([`visible`]) and without unfilled mailing-list fields
/// (`*|MC_PREVIEW_TEXT|*`); `None` if nothing is left.
fn preheader(all: &str, shown: &str) -> Option<String> {
    let lead = match shown.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(first) => {
            let mut lines = all.split_inclusive('\n').scan(0, |end, line| {
                *end += line.len();
                Some((*end - line.len(), line))
            });
            // Laid out differently than shown: what is hidden can't be told apart.
            let (end, _) = lines.find(|(_, line)| line.trim() == first)?;
            &all[..end]
        }
        None => all,
    };
    let lead = visible(lead);
    let words: Vec<&str> = lead
        .split_whitespace()
        .filter(|word| !(word.starts_with("*|") && word.ends_with("|*") && word.len() > 3))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// `s` without the characters that take no room when shown: zero-width spaces and
/// joiners, soft hyphens, direction marks and the like, which mail for lists repeats
/// after a preheader to keep the body out of the list's line. A zero-width joiner stays
/// between two characters it joins, and a variation selector after the character it
/// selects for: emoji are made of them.
pub fn visible(s: &str) -> String {
    let shown = |c: char| !c.is_whitespace() && c != JOINER && !is_selector(c);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if is_invisible(c) {
            continue;
        }
        let after_shown = out
            .chars()
            .next_back()
            .is_some_and(|p| shown(p) || is_selector(p));
        let keep = if is_selector(c) {
            after_shown
        } else if c == JOINER {
            after_shown && chars.clone().find(|&n| !is_invisible(n)).is_some_and(shown)
        } else {
            true
        };
        if keep {
            out.push(c);
        }
    }
    out
}

const JOINER: char = '\u{200D}';

fn is_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}')
}

/// Whether `c` takes no room and joins nothing (see [`visible`]).
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
    )
}

/// A web or mail address split after its scheme (`https://`, `http://` or `mailto:`, in
/// any case), as plain text writes the addresses of links.
pub fn split_address(s: &str) -> Option<(&'static str, &str)> {
    ["https://", "http://", "mailto:"]
        .into_iter()
        .find_map(|scheme| {
            s.get(..scheme.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(scheme))
                .map(|_| (scheme, &s[scheme.len()..]))
        })
}

fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn parse(html: &str) -> Option<RcDom> {
    config::plain().parse_html(html.as_bytes()).ok()
}

/// The text of `dom` with what its style sheets and `style` attributes hide left out,
/// and whether they were: style it can't read is ignored rather than losing the text.
fn shown(dom: &RcDom) -> (String, bool) {
    match render(dom, true) {
        Ok(text) => (text, true),
        Err(_) => (render(dom, false).unwrap_or_default(), false),
    }
}

/// The text of `dom`, laid out with its style if `doc_css`.
fn render(dom: &RcDom, doc_css: bool) -> Result<String, html2text::Error> {
    // Tables a cell to a line, without borders.
    let mut config = config::with_decorator(Plain).raw_mode(true);
    if doc_css {
        config = config.use_doc_css();
    }
    let tree = config.dom_to_render_tree(dom)?;
    Ok(write_lines(&config.render_to_lines(tree, WIDTH)?))
}

/// The link a piece of text is in, if its address is to be written. Equal only to
/// itself, not to another link to the same address: html2text gives it to every piece
/// of the link's text, in each block inside the link.
#[derive(Clone, Debug, Default)]
struct Link(Option<Rc<str>>);

impl PartialEq for Link {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (a, b) => a.is_none() && b.is_none(),
        }
    }
}

impl Eq for Link {}

/// Writes plain text, with marks only where a reader of plain text needs them. Links
/// are annotated, and their addresses written once their text is known
/// ([`write_lines`]).
#[derive(Clone, Copy, Debug)]
struct Plain;

impl TextDecorator for Plain {
    type Annotation = Link;

    fn decorate_link_start(&mut self, url: &str) -> (String, Link) {
        // An address to read: not a part of the message (`cid:`) or a place in it.
        let shown = url
            .split_once(':')
            .is_some_and(|(scheme, _)| !scheme.eq_ignore_ascii_case("cid"));
        (String::new(), Link(shown.then(|| url.into())))
    }

    fn decorate_link_end(&mut self) -> String {
        String::new()
    }

    fn decorate_em_start(&self) -> (String, Link) {
        (String::new(), Link::default())
    }

    fn decorate_em_end(&self) -> String {
        String::new()
    }

    fn decorate_strong_start(&self) -> (String, Link) {
        (String::new(), Link::default())
    }

    fn decorate_strong_end(&self) -> String {
        String::new()
    }

    fn decorate_strikeout_start(&self) -> (String, Link) {
        (String::new(), Link::default())
    }

    fn decorate_strikeout_end(&self) -> String {
        String::new()
    }

    fn decorate_code_start(&self) -> (String, Link) {
        (String::new(), Link::default())
    }

    fn decorate_code_end(&self) -> String {
        String::new()
    }

    fn decorate_preformat_first(&self) -> Link {
        Link::default()
    }

    fn decorate_preformat_cont(&self) -> Link {
        Link::default()
    }

    /// Nothing: an image's text (`alt`) shows only when the image doesn't.
    fn decorate_image(&mut self, _src: &str, _title: &str) -> (String, Link) {
        (String::new(), Link::default())
    }

    fn header_prefix(&self, _level: usize) -> String {
        String::new()
    }

    fn quote_prefix(&self) -> String {
        "> ".into()
    }

    fn unordered_item_prefix(&self) -> String {
        "* ".into()
    }

    fn ordered_item_prefix(&self, i: i64) -> String {
        format!("{i}. ")
    }

    fn make_subblock_decorator(&self) -> Self {
        Plain
    }

    fn decorate_superscript_start(&self) -> (String, Link) {
        (String::new(), Link::default())
    }

    fn decorate_superscript_end(&self) -> String {
        String::new()
    }

    /// No list of links at the end: each address follows its text.
    fn finalise(&mut self, _urls: Vec<String>) -> Vec<TaggedLine<Link>> {
        Vec::new()
    }
}

/// The lines html2text laid out, joined, with each link's address after its text, as
/// `text <url>`, unless the text is the address or there is none (a linked image).
fn write_lines(lines: &[TaggedLine<Vec<Link>>]) -> String {
    /// A link's address, and where its text starts and ends in `out`.
    type Open = (Rc<str>, usize, usize);
    fn close(out: &mut String, link: Option<Open>) {
        let Some((url, start, end)) = link else {
            return;
        };
        let text = out[start..end].trim();
        if !text.is_empty() && !same_address(text, &url) {
            let end = start + out[start..end].trim_end().len();
            out.insert_str(end, &format!(" <{url}>"));
        }
    }
    let mut out = String::new();
    let mut open: Option<Open> = None;
    for line in lines {
        for piece in line.tagged_strings() {
            let link = piece.tag.iter().rev().find_map(|link| link.0.as_ref());
            let same =
                matches!((&open, link), (Some((url, ..)), Some(link)) if Rc::ptr_eq(url, link));
            if !same {
                close(&mut out, open.take());
                open = link.map(|link| (link.clone(), out.len(), out.len()));
            }
            out.push_str(&piece.s);
            if let Some((_, _, end)) = &mut open {
                *end = out.len();
            }
        }
        out.push('\n');
    }
    close(&mut out, open);
    out
}

/// Whether a link's text is its address, written as people write addresses: without the
/// scheme, a trailing `/`, or a mail link's fields (`?subject=…`).
fn same_address(text: &str, url: &str) -> bool {
    fn bare(address: &str) -> &str {
        let address = address.trim();
        let address = match split_address(address) {
            Some(("mailto:", rest)) => rest.split('?').next().unwrap_or_default(),
            Some((_, rest)) => rest,
            None => address,
        };
        address.trim_end_matches('/')
    }
    bare(text).eq_ignore_ascii_case(bare(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize_html;

    fn shown(html: &str) -> String {
        text(&sanitize_html(html))
    }

    #[test]
    fn leaves_out_what_style_sheets_and_attributes_hide() {
        let html = "<html><head><title>Receipt</title>\
            <style type=\"text/css\">.pre { display: none !important } p { color: red }</style>\
            </head><body>\
            <div style=\"display:none;max-height:0;overflow:hidden\">Inline &zwnj;&#847;</div>\
            <span class=\"pre\">*|MC_PREVIEW_TEXT|*</span>\
            <p>Your order</p>\
            <style media=\"print\">.screen { display: none }</style>\
            <p class=\"screen\">ships today</p></body></html>";
        assert_eq!(shown(html), "Your order\n\nships today\n");
    }

    #[test]
    fn lays_out_blocks_lists_tables_and_quotes() {
        let html = "<h1>Invoice</h1>\
            <table><tr><td>Item</td><td>$5</td></tr></table>\
            <ul><li>one</li><li>two</li></ul><ol><li>first</li></ol>\
            <div>a</div><div>b<br>c</div>\
            <p><img src=\"cid:logo\" alt=\"Logo\"><b>bold</b> <em>em</em> x<sup>2</sup></p>\
            <blockquote type=\"cite\">quoted<blockquote>again</blockquote></blockquote>";
        assert_eq!(
            shown(html),
            "Invoice\n\nItem\n$5\n* one\n* two\n1. first\na\nb\nc\n\nbold em x²\n\n\
             > quoted\n> \n> > again\n"
        );
    }

    #[test]
    fn writes_a_link_address_after_its_text() {
        let html = "<p>Read <a href=\"https://example.com/a?b=c\">the post</a>.</p>\
            <p><a href=\"https://example.com/x/\">example.com/x</a></p>\
            <p><a href=\"mailto:me@example.com?subject=Hi\">me@example.com</a></p>\
            <p><a href=\"https://example.com/img\"><img src=\"cid:i\" alt=\"Shop\"></a></p>\
            <p><a href=\"cid:part\">part</a></p>\
            <a href=\"https://example.com/b\"><div>Block</div><div>link</div></a>";
        assert_eq!(
            shown(html),
            "Read the post <https://example.com/a?b=c>.\n\nexample.com/x\n\nme@example.com\n\n\
             \npart\n\nBlock\nlink <https://example.com/b>\n"
        );
    }

    #[test]
    fn visible_keeps_what_joins_emoji() {
        assert_eq!(visible("plain"), "plain");
        assert_eq!(visible("a\u{200c}\u{34f}b\u{ad}c\u{feff}"), "abc");
        // Joiners and selectors as filler, between spaces or on their own.
        assert_eq!(visible("x \u{200d} \u{200d}\u{fe0f} \u{200d}y"), "x   y");
        // Emoji: a joiner between two, a selector after one, both.
        for emoji in [
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{2764}\u{fe0f}",
            "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
        ] {
            assert_eq!(visible(&format!("{emoji}\u{200c}")), emoji);
        }
    }

    #[test]
    fn preheader_is_what_is_hidden_in_front() {
        let html = sanitize_html(
            "<style>.p{display:none}</style>\
             <div class=\"p\">Up to 50% off this week</div>\
             <div style=\"display:none\">&zwnj;&#847; &zwnj;&#847; &#8203;</div>\
             <p>Dear reader,</p><p>Our sale</p>",
        );
        let (text, preheader) = text_and_preheader(&html);
        assert_eq!(text, "Dear reader,\n\nOur sale\n");
        assert_eq!(preheader.as_deref(), Some("Up to 50% off this week"));

        // An unfilled field is no preheader.
        let html =
            sanitize_html("<span style=\"display:none\">*|MC_PREVIEW_TEXT|*</span><p>Hi</p>");
        assert_eq!(text_and_preheader(&html).1, None);
    }

    #[test]
    fn no_preheader_without_hidden_text_in_front() {
        for html in [
            "<p>Hello</p><p>there</p>",
            // Hidden later on, not in front.
            "<p>Hello</p><p style=\"display:none\">hidden</p><p>there</p>",
            // Only characters that take no room.
            "<div style=\"display:none\">&zwnj;&#847;&shy;</div><p>Hello</p>",
        ] {
            let (text, preheader) = text_and_preheader(&sanitize_html(html));
            assert!(text.starts_with("Hello"), "{html}");
            assert_eq!(preheader, None, "{html}");
        }
        // Nothing shown at all: what is hidden is the whole preheader.
        let (text, preheader) =
            text_and_preheader(&sanitize_html("<div style=\"display:none\">Preview</div>"));
        assert_eq!((text.as_str(), preheader.as_deref()), ("", Some("Preview")));
    }
}
