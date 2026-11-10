//! The tokens agents present (`Authorization: Bearer …`), one per agent, in a file of
//! the config directory rather than the database: managing them doesn't need the
//! engine, so `catmail mcp token add` works while `catmail serve` holds the data
//! directory, and the hosts (the app, `catmail serve`) share them. Only a token's
//! SHA-256 is stored; the token itself is shown once, when made.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
    #[error("can't get random bytes for a token: {0}")]
    Random(getrandom::Error),
}

/// A token as stored: what it is for, never what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Which agent it is for, as the user named it.
    pub name: String,
    /// Unix timestamp, seconds.
    pub created_at: i64,
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

    pub fn add(&self, name: &str) -> Result<NewToken, TokenError> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_NAME_CHARS
            || name.chars().any(char::is_control)
        {
            return Err(TokenError::BadName);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(TokenError::Random)?;
        let secret = format!("{PREFIX}{}", hex::encode(bytes));
        let token = Token {
            name: name.to_owned(),
            created_at: jiff::Timestamp::now().as_second(),
        };
        self.change(|file| {
            if file.tokens.iter().any(|e| e.name == token.name) {
                return Err(TokenError::NameTaken(token.name.clone()));
            }
            file.tokens.push(Entry {
                name: token.name.clone(),
                sha256: digest(&secret),
                created_at: token.created_at,
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
        let laptop = tokens.add(" laptop ").unwrap();
        assert_eq!(laptop.token.name, "laptop");
        assert!(laptop.secret.starts_with(PREFIX));
        let phone = tokens.add("phone").unwrap();
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
        let new = tokens.add("laptop").unwrap();
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
        tokens.add("laptop").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&config), 0o700);
        assert_eq!(mode(&tokens.path()), 0o600);
    }

    #[test]
    fn names_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        tokens.add("laptop").unwrap();
        assert!(matches!(
            tokens.add("laptop"),
            Err(TokenError::NameTaken(_))
        ));
        assert!(matches!(tokens.add("  "), Err(TokenError::BadName)));
        assert!(matches!(tokens.add("a\nb"), Err(TokenError::BadName)));
        assert!(matches!(
            tokens.add(&"x".repeat(65)),
            Err(TokenError::BadName)
        ));
        assert_eq!(tokens.list().unwrap().len(), 1);
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
        assert!(tokens.add("laptop").is_err());
    }
}
