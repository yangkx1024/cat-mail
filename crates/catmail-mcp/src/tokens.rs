//! The tokens agents present (`Authorization: Bearer …`), one per agent, in a file of
//! the config directory rather than the database: managing them doesn't need the
//! engine, so `catmail mcp token add` works while `catmail serve` holds the data
//! directory, and the hosts (the app, `catmail serve`) share them. Only a token's
//! SHA-256 is stored; the token itself is shown once, when made. Each says what the
//! agent may do ([`Access`]).

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use catmail_core::{create_private_dir, private_file_options};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The file's name in the config directory.
pub const FILE_NAME: &str = "mcp-tokens.json";
/// Held while the file is read and written again, so two changes at once don't lose one.
const LOCK_NAME: &str = "mcp-tokens.lock";
const VERSION: u32 = 1;
/// Tells a catmail token apart from others, for people and for secret scanners.
const PREFIX: &str = "catmail_";
const MAX_NAME_CHARS: usize = 64;
/// More than anyone has; a list that long is a mistake.
const MAX_ACCOUNTS: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{} was written by a newer catmail (version {version})", path.display())]
    Version { path: PathBuf, version: u32 },
    #[error("a token named `{0}` exists already")]
    NameTaken(String),
    #[error("a token's name is 1 to {MAX_NAME_CHARS} characters, without control characters")]
    BadName,
    #[error("`{0}` isn't an email address")]
    BadAccount(String),
    #[error("a token limited to accounts needs at least one, and at most {MAX_ACCOUNTS}")]
    BadAccounts,
    #[error("can't get random bytes for a token: {0}")]
    Random(getrandom::Error),
}

/// What a token lets an agent do. Each tier can do what those before it can; an agent
/// doesn't see the tools of a tier above its token's.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// List, search and read mail, and sync.
    #[default]
    Read,
    /// Also mark messages, move, archive, label them and move them to Trash.
    Organize,
    /// Also write drafts (new ones, replies, forwards) and change them and their
    /// attachments, for the user to send.
    Draft,
    /// Also send drafts, each once the user confirms it in the agent's client.
    Send,
    /// Also send drafts without asking the user: for an agent whose client can't ask, or
    /// that works on its own.
    SendUnasked,
}

impl Tier {
    pub const ALL: [Tier; 5] = [
        Tier::Read,
        Tier::Organize,
        Tier::Draft,
        Tier::Send,
        Tier::SendUnasked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::Organize => "organize",
            Tier::Draft => "draft",
            Tier::Send => "send",
            Tier::SendUnasked => "send_unasked",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Tier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Tier::ALL
            .into_iter()
            .find(|t| t.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| {
                let names: Vec<_> = Tier::ALL.iter().map(|t| t.as_str()).collect();
                format!("`{s}` isn't a tier; one of {}", names.join(", "))
            })
    }
}

/// What an agent with the token may do, and with which accounts. A token from before
/// tiers reads as reading only, with every account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    #[serde(default)]
    pub tier: Tier,
    /// The accounts, by email address; every account with `None`, those added later too.
    /// By address rather than id: what the user named, and the same again if the
    /// account is removed and added back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<String>>,
}

impl Access {
    /// Whether the account with this address is one of the token's.
    pub fn allows(&self, email: &str) -> bool {
        self.accounts
            .as_ref()
            .is_none_or(|list| list.iter().any(|a| a.eq_ignore_ascii_case(email)))
    }

    /// Trimmed, checked, and each address once.
    fn checked(mut self) -> Result<Self, TokenError> {
        if let Some(list) = &mut self.accounts {
            let mut seen: Vec<String> = Vec::new();
            for email in list.iter() {
                let email = email.trim();
                if !plausible_email(email) {
                    return Err(TokenError::BadAccount(email.to_owned()));
                }
                if !seen.iter().any(|s| s.eq_ignore_ascii_case(email)) {
                    seen.push(email.to_owned());
                }
            }
            if seen.is_empty() || seen.len() > MAX_ACCOUNTS {
                return Err(TokenError::BadAccounts);
            }
            *list = seen;
        }
        Ok(self)
    }
}

/// Enough to catch a name given where an address was meant; the accounts themselves
/// aren't known here (making a token doesn't open the engine).
fn plausible_email(email: &str) -> bool {
    email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    }) && !email.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// A token as stored: what it is for, never what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Which agent it is for, as the user named it.
    pub name: String,
    /// Unix timestamp, seconds.
    pub created_at: i64,
    pub access: Access,
}

/// A token just made: `secret` is what the agent sends, and is not kept.
#[derive(Debug)]
pub struct NewToken {
    pub token: Token,
    pub secret: String,
}

/// The token file of a config directory.
#[derive(Debug, Clone)]
pub struct Tokens {
    dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    tokens: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    /// Hex.
    sha256: String,
    created_at: i64,
    #[serde(flatten)]
    access: Access,
}

impl Tokens {
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: config_dir.into(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// Oldest first.
    pub fn list(&self) -> Result<Vec<Token>, TokenError> {
        Ok(self.read()?.tokens.iter().map(Entry::token).collect())
    }

    /// The token `secret` is, if it is one of the file's. Reads the file every time:
    /// it is small, and a token taken out stops working at once.
    pub fn verify(&self, secret: &str) -> Result<Option<Token>, TokenError> {
        let hash = digest(secret);
        Ok(self
            .read()?
            .tokens
            .iter()
            .find(|e| e.sha256 == hash)
            .map(Entry::token))
    }

    pub fn add(&self, name: &str, access: Access) -> Result<NewToken, TokenError> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_NAME_CHARS
            || name.chars().any(char::is_control)
        {
            return Err(TokenError::BadName);
        }
        let access = access.checked()?;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(TokenError::Random)?;
        let secret = format!("{PREFIX}{}", hex::encode(bytes));
        let token = Token {
            name: name.to_owned(),
            created_at: jiff::Timestamp::now().as_second(),
            access,
        };
        self.change(|file| {
            if file.tokens.iter().any(|e| e.name == token.name) {
                return Err(TokenError::NameTaken(token.name.clone()));
            }
            file.tokens.push(Entry {
                name: token.name.clone(),
                sha256: digest(&secret),
                created_at: token.created_at,
                access: token.access.clone(),
            });
            Ok(())
        })?;
        Ok(NewToken { token, secret })
    }

    /// Takes the token named `name` out; false if there was none.
    pub fn remove(&self, name: &str) -> Result<bool, TokenError> {
        let mut removed = false;
        self.change(|file| {
            let before = file.tokens.len();
            file.tokens.retain(|e| e.name != name.trim());
            removed = file.tokens.len() != before;
            Ok(())
        })?;
        Ok(removed)
    }

    fn read(&self) -> Result<File, TokenError> {
        let path = self.path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(File {
                    version: VERSION,
                    tokens: Vec::new(),
                });
            }
            Err(source) => return Err(TokenError::Io { path, source }),
        };
        let file: File = serde_json::from_slice(&bytes).map_err(|source| TokenError::Parse {
            path: path.clone(),
            source,
        })?;
        if file.version > VERSION {
            return Err(TokenError::Version {
                path,
                version: file.version,
            });
        }
        Ok(file)
    }

    /// Reads the file, changes it and writes it whole to a new file put in its place:
    /// a reader sees the old file or the new one, never half of one.
    fn change(
        &self,
        edit: impl FnOnce(&mut File) -> Result<(), TokenError>,
    ) -> Result<(), TokenError> {
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| TokenError::Io { path, source }
        };
        // Only this user may enter a config directory made here; one that exists is left
        // as it is. Its parent may not exist yet either (`~/.config` on a new system).
        if let Some(parent) = self.dir.parent() {
            fs::create_dir_all(parent).map_err(io(parent))?;
        }
        create_private_dir(&self.dir).map_err(io(&self.dir))?;
        let lock_path = self.dir.join(LOCK_NAME);
        let lock = private_file_options()
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(io(&lock_path))?;
        lock.lock().map_err(io(&lock_path))?;
        let mut file = self.read()?;
        edit(&mut file)?;
        file.version = VERSION;
        let path = self.path();
        let mut json = serde_json::to_vec_pretty(&file).expect("tokens serialize");
        json.push(b'\n');
        // Only this user can read it on Unix (0600); Windows inherits the directory's ACL.
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir).map_err(io(&self.dir))?;
        temp.write_all(&json).map_err(io(temp.path()))?;
        temp.as_file().sync_all().map_err(io(temp.path()))?;
        temp.persist(&path).map_err(|e| TokenError::Io {
            path: path.clone(),
            source: e.error,
        })?;
        Ok(())
    }
}

impl Entry {
    fn token(&self) -> Token {
        Token {
            name: self.name.clone(),
            created_at: self.created_at,
            access: self.access.clone(),
        }
    }
}

fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_token_verifies_until_removed() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path().join("config"));
        assert!(tokens.list().unwrap().is_empty());
        let laptop = tokens.add(" laptop ", Access::default()).unwrap();
        assert_eq!(laptop.token.name, "laptop");
        assert!(laptop.secret.starts_with(PREFIX));
        let phone = tokens.add("phone", Access::default()).unwrap();
        assert_ne!(laptop.secret, phone.secret);

        assert_eq!(
            tokens.verify(&laptop.secret).unwrap().map(|t| t.name),
            Some("laptop".into())
        );
        assert_eq!(tokens.verify("catmail_nope").unwrap(), None);
        let names: Vec<_> = tokens.list().unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["laptop", "phone"]);

        assert!(tokens.remove("laptop").unwrap());
        assert!(!tokens.remove("laptop").unwrap());
        assert_eq!(tokens.verify(&laptop.secret).unwrap(), None);
        assert!(tokens.verify(&phone.secret).unwrap().is_some());
    }

    #[test]
    fn only_hashes_are_stored() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        let new = tokens.add("laptop", Access::default()).unwrap();
        let stored = fs::read_to_string(tokens.path()).unwrap();
        assert!(!stored.contains(&new.secret));
        assert!(stored.contains(&digest(&new.secret)));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_and_a_new_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let tokens = Tokens::new(&config);
        tokens.add("laptop", Access::default()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&config), 0o700);
        assert_eq!(mode(&tokens.path()), 0o600);
    }

    #[test]
    fn names_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        tokens.add("laptop", Access::default()).unwrap();
        assert!(matches!(
            tokens.add("laptop", Access::default()),
            Err(TokenError::NameTaken(_))
        ));
        assert!(matches!(
            tokens.add("  ", Access::default()),
            Err(TokenError::BadName)
        ));
        assert!(matches!(
            tokens.add("a\nb", Access::default()),
            Err(TokenError::BadName)
        ));
        assert!(matches!(
            tokens.add(&"x".repeat(65), Access::default()),
            Err(TokenError::BadName)
        ));
        assert_eq!(tokens.list().unwrap().len(), 1);
    }

    #[test]
    fn access_is_kept_with_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        let organize = Access {
            tier: Tier::Organize,
            accounts: Some(vec![
                " a@example.com".into(),
                "A@example.com".into(),
                "b@x".into(),
            ]),
        };
        let new = tokens.add("laptop", organize).unwrap();
        let access = tokens.verify(&new.secret).unwrap().unwrap().access;
        assert_eq!(access.tier, Tier::Organize);
        assert_eq!(
            access.accounts.as_deref(),
            Some(&["a@example.com".to_owned(), "b@x".to_owned()][..])
        );
        assert!(access.allows("A@Example.com"));
        assert!(!access.allows("c@example.com"));
        assert!(Access::default().allows("c@example.com"));

        let only = |accounts: Vec<&str>| Access {
            tier: Tier::Read,
            accounts: Some(accounts.into_iter().map(Into::into).collect()),
        };
        assert!(matches!(
            tokens.add("phone", only(vec![])),
            Err(TokenError::BadAccounts)
        ));
        assert!(matches!(
            tokens.add("phone", only(vec!["alice"])),
            Err(TokenError::BadAccount(_))
        ));
    }

    #[test]
    fn a_token_from_before_tiers_reads_only() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        let secret = "catmail_old";
        fs::write(
            tokens.path(),
            format!(
                r#"{{"version": 1, "tokens": [{{"name": "old", "sha256": "{}", "created_at": 0}}]}}"#,
                digest(secret)
            ),
        )
        .unwrap();
        let token = tokens.verify(secret).unwrap().unwrap();
        assert_eq!(token.access, Access::default());
        assert_eq!(token.access.tier, Tier::Read);
    }

    #[test]
    fn tiers_parse_and_order() {
        assert_eq!("Organize".parse::<Tier>(), Ok(Tier::Organize));
        assert_eq!(" draft".parse::<Tier>(), Ok(Tier::Draft));
        assert_eq!("send_unasked".parse::<Tier>(), Ok(Tier::SendUnasked));
        assert!("everything".parse::<Tier>().is_err());
        assert!(Tier::ALL.is_sorted_by(|a, b| a < b));
        // The file says what the CLI does.
        for tier in Tier::ALL {
            assert_eq!(serde_json::to_value(tier).unwrap(), tier.as_str());
        }
    }

    #[test]
    fn a_newer_file_is_not_read_as_this_version() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        fs::write(tokens.path(), r#"{"version": 2, "tokens": []}"#).unwrap();
        assert!(matches!(
            tokens.list(),
            Err(TokenError::Version { version: 2, .. })
        ));
        // Nor written over.
        assert!(tokens.add("laptop", Access::default()).is_err());
    }
}
