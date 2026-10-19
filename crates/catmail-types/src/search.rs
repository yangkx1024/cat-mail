use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId};

/// A full-text search over subjects, addresses and downloaded bodies.
///
/// `text` is a list of terms that must all match. A term is a word, a `"quoted
/// phrase"`, or either prefixed with `from:`, `to:` (also covers Cc) or `subject:` to
/// search only that field. Chinese, Japanese and Korean text matches as a substring;
/// other words match whole (case and diacritics are ignored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    /// Only this account; `None` searches every account.
    pub account: Option<AccountId>,
    /// Only this folder.
    pub folder: Option<FolderId>,
    pub limit: u32,
    pub offset: u32,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            account: None,
            folder: None,
            limit: 50,
            offset: 0,
        }
    }
}
