use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};

/// How to log in, to IMAP and SMTP alike. Each variant maps to a SASL mechanism (or
/// IMAP LOGIN / SMTP AUTH LOGIN), so supporting a new kind of credential (OAuth2 in M6)
/// adds a variant here and leaves sync and send code alone.
///
/// Secrets are [`SecretString`]s: zeroed when dropped and never in `Debug` output.
/// Copies handed to imap-next or written to the socket are out of reach.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Credentials {
    Password {
        username: String,
        password: SecretString,
    },
}

impl Credentials {
    /// The SASL PLAIN response (RFC 4616): no authorization identity, then the
    /// username and password, separated by NUL.
    pub(crate) fn sasl_plain(username: &str, password: &SecretString) -> Zeroizing<Vec<u8>> {
        let password = password.expose_secret();
        let mut out = Zeroizing::new(Vec::with_capacity(username.len() + password.len() + 2));
        out.push(0);
        out.extend_from_slice(username.as_bytes());
        out.push(0);
        out.extend_from_slice(password.as_bytes());
        out
    }
}
