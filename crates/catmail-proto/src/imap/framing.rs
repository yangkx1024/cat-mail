//! Splits what the server sends into whole responses: a line, and any literals it
//! announces with the lines that follow them. imap-proto parses only whole responses.

use std::collections::VecDeque;

/// The largest response taken. One whole message is the largest we ask for; past this the
/// server is broken or hostile.
pub(crate) const MAX_RESPONSE: usize = 100 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct TooLarge;

#[derive(Debug)]
pub(crate) struct Framer {
    /// The response being read.
    current: Vec<u8>,
    /// How far the current line is into announcing a literal.
    announcing: Announcing,
    /// Bytes of the current literal still to come.
    literal: usize,
    ready: VecDeque<Vec<u8>>,
    max: usize,
}

impl Default for Framer {
    fn default() -> Self {
        Self::with_max(MAX_RESPONSE)
    }
}

impl Framer {
    pub fn with_max(max: usize) -> Self {
        Self {
            current: Vec::new(),
            announcing: Announcing::Other,
            literal: 0,
            ready: VecDeque::new(),
            max,
        }
    }

    /// Takes bytes from the server. Literals pass through untouched: they are message
    /// content, and their announced length must stay right. A line ending in a bare LF
    /// gets its CR, which servers occasionally leave out and the parser requires.
    pub fn push(&mut self, mut input: &[u8]) -> Result<(), TooLarge> {
        while !input.is_empty() {
            if self.literal > 0 {
                let n = self.literal.min(input.len());
                self.current.extend_from_slice(&input[..n]);
                self.literal -= n;
                input = &input[n..];
            } else {
                let mut line_end = None;
                for (ix, &byte) in input.iter().enumerate() {
                    match self.announcing.next(byte) {
                        Ok(next) => self.announcing = next,
                        Err(literal) => {
                            self.announcing = Announcing::Other;
                            line_end = Some((ix, literal));
                            break;
                        }
                    }
                }
                let Some((end, literal)) = line_end else {
                    self.current.extend_from_slice(input);
                    input = &[];
                    self.check_size()?;
                    continue;
                };
                self.current.extend_from_slice(&input[..end]);
                if self.current.last() != Some(&b'\r') {
                    self.current.push(b'\r');
                }
                self.current.push(b'\n');
                input = &input[end + 1..];
                match literal {
                    Some(n) => {
                        self.literal = n as usize;
                        self.check_size()?;
                        // Grown once, not doubled again and again as a large one arrives.
                        self.current.reserve(self.literal);
                    }
                    None => self.ready.push_back(std::mem::take(&mut self.current)),
                }
            }
            self.check_size()?;
        }
        Ok(())
    }

    /// The next whole response, if one has arrived.
    pub fn next(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }

    fn check_size(&self) -> Result<(), TooLarge> {
        // The literal still to come counts: no point reading 1 GiB announced to drop it.
        if self.current.len().saturating_add(self.literal) > self.max {
            return Err(TooLarge);
        }
        Ok(())
    }
}

/// How far a line is into announcing a literal (`{n}` or `{n+}`, then CRLF or LF), byte by
/// byte: the announcement can be any length (`{0000…5}`), and arrive split across reads.
#[derive(Debug, Clone, Copy, Default)]
enum Announcing {
    #[default]
    Other,
    Open,
    Digits(u32),
    Plus(u32),
    Closed(u32),
    Cr(Option<u32>),
}

impl Announcing {
    /// The state after `byte`, or at the end of the line the length of the literal it
    /// announced, if any.
    fn next(self, byte: u8) -> Result<Self, Option<u32>> {
        use Announcing::*;
        Ok(match (self, byte) {
            (Closed(n), b'\n') => return Err(Some(n)),
            (Cr(announced), b'\n') => return Err(announced),
            (_, b'\n') => return Err(None),
            (Closed(n), b'\r') => Cr(Some(n)),
            (_, b'\r') => Cr(None),
            (_, b'{') => Open,
            (Open, b'0'..=b'9') => Digits(u32::from(byte - b'0')),
            // Past u32 it is no announcement, and what follows is read as lines.
            (Digits(n), b'0'..=b'9') => n
                .checked_mul(10)
                .and_then(|n| n.checked_add(u32::from(byte - b'0')))
                .map_or(Other, Digits),
            (Digits(n), b'+') => Plus(n),
            (Digits(n) | Plus(n), b'}') => Closed(n),
            _ => Other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut framer = Framer::default();
        let mut out = Vec::new();
        for chunk in chunks {
            framer.push(chunk).unwrap();
            out.extend(std::iter::from_fn(|| framer.next()));
        }
        out
    }

    #[test]
    fn a_response_includes_its_literals_however_the_reads_split_it() {
        let literal = b"Subject: x\r\n\r\n";
        let mut response =
            format!("* 1 FETCH (BODY[HEADER] {{{}}}\r\n", literal.len()).into_bytes();
        response.extend_from_slice(literal);
        response.extend_from_slice(b" UID 7)\r\n");
        let mut input = response.clone();
        input.extend_from_slice(b"C1 OK done\r\n");

        let expected = vec![response.clone(), b"C1 OK done\r\n".to_vec()];
        assert_eq!(frame(&[&input]), expected);
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(frame(&bytes), expected);
        let (a, b) = input.split_at(30);
        assert_eq!(frame(&[a, b]), expected);
    }

    #[test]
    fn literal_bytes_are_not_lines() {
        // A literal with a line break and what looks like an announcement inside.
        let literal = b"a {5}\nb";
        let mut input = format!("* 1 FETCH (BODY[] {{{}}}\r\n", literal.len()).into_bytes();
        input.extend_from_slice(literal);
        input.extend_from_slice(b")\r\n");
        assert_eq!(frame(&[&input]), [input]);
    }

    #[test]
    fn a_bare_lf_gets_its_cr() {
        assert_eq!(
            frame(&[b"* 3 EXISTS\n* OK [UIDNEXT 4] x\r\n"]),
            [
                b"* 3 EXISTS\r\n".to_vec(),
                b"* OK [UIDNEXT 4] x\r\n".to_vec()
            ]
        );
        // Also on a line that announces a literal; the literal itself stays as sent.
        assert_eq!(
            frame(&[b"* 1 FETCH (BODY[] {2}\na\n)\n"]),
            [b"* 1 FETCH (BODY[] {2}\r\na\n)\r\n".to_vec()]
        );
    }

    #[test]
    fn a_response_over_the_limit_is_refused() {
        let mut framer = Framer::with_max(64);
        assert!(framer.push(&[b'x'; 65]).is_err(), "a line without an end");
        let mut framer = Framer::with_max(64);
        assert!(
            framer.push(b"* 1 FETCH (BODY[] {1000}\r\n").is_err(),
            "announced"
        );
        let mut framer = Framer::with_max(64);
        for _ in 0..10 {
            framer.push(b"* 1 EXISTS\r\n").unwrap();
        }
        assert_eq!(
            std::iter::from_fn(|| framer.next()).count(),
            10,
            "per response"
        );
    }

    /// The literal `line` announces, if it is one whole line.
    fn announced(line: &[u8]) -> Option<u32> {
        let (last, body) = line.split_last()?;
        let state = body
            .iter()
            .try_fold(Announcing::Other, |state, &byte| state.next(byte).ok())?;
        state.next(*last).err()?
    }

    #[test]
    fn finds_literal_announcements() {
        assert_eq!(announced(b"* 1 FETCH (BODY[] {12}\r\n"), Some(12));
        assert_eq!(announced(b"a {0}\n"), Some(0));
        assert_eq!(announced(b"a {5+}\r\n"), Some(5));
        assert_eq!(announced(b"a ~{5}\r\n"), Some(5));
        assert_eq!(announced(b"a {00000000000000000005}\r\n"), Some(5));
        assert_eq!(announced(b"a {1{5}\r\n"), Some(5));
        assert_eq!(announced(b"a {5}\r\r\n"), None);
        assert_eq!(announced(b"a {}\r\n"), None);
        assert_eq!(announced(b"a {x}\r\n"), None);
        assert_eq!(announced(b"a {5+5}\r\n"), None);
        assert_eq!(announced(b"a {5} \r\n"), None);
        assert_eq!(announced(b"a {99999999999}\r\n"), None);
    }

    #[test]
    fn an_empty_literal_continues_the_response() {
        let input = b"* 1 FETCH (BODY[] {0}\r\n UID 3)\r\n";
        assert_eq!(frame(&[input]), [input.to_vec()]);
    }
}
