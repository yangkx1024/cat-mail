//! Commands as written to the server. Each argument goes out as an atom, a quoted string
//! or a literal, whichever its bytes allow; literals as the server's capabilities allow.

use std::borrow::Cow;

use secrecy::zeroize::Zeroizing;

use super::{Error, Result};

/// How the server takes literals in commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Literals {
    /// `{n}`, then wait for the server's `+` before sending the bytes.
    #[default]
    Synchronizing,
    /// `{n+}` and the bytes straight after (LITERAL+, RFC 7888).
    NonSynchronizing,
    /// `{n+}` for up to 4096 bytes, else synchronizing (LITERAL-, RFC 7888).
    NonSynchronizingUpTo4k,
}

impl Literals {
    fn wait_for(self, len: usize) -> bool {
        match self {
            Self::Synchronizing => true,
            Self::NonSynchronizing => false,
            Self::NonSynchronizingUpTo4k => len > 4096,
        }
    }
}

/// A command without its tag. Its bytes are zeroed when dropped: LOGIN carries a
/// password.
pub(crate) struct Command {
    /// For errors and logs, which never show the arguments.
    pub name: &'static str,
    parts: Vec<Part>,
}

enum Part {
    Text(Zeroizing<Vec<u8>>),
    Literal(Zeroizing<Vec<u8>>),
}

impl Command {
    /// `name` is also what goes on the wire (`UID FETCH`).
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            parts: vec![Part::Text(Zeroizing::new(name.as_bytes().to_vec()))],
        }
    }

    /// Appends `text` as it is, after a space: sequence sets, item lists, flags. The
    /// caller makes sure it is valid there.
    pub fn raw(mut self, text: &str) -> Self {
        self.separate();
        self.text().extend_from_slice(text.as_bytes());
        self
    }

    /// Appends a parenthesized list of `items`, each added by `item` (with
    /// [`raw`](Self::raw), [`astring`](Self::astring) …).
    pub fn list<T>(
        self,
        items: impl IntoIterator<Item = T>,
        mut item: impl FnMut(Self, T) -> Result<Self>,
    ) -> Result<Self> {
        let mut command = self.raw("(");
        for i in items {
            command = item(command, i)?;
        }
        command.text().push(b')');
        Ok(command)
    }

    /// The space before an argument, unless it opens a list.
    fn separate(&mut self) {
        let out = self.text();
        if out.last() != Some(&b'(') {
            out.push(b' ');
        }
    }

    /// Appends an astring: an atom if it can be one, else a string.
    pub fn astring(self, value: &[u8]) -> Result<Self> {
        if !value.is_empty() && value.iter().all(|&b| is_atom_char(b)) {
            let text = std::str::from_utf8(value).expect("atom chars are ASCII");
            return Ok(self.raw(text));
        }
        self.string(value)
    }

    /// Appends a string: quoted if it can be, else a literal.
    pub fn string(mut self, value: &[u8]) -> Result<Self> {
        if value.iter().all(|&b| is_text_char(b)) {
            self.separate();
            let out = self.text();
            out.push(b'"');
            for &b in value {
                if b == b'"' || b == b'\\' {
                    out.push(b'\\');
                }
                out.push(b);
            }
            out.push(b'"');
            return Ok(self);
        }
        self.literal(value)
    }

    /// Appends a literal. A NUL can't be sent in one (that takes BINARY's `~{n}`).
    pub fn literal(mut self, value: &[u8]) -> Result<Self> {
        if value.contains(&0) {
            return Err(Error::InvalidInput(format!(
                "{} can't carry a NUL byte",
                self.name
            )));
        }
        self.separate();
        self.parts
            .push(Part::Literal(Zeroizing::new(value.to_vec())));
        self.parts.push(Part::Text(Zeroizing::new(Vec::new())));
        Ok(self)
    }

    fn text(&mut self) -> &mut Vec<u8> {
        match self.parts.last_mut() {
            Some(Part::Text(text)) => text,
            _ => unreachable!("a command always ends in text"),
        }
    }

    /// The chunks to write, grouped where the server must answer `+` before the rest.
    /// They borrow the command's bytes: a message to APPEND is not copied again.
    pub fn encode(&self, tag: &str, literals: Literals) -> Vec<Vec<Cow<'_, [u8]>>> {
        let mut segments = Vec::new();
        let mut out = vec![Cow::Owned(format!("{tag} ").into_bytes())];
        for part in &self.parts {
            match part {
                Part::Text(text) => out.push(Cow::Borrowed(&text[..])),
                Part::Literal(data) => {
                    let wait = literals.wait_for(data.len());
                    let plus = if wait { "" } else { "+" };
                    let announce = format!("{{{}{plus}}}\r\n", data.len());
                    out.push(Cow::Owned(announce.into_bytes()));
                    if wait {
                        segments.push(std::mem::take(&mut out));
                    }
                    out.push(Cow::Borrowed(&data[..]));
                }
            }
        }
        out.push(Cow::Borrowed(b"\r\n"));
        segments.push(out);
        segments
    }
}

/// ATOM-CHAR (RFC 3501): CHAR but atom-specials. Also leaves out `]`, which ASTRING-CHAR
/// allows but some servers' parsers don't.
fn is_atom_char(b: u8) -> bool {
    matches!(b, 0x21..=0x7e) && !b"(){%*\"\\]".contains(&b)
}

/// TEXT-CHAR (RFC 3501): 7-bit, no CR, LF or NUL.
fn is_text_char(b: u8) -> bool {
    matches!(b, 0x01..=0x7f) && b != b'\r' && b != b'\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(command: Command, literals: Literals) -> Vec<String> {
        command
            .encode("C1", literals)
            .iter()
            .map(|s| String::from_utf8_lossy(&s.concat()).into_owned())
            .collect()
    }

    #[test]
    fn arguments_take_the_simplest_form_their_bytes_allow() {
        let command = Command::new("SELECT")
            .astring(b"INBOX")
            .unwrap()
            .astring(b"[Gmail]/All Mail")
            .unwrap()
            .astring(b"a\"b\\c")
            .unwrap()
            .astring(b"")
            .unwrap()
            .astring(b"%")
            .unwrap();
        assert_eq!(
            wire(command, Literals::Synchronizing),
            [r#"C1 SELECT INBOX "[Gmail]/All Mail" "a\"b\\c" "" "%""#.to_owned() + "\r\n"]
        );
    }

    #[test]
    fn lists_hold_any_argument() {
        let command = Command::new("UID STORE")
            .raw("1:3")
            .raw("+X-GM-LABELS")
            .list(["\\Inbox", "Work", "Two words", "已发送"], |c, label| {
                if label.starts_with('\\') {
                    Ok(c.raw(label))
                } else {
                    c.astring(label.as_bytes())
                }
            })
            .unwrap();
        assert_eq!(
            wire(command, Literals::NonSynchronizing),
            ["C1 UID STORE 1:3 +X-GM-LABELS (\\Inbox Work \"Two words\" {9+}\r\n已发送)\r\n"]
        );
        let empty = Command::new("X").list([0; 0], |c, _| Ok(c)).unwrap();
        assert_eq!(wire(empty, Literals::Synchronizing), ["C1 X ()\r\n"]);
    }

    #[test]
    fn eight_bit_and_line_breaks_go_as_literals() {
        let command = Command::new("SELECT").astring("已发送".as_bytes()).unwrap();
        assert_eq!(
            wire(command, Literals::Synchronizing),
            ["C1 SELECT {9}\r\n", "已发送\r\n"]
        );
        let command = Command::new("LOGIN")
            .astring(b"me")
            .unwrap()
            .astring(b"pa\r\nss")
            .unwrap();
        assert_eq!(
            wire(command, Literals::NonSynchronizing),
            ["C1 LOGIN me {6+}\r\npa\r\nss\r\n"]
        );
    }

    #[test]
    fn literal_minus_waits_only_for_large_literals() {
        let command = || {
            Command::new("APPEND")
                .astring(b"INBOX")
                .unwrap()
                .literal(&[b'x'; 5000])
                .unwrap()
                .literal(b"small")
                .unwrap()
        };
        let segments = wire(command(), Literals::NonSynchronizingUpTo4k);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], "C1 APPEND INBOX {5000}\r\n");
        assert!(
            segments[1].ends_with(" {5+}\r\nsmall\r\n"),
            "{}",
            segments[1]
        );
        assert_eq!(wire(command(), Literals::NonSynchronizing).len(), 1);
        assert_eq!(wire(command(), Literals::Synchronizing).len(), 3);
    }

    #[test]
    fn a_nul_is_refused_before_anything_is_sent() {
        let err = Command::new("APPEND").literal(b"a\0b").err().unwrap();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        assert!(!err.is_fatal());
    }
}
