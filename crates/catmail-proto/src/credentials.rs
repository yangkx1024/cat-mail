use std::fmt;

/// How to log in, to IMAP and SMTP alike. Each variant maps to a SASL mechanism (or
/// IMAP LOGIN / SMTP AUTH LOGIN), so supporting a new kind of credential (OAuth2 in M6)
/// adds a variant here and leaves sync and send code alone.
#[derive(Clone)]
#[non_exhaustive]
pub enum Credentials {
    Password { username: String, password: String },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("Password")
                .field("username", username)
                .finish_non_exhaustive(),
        }
    }
}

impl Credentials {
    /// The SASL PLAIN response (RFC 4616): no authorization identity, then the
    /// username and password, separated by NUL.
    pub(crate) fn sasl_plain(username: &str, password: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(username.len() + password.len() + 2);
        out.push(0);
        out.extend_from_slice(username.as_bytes());
        out.push(0);
        out.extend_from_slice(password.as_bytes());
        out
    }
}
