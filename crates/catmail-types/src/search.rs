use serde::{Deserialize, Serialize};

use crate::{AccountId, Flags, FolderId, MessageCursor, MessageId};

/// A full-text search over subjects, addresses and downloaded bodies.
///
/// `text` is a list of terms that must all match. A term is a word, a `"quoted
/// phrase"`, or either prefixed with `from:`, `to:` (also covers Cc) or `subject:` to
/// search only that field. Chinese, Japanese and Korean text matches as a substring;
/// other words match whole (case and diacritics are ignored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    /// Only these accounts; `None` searches every account, an empty list none.
    pub accounts: Option<Vec<AccountId>>,
    pub folder: Option<FolderId>,
    /// Which page of the matches, and only those its filter takes: paged and filtered
    /// as a list is.
    #[serde(flatten)]
    pub page: ListQuery,
    /// Messages the servers found for this text whose bodies aren't downloaded (see
    /// `MailClient::search_server`): they match too, within the account and folder.
    #[serde(default)]
    pub found_on_server: Vec<MessageId>,
}

impl SearchQuery {
    /// The first 50 matches, whatever their flags.
    pub fn new(text: impl Into<String>) -> Self {
        Self::paged(text, ListQuery::new(50))
    }

    pub fn paged(text: impl Into<String>, page: ListQuery) -> Self {
        Self {
            text: text.into(),
            accounts: None,
            folder: None,
            page,
            found_on_server: Vec::new(),
        }
    }
}

/// A page of a list of messages (a folder, the inboxes together, search results),
/// newest first: the first `limit` that `filter` takes, or the `limit` after `after`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListQuery {
    pub limit: u32,
    /// Only the messages after this one; the next page after a page ending with it.
    pub after: Option<MessageCursor>,
    #[serde(default)]
    pub filter: ListFilter,
}

impl ListQuery {
    /// The first `limit` messages, whatever their flags.
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            after: None,
            filter: ListFilter::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagFilter {
    #[default]
    All,
    Read,
    Unread,
    Flagged,
    Unflagged,
}

impl FlagFilter {
    /// The flags it looks at, and what they must be: `flags & mask == want`.
    pub const fn mask(self) -> (Flags, Flags) {
        match self {
            FlagFilter::All => (Flags::empty(), Flags::empty()),
            FlagFilter::Read => (Flags::SEEN, Flags::SEEN),
            FlagFilter::Unread => (Flags::SEEN, Flags::empty()),
            FlagFilter::Flagged => (Flags::FLAGGED, Flags::FLAGGED),
            FlagFilter::Unflagged => (Flags::FLAGGED, Flags::empty()),
        }
    }
}

/// Which messages of a list (or of search results) to list: those whose flags `flags`
/// takes, and `also` these whatever their flags, if the list has them. `also` keeps the
/// rows shown in a view that a change took out of it (read in Unread) until it is left.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListFilter {
    #[serde(default)]
    pub flags: FlagFilter,
    #[serde(default)]
    pub also: Vec<MessageId>,
}

impl ListFilter {
    pub fn new(flags: FlagFilter) -> Self {
        Self {
            flags,
            also: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTerm {
    pub field: SearchField,
    /// As typed, without the quotes and the field's prefix. Holds a letter or a digit.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    /// The subject, the addresses and the body.
    Anywhere,
    From,
    /// To and Cc.
    To,
    Subject,
}

impl SearchTerm {
    /// Splits input into terms: whitespace-separated words or `"quoted phrases"`, each
    /// optionally prefixed with `from:`, `to:` or `subject:`. A term without a letter or a
    /// digit matches nothing and is left out. Control characters separate
    /// words too, and become spaces inside a phrase: a pasted NUL would end an FTS5
    /// string, and a line break an IMAP command.
    pub fn parse(input: &str) -> Vec<Self> {
        let mut terms = Vec::new();
        let mut chars = input.chars().peekable();
        loop {
            while chars
                .next_if(|c| c.is_whitespace() || c.is_control())
                .is_some()
            {}
            if chars.peek().is_none() {
                return terms;
            }
            let mut word = String::new();
            let mut quoted = false;
            while let Some(&c) = chars.peek() {
                if c == '"' {
                    quoted = !quoted;
                } else if (c.is_whitespace() || c.is_control()) && !quoted {
                    break;
                } else if c.is_control() {
                    word.push(' ');
                } else {
                    word.push(c);
                }
                chars.next();
            }
            let prefixed = PREFIXES.iter().find_map(|(prefix, field)| {
                let head = word.get(..prefix.len())?;
                head.eq_ignore_ascii_case(prefix)
                    .then(|| (*field, word[prefix.len()..].to_owned()))
            });
            let (field, text) = prefixed.unwrap_or((SearchField::Anywhere, word));
            if text.chars().any(char::is_alphanumeric) {
                terms.push(Self { field, text });
            }
        }
    }
}

const PREFIXES: [(&str, SearchField); 3] = [
    ("from:", SearchField::From),
    ("to:", SearchField::To),
    ("subject:", SearchField::Subject),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn term(field: SearchField, text: &str) -> SearchTerm {
        SearchTerm {
            field,
            text: text.into(),
        }
    }

    #[test]
    fn a_search_keeps_its_page_beside_its_text() {
        let mut query = SearchQuery::paged("lunch", ListQuery::new(10));
        query.page.filter = ListFilter::new(FlagFilter::Unread);
        let json = serde_json::to_value(&query).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "text": "lunch", "accounts": null, "folder": null, "limit": 10, "after": null,
                "filter": {"flags": "unread", "also": []}, "found_on_server": [],
            })
        );
        assert_eq!(serde_json::from_value::<SearchQuery>(json).unwrap(), query);
        // Without a filter, as before there was one.
        let old = serde_json::json!({"text": "lunch", "accounts": null, "folder": null, "limit": 10, "after": null});
        assert_eq!(
            serde_json::from_value::<SearchQuery>(old).unwrap(),
            SearchQuery::paged("lunch", ListQuery::new(10))
        );
    }

    #[test]
    fn parses_words_phrases_and_fields() {
        use SearchField::*;
        assert_eq!(SearchTerm::parse("  "), []);
        assert_eq!(
            SearchTerm::parse(r#"From:alice "quarterly report" 季度报告 subject:"a b""#),
            [
                term(From, "alice"),
                term(Anywhere, "quarterly report"),
                term(Anywhere, "季度报告"),
                term(Subject, "a b"),
            ]
        );
        assert_eq!(
            SearchTerm::parse("hel\0lo \"a\0b\""),
            [
                term(Anywhere, "hel"),
                term(Anywhere, "lo"),
                term(Anywhere, "a b")
            ]
        );
        assert_eq!(SearchTerm::parse("*** from:() to:季"), [term(To, "季")]);
    }
}
