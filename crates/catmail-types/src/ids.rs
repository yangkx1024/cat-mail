use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! row_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

row_id!(
    /// Local identifier of an account.
    AccountId
);
row_id!(
    /// Local identifier of a folder (IMAP mailbox).
    FolderId
);
row_id!(
    /// Local identifier of a message in one folder, i.e. of one IMAP (mailbox, UID)
    /// pair. A copy in another folder has its own id; a move yields a new id.
    MessageId
);

/// Lowercase hex SHA-256 of a blob's contents.
///
/// Always valid by construction, including when deserialized: the value becomes a file
/// path inside the blob store, so an unchecked string could escape it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlobHash(String);

impl BlobHash {
    /// Accepts exactly 64 lowercase hex characters.
    pub fn parse(s: &str) -> Option<Self> {
        let valid = s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid.then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BlobHash {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s).ok_or_else(|| format!("invalid blob hash `{s}`"))
    }
}

impl From<BlobHash> for String {
    fn from(hash: BlobHash) -> Self {
        hash.0
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_hash_parse() {
        let ok = "a".repeat(64);
        assert_eq!(BlobHash::parse(&ok).unwrap().as_str(), ok);
        assert!(BlobHash::parse(&"A".repeat(64)).is_none());
        assert!(BlobHash::parse(&"a".repeat(63)).is_none());
        assert!(BlobHash::parse("../etc/passwd").is_none());
    }

    #[test]
    fn blob_hash_serde_validates() {
        let ok = format!("\"{}\"", "a".repeat(64));
        let hash: BlobHash = serde_json::from_str(&ok).unwrap();
        assert_eq!(serde_json::to_string(&hash).unwrap(), ok);

        for bad in [r#""""#, r#""aa../../victim""#] {
            assert!(serde_json::from_str::<BlobHash>(bad).is_err(), "{bad}");
        }
    }
}
