//! UID SEARCH: the keys the fake understands and which messages they match.

use super::wire::Tok;
use super::{FakeMessage, filter_headers, header_end, set_contains};

/// A UID SEARCH key the fake understands: `ALL`, `UID <set>`, `HEADER <field> <text>`,
/// `TEXT` / `BODY <text>`, `FROM` / `TO` / `CC` / `SUBJECT <text>`, `OR <key> <key>`,
/// and on Gmail `X-GM-MSGID <id>` and `X-GM-RAW <query>` (see [`gm_raw_matches`]). Text
/// matches as a substring, whatever its case, as Dovecot without full-text search matches
/// it; MIME encodings aren't decoded.
pub(super) enum SearchKey {
    All,
    Uid(String),
    /// A header field, lowercase, and the text.
    Header(String, String),
    /// Anywhere in the message.
    Text(String),
    /// After the header.
    Body(String),
    GmMsgid(u64),
    GmRaw(String),
    Or(Box<SearchKey>, Box<SearchKey>),
}

/// The key at `args[*i]`, moving `i` past it; `None` if it isn't one.
pub(super) fn search_key(args: &[Tok], i: &mut usize, gmail: bool) -> Option<SearchKey> {
    let mut next = || {
        let tok = args.get(*i).map(Tok::text);
        *i += 1;
        tok
    };
    let name = next()?.to_ascii_uppercase();
    Some(match name.as_str() {
        "ALL" => SearchKey::All,
        "UID" => SearchKey::Uid(next()?),
        "HEADER" => SearchKey::Header(next()?.to_ascii_lowercase(), next()?),
        "TEXT" => SearchKey::Text(next()?),
        "BODY" => SearchKey::Body(next()?),
        "FROM" | "TO" | "CC" | "SUBJECT" => SearchKey::Header(name.to_ascii_lowercase(), next()?),
        "X-GM-MSGID" if gmail => SearchKey::GmMsgid(next()?.parse().ok()?),
        "X-GM-RAW" if gmail => SearchKey::GmRaw(next()?),
        "OR" => SearchKey::Or(
            Box::new(search_key(args, i, gmail)?),
            Box::new(search_key(args, i, gmail)?),
        ),
        _ => return None,
    })
}

impl SearchKey {
    /// `max_uid` is the mailbox's highest UID, which `*` stands for.
    pub(super) fn matches(&self, m: &FakeMessage, max_uid: u32) -> bool {
        match self {
            SearchKey::All => true,
            SearchKey::Uid(set) => set_contains(set, m.uid, || max_uid),
            SearchKey::Header(field, text) => header_has(&m.raw, field, text),
            SearchKey::Text(text) => contains(&String::from_utf8_lossy(&m.raw), text),
            SearchKey::Body(text) => {
                contains(&String::from_utf8_lossy(&m.raw[header_end(&m.raw)..]), text)
            }
            SearchKey::GmMsgid(id) => m.gm_msgid == *id,
            SearchKey::GmRaw(query) => gm_raw_matches(query, &m.raw),
            SearchKey::Or(a, b) => a.matches(m, max_uid) || b.matches(m, max_uid),
        }
    }

    /// Whether this key or one in it is `what`.
    pub(super) fn has(&self, what: impl Fn(&SearchKey) -> bool + Copy) -> bool {
        what(self)
            || match self {
                SearchKey::Or(a, b) => a.has(what) || b.has(what),
                _ => false,
            }
    }
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Whether a value of the header `field` (lowercase) holds `text`.
fn header_has(raw: &[u8], field: &str, text: &str) -> bool {
    let found = filter_headers(raw, &[field.to_owned()]);
    String::from_utf8_lossy(&found)
        .lines()
        .filter_map(|line| line.split_once(':').map(|(_, value)| value))
        .any(|value| contains(value, text))
}

/// A Gmail search as far as catmail writes one: terms ANDed, each a word or a `"phrase"`,
/// optionally prefixed with `from:`, `to:`, `cc:` or `subject:`; `{…}` ORs the terms in it.
fn gm_raw_matches(query: &str, raw: &[u8]) -> bool {
    enum Item {
        Term(Option<String>, String),
        AnyOf(Vec<Item>),
    }
    fn items(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Vec<Item> {
        let mut out = Vec::new();
        while let Some(&c) = chars.peek() {
            match c {
                ' ' => {
                    chars.next();
                }
                '{' => {
                    chars.next();
                    out.push(Item::AnyOf(items(chars)));
                }
                '}' => {
                    chars.next();
                    return out;
                }
                _ => {
                    let mut field = None;
                    let mut text = String::new();
                    let mut quoted = false;
                    while let Some(&c) = chars.peek() {
                        match c {
                            '"' => quoted = !quoted,
                            ' ' | '{' | '}' if !quoted => break,
                            ':' if !quoted && field.is_none() => {
                                field = Some(std::mem::take(&mut text).to_ascii_lowercase());
                            }
                            c => text.push(c),
                        }
                        chars.next();
                    }
                    out.push(Item::Term(field, text));
                }
            }
        }
        out
    }
    fn matches(item: &Item, raw: &[u8]) -> bool {
        match item {
            Item::Term(Some(field), text) => header_has(raw, field, text),
            Item::Term(None, text) => contains(&String::from_utf8_lossy(raw), text),
            Item::AnyOf(items) => items.iter().any(|i| matches(i, raw)),
        }
    }
    items(&mut query.chars().peekable())
        .iter()
        .all(|item| matches(item, raw))
}
