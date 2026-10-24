use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};

/// How to log in, to IMAP and SMTP alike: a username and password, sent with SASL PLAIN
/// where the server offers it, else IMAP LOGIN / SMTP AUTH LOGIN.
///
/// The password is a [`SecretString`]: zeroed when dropped and never in `Debug` output.
/// Copies written to the socket are out of reach.
#[derive(Clone, Debug)]
pub struct Credentials {
    pub username: String,
    pub password: SecretString,
}

impl Credentials {
    /// The SASL PLAIN response (RFC 4616): no authorization identity, then the
    /// username and password, separated by NUL.
    pub(crate) fn sasl_plain(&self) -> Zeroizing<Vec<u8>> {
        let (username, password) = (&self.username, self.password.expose_secret());
        let mut out = Zeroizing::new(Vec::with_capacity(username.len() + password.len() + 2));
        out.push(0);
        out.extend_from_slice(username.as_bytes());
        out.push(0);
        out.extend_from_slice(password.as_bytes());
        out
    }
}
