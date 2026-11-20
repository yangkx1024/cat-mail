//! The commands as they come over the wire: a line with its literals inlined
//! ([`read_command`]), split into tokens ([`tokenize`]), and strings and literals written
//! back ([`quote`], [`literal`]).

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::tls::Stream;

pub(super) struct Read {
    /// The command line without CRLF, its literals inlined as quoted strings.
    pub line: Vec<u8>,
    /// The literals, as they came.
    pub literals: Vec<Vec<u8>>,
    /// How many of them the server asked for with `+`.
    pub synchronized: usize,
}

/// Reads one command line, inlining `{n}` / `{n+}` literals, which are also returned as
/// they came (APPEND needs the exact bytes). `None` at EOF. With `refuse_append`, an
/// APPEND is answered with NO instead of being asked for its message, and skipped. With
/// `qq_mail`, a literal in a UID SEARCH is inlined as its `{n}`. A non-synchronizing
/// literal (`{n+}`) is taken up to `non_sync_up_to` bytes (none without LITERAL+ or
/// LITERAL-); a longer one is refused with BAD and the connection closed (RFC 7888), so a
/// client sending one where it may not fails instead of passing unnoticed.
pub(super) async fn read_command(
    io: &mut BufReader<Box<dyn Stream>>,
    refuse_append: bool,
    qq_mail: bool,
    non_sync_up_to: Option<usize>,
) -> std::io::Result<Option<Read>> {
    let mut line = Vec::new();
    let mut literals = Vec::new();
    let mut synchronized = 0;
    loop {
        let mut chunk = Vec::new();
        if io.read_until(b'\n', &mut chunk).await? == 0 {
            return Ok(None);
        }
        while matches!(chunk.last(), Some(b'\n' | b'\r')) {
            chunk.pop();
        }
        let literal = parse_literal_suffix(&chunk);
        match literal {
            Some((len, non_sync, prefix_len)) => {
                line.extend_from_slice(&chunk[..prefix_len]);
                let text = String::from_utf8_lossy(&line).to_ascii_uppercase();
                if refuse_append && !non_sync && text.split(' ').nth(1) == Some("APPEND") {
                    let tag = text.split(' ').next().unwrap_or_default();
                    io.get_mut()
                        .write_all(format!("{tag} NO [SERVERBUG] Internal error\r\n").as_bytes())
                        .await?;
                    // Answered; read the next command instead.
                    line.clear();
                    literals.clear();
                    continue;
                }
                if non_sync && non_sync_up_to.is_none_or(|max| len > max) {
                    let tag = text.split(' ').next().unwrap_or_default();
                    io.get_mut()
                        .write_all(
                            format!("{tag} BAD [TOOBIG] Non-synchronizing literal not allowed\r\n")
                                .as_bytes(),
                        )
                        .await?;
                    return Ok(None);
                }
                if !non_sync {
                    io.get_mut()
                        .write_all(b"+ Ready for literal data\r\n")
                        .await?;
                    synchronized += 1;
                }
                let mut data = vec![0; len];
                io.read_exact(&mut data).await?;
                let searching = text.split(' ').skip(1).take(2).eq(["UID", "SEARCH"]);
                let inlined = match qq_mail && searching {
                    true => format!("{{{len}}}").into_bytes(),
                    false => data.clone(),
                };
                // Inline as a quoted string so the tokenizer sees one token.
                line.push(b'"');
                for &b in &inlined {
                    if b == b'"' || b == b'\\' {
                        line.push(b'\\');
                    }
                    line.push(b);
                }
                line.push(b'"');
                literals.push(data);
            }
            None => {
                line.extend_from_slice(&chunk);
                return Ok(Some(Read {
                    line,
                    literals,
                    synchronized,
                }));
            }
        }
    }
}

/// `… {12}` → (12, false, offset of `{`).
fn parse_literal_suffix(chunk: &[u8]) -> Option<(usize, bool, usize)> {
    if chunk.last() != Some(&b'}') {
        return None;
    }
    let open = chunk.iter().rposition(|&b| b == b'{')?;
    let inner = std::str::from_utf8(&chunk[open + 1..chunk.len() - 1]).ok()?;
    let (digits, non_sync) = match inner.strip_suffix('+') {
        Some(d) => (d, true),
        None => (inner, false),
    };
    Some((digits.parse().ok()?, non_sync, open))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tok {
    Atom(String),
    Str(String),
    List(Vec<Tok>),
}

impl Tok {
    pub(super) fn text(&self) -> String {
        match self {
            Tok::Atom(s) | Tok::Str(s) => s.clone(),
            Tok::List(items) => items.iter().map(Tok::text).collect::<Vec<_>>().join(" "),
        }
    }
}

pub(super) fn tokenize(input: &str) -> Vec<Tok> {
    fn parse(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        until: Option<char>,
    ) -> Vec<Tok> {
        let mut out = Vec::new();
        while let Some(&c) = chars.peek() {
            match c {
                ' ' => {
                    chars.next();
                }
                ')' if until == Some(')') => {
                    chars.next();
                    return out;
                }
                '(' => {
                    chars.next();
                    out.push(Tok::List(parse(chars, Some(')'))));
                }
                '"' => {
                    chars.next();
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => s.extend(chars.next()),
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    out.push(Tok::Str(s));
                }
                _ => {
                    // Brackets may contain spaces: BODY.PEEK[HEADER.FIELDS (DATE FROM)].
                    let mut s = String::new();
                    let mut depth = 0;
                    while let Some(&c) = chars.peek() {
                        if depth == 0 && matches!(c, ' ' | '(' | ')') {
                            break;
                        }
                        match c {
                            '[' => depth += 1,
                            ']' => depth -= 1,
                            _ => {}
                        }
                        s.push(c);
                        chars.next();
                    }
                    if s.is_empty() {
                        chars.next(); // A stray `)`.
                    } else {
                        out.push(Tok::Atom(s));
                    }
                }
            }
        }
        out
    }
    parse(&mut input.chars().peekable(), None)
}

pub(crate) fn literal(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{{{}}}\r\n", data.len()).into_bytes();
    out.extend(data);
    out
}

pub(crate) fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_fetch_arguments() {
        let toks = tokenize(
            "UID FETCH 1:3,5 (UID FLAGS BODY.PEEK[HEADER.FIELDS (DATE FROM)]) (CHANGEDSINCE 7)",
        );
        assert_eq!(toks[2], Tok::Atom("1:3,5".into()));
        let Tok::List(items) = &toks[3] else {
            panic!("{toks:?}")
        };
        assert_eq!(
            items[2],
            Tok::Atom("BODY.PEEK[HEADER.FIELDS (DATE FROM)]".into())
        );
        assert_eq!(
            toks[4],
            Tok::List(vec![
                Tok::Atom("CHANGEDSINCE".into()),
                Tok::Atom("7".into())
            ])
        );
        assert_eq!(
            tokenize(r#"LOGIN "a\"b" pw"#),
            vec![
                Tok::Atom("LOGIN".into()),
                Tok::Str("a\"b".into()),
                Tok::Atom("pw".into())
            ]
        );
    }
}
