//! The login forms without their widgets: the sign-in page (an account) and the form for
//! another sending address (an identity) take the same fields, read the same way, and
//! submit the same way.

use catmail_core::{NewAccount, NewIdentity, SecretString, Security, ServerConfig};

/// Security choices, in the order the forms show them.
pub const SECURITY: [(Security, &str); 3] = [
    (Security::Tls, "TLS"),
    (Security::StartTls, "STARTTLS"),
    (Security::Plain, "None"),
];

/// Under the Security choice, when it is `Plain`.
pub const PLAIN_WARNING: &str = "Unencrypted. Only for local test servers.";

/// Under the Name field.
pub const NAME_HELP: &str = "Shown to the people you write to.";

/// Who signs in, as typed.
#[derive(Debug, Clone)]
pub struct LoginValues {
    pub email: String,
    /// Redacted in `Debug` output.
    pub password: SecretString,
    pub name: String,
    pub username: String,
}

impl LoginValues {
    /// The address, the display name if any, and the username: an empty one is the
    /// address, as in the CLI.
    pub fn who(&self) -> (String, Option<String>, String) {
        let email = self.email.trim();
        let username = match self.username.trim() {
            "" => email,
            username => username,
        };
        let name = self.name.trim();
        (
            email.to_owned(),
            (!name.is_empty()).then(|| name.to_owned()),
            username.to_owned(),
        )
    }
}

/// A server's part of a form, as typed.
#[derive(Debug, Clone)]
pub struct ServerValues {
    pub host: String,
    pub port: String,
    pub security: Security,
}

impl ServerValues {
    /// An empty port means the usual one for the security mode, as in the CLI. The rest
    /// (an empty host, say) is the engine's to reject.
    pub fn config(&self, default_port: fn(Security) -> u16) -> Result<ServerConfig, &'static str> {
        let port = match self.port.trim() {
            "" => default_port(self.security),
            port => port
                .parse::<u16>()
                .ok()
                .filter(|&p| p != 0)
                .ok_or("A port is a number from 1 to 65535.")?,
        };
        Ok(ServerConfig {
            host: self.host.trim().to_owned(),
            port,
            security: self.security,
        })
    }
}

/// The sign-in page, as typed.
#[derive(Debug, Clone)]
pub struct AccountValues {
    pub login: LoginValues,
    pub imap: ServerValues,
    pub smtp: ServerValues,
}

impl AccountValues {
    /// The account to add.
    pub fn new_account(&self) -> Result<NewAccount, &'static str> {
        let (email, display_name, username) = self.login.who();
        Ok(NewAccount {
            email,
            display_name,
            username,
            imap: self.imap.config(Security::default_imap_port)?,
            smtp: self.smtp.config(Security::default_smtp_port)?,
        })
    }
}

/// The form for another sending address, as typed.
#[derive(Debug, Clone)]
pub struct IdentityValues {
    pub login: LoginValues,
    pub smtp: ServerValues,
}

impl IdentityValues {
    pub fn new_identity(&self) -> Result<NewIdentity, &'static str> {
        let (email, display_name, username) = self.login.who();
        Ok(NewIdentity {
            email,
            display_name,
            username,
            smtp: self.smtp.config(Security::default_smtp_port)?,
        })
    }
}

/// A form's submission: whether one is running, and why the last one failed. The form
/// runs the request; this says whether to, and what to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    busy: bool,
    error: Option<String>,
    /// What submitting does, for the error: "Couldn't sign in".
    what: &'static str,
}

impl Submission {
    /// `what` submitting does, to say what failed: "sign in".
    pub fn new(what: &'static str) -> Self {
        Self {
            busy: false,
            error: None,
            what,
        }
    }

    /// The request to run, unless one is already running. A problem with the values
    /// (`Err`) shows as the error instead.
    pub fn begin<R>(&mut self, request: Result<R, &'static str>) -> Option<R> {
        if self.busy {
            return None;
        }
        match request {
            Ok(request) => {
                self.busy = true;
                self.error = None;
                Some(request)
            }
            Err(problem) => {
                self.error = Some(problem.to_owned());
                None
            }
        }
    }

    /// The request ended: its value, or `None` with its failure shown.
    pub fn ended<T>(&mut self, result: catmail_core::Result<T>) -> Option<T> {
        self.busy = false;
        result
            .inspect_err(|e| self.error = Some(format!("Couldn't {}: {e}", self.what)))
            .ok()
    }

    /// Whether a submission is running. Leaving the form then would only skip what
    /// follows: the engine finishes the request whatever the window does (its task
    /// isn't cancelled with the form's), so Cancel is off meanwhile.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Why the last submission failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, security: Security) -> ServerValues {
        ServerValues {
            host: host.into(),
            port: String::new(),
            security,
        }
    }

    fn login() -> LoginValues {
        LoginValues {
            email: " me@example.com ".into(),
            password: "pw".into(),
            name: String::new(),
            username: String::new(),
        }
    }

    fn filled() -> AccountValues {
        AccountValues {
            login: login(),
            imap: server("imap.example.com", Security::Tls),
            smtp: server("smtp.example.com", Security::StartTls),
        }
    }

    #[test]
    fn debug_output_leaves_out_the_password() {
        let values = AccountValues {
            login: LoginValues {
                password: "hunter2".into(),
                ..login()
            },
            ..filled()
        };
        let shown = format!("{values:?}");
        assert!(shown.contains("me@example.com"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn empty_fields_take_the_cli_defaults() {
        let new = filled().new_account().unwrap();
        assert_eq!(new.email, "me@example.com");
        assert_eq!(new.username, "me@example.com");
        assert_eq!(new.display_name, None);
        assert_eq!((new.imap.port, new.imap.security), (993, Security::Tls));
        assert_eq!(
            (new.smtp.port, new.smtp.security),
            (587, Security::StartTls)
        );
    }

    #[test]
    fn typed_values_win() {
        let new = AccountValues {
            login: LoginValues {
                name: " 张三 ".into(),
                username: "zhang".into(),
                ..login()
            },
            imap: ServerValues {
                port: "10143".into(),
                ..server("127.0.0.1", Security::Plain)
            },
            ..filled()
        }
        .new_account()
        .unwrap();
        assert_eq!(new.display_name.as_deref(), Some("张三"));
        assert_eq!(new.username, "zhang");
        assert_eq!((new.imap.port, new.imap.security), (10143, Security::Plain));
    }

    #[test]
    fn rejects_ports_that_are_not_ports() {
        for port in ["0", "65536", "x"] {
            let values = AccountValues {
                imap: ServerValues {
                    port: port.into(),
                    ..server("imap.example.com", Security::Tls)
                },
                ..filled()
            };
            assert_eq!(
                values.new_account().unwrap_err(),
                "A port is a number from 1 to 65535."
            );
        }
    }

    #[test]
    fn an_identitys_empty_fields_take_the_defaults() {
        let values = IdentityValues {
            login: LoginValues {
                email: " news@example.org ".into(),
                password: "key".into(),
                name: String::new(),
                username: String::new(),
            },
            smtp: server("smtp.resend.com", Security::Tls),
        };
        let new = values.new_identity().unwrap();
        assert_eq!(new.email, "news@example.org");
        assert_eq!(new.username, "news@example.org");
        assert_eq!(new.display_name, None);
        assert_eq!((new.smtp.port, new.smtp.security), (465, Security::Tls));
    }

    #[test]
    fn one_submission_at_a_time() {
        let mut submit = Submission::new("sign in");
        assert_eq!(submit.begin(Err("A port is a number")), None::<()>);
        assert_eq!(submit.error(), Some("A port is a number"));
        assert_eq!(submit.begin(Ok(1)), Some(1));
        assert!(submit.busy());
        assert_eq!(submit.error(), None, "a new try clears the old error");
        assert_eq!(submit.begin(Ok(2)), None, "one is running");
        let failed: catmail_core::Result<()> = Err(catmail_core::Error::AuthFailed("no".into()));
        assert_eq!(submit.ended(failed), None);
        assert!(!submit.busy());
        assert!(
            submit
                .error()
                .is_some_and(|e| e.starts_with("Couldn't sign in: "))
        );
        assert_eq!(submit.ended(Ok(3)), Some(3));
    }
}
