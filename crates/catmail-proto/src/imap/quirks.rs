//! Repairs server responses that imap-codec rejects, on the bytes before imap-next
//! parses them. imap-codec drops a whole response it can't parse, so one bad field
//! would hide a message from sync, every time.

/// The FETCH item whose value `pad_internaldate_day` repairs.
const INTERNALDATE: &[u8] = b"INTERNALDATE \"";

/// A line longer than this is passed on before it ends, so that imap-next's limit on a
/// response's size still applies to a server that never ends one.
const LONG_LINE: usize = 64 * 1024;

/// What a line passed on early keeps back: enough for the repair to see a day after
/// `INTERNALDATE "` whole.
const KEEP: usize = INTERNALDATE.len() + 1;

/// Rewrites response lines, passing literals through untouched: they are message
/// content, and their announced length must stay right. Lines and literals are told
/// apart the way imap-codec's fragmentizer does it, so both agree on where a literal
/// starts.
#[derive(Debug, Default)]
pub(crate) struct Quirks {
    /// The current line, held until it is complete or long.
    line: Vec<u8>,
    /// How far the current line is into announcing a literal.
    announcing: Announcing,
    /// Bytes of the current literal still to come.
    literal: usize,
    /// What `push` returns, kept to reuse its allocation.
    out: Vec<u8>,
}

impl Quirks {
    /// Returns the bytes ready for the parser: whole lines, repaired, and literal data.
    pub fn push(&mut self, mut input: &[u8]) -> &[u8] {
        self.out.clear();
        while !input.is_empty() {
            if self.literal > 0 {
                let n = self.literal.min(input.len());
                self.out.extend_from_slice(&input[..n]);
                self.literal -= n;
                input = &input[n..];
                continue;
            }
            let Some((end, literal)) = self.line_end(input) else {
                self.line.extend_from_slice(input);
                if self.line.len() > LONG_LINE {
                    // Repaired as far as it can be; what could still be part of a repair
                    // stays. Whether it announces a literal is tracked apart from it.
                    pad_internaldate_day(&mut self.line);
                    let ready = self.line.len() - KEEP;
                    self.out.extend(self.line.drain(..ready));
                }
                break;
            };
            self.line.extend_from_slice(&input[..=end]);
            input = &input[end + 1..];
            pad_internaldate_day(&mut self.line);
            self.literal = literal;
            self.out.append(&mut self.line);
        }
        &self.out
    }

    /// Where in `input` the current line ends, and the length of the literal it announced;
    /// `None` if it goes on past `input`.
    fn line_end(&mut self, input: &[u8]) -> Option<(usize, usize)> {
        for (ix, &byte) in input.iter().enumerate() {
            match self.announcing.next(byte) {
                Ok(next) => self.announcing = next,
                Err(literal) => {
                    self.announcing = Announcing::Other;
                    return Some((ix, literal.map_or(0, |n| n as usize)));
                }
            }
        }
        None
    }
}

/// How far a line is into announcing a literal (`{n}` or `{n+}`, then CRLF or LF), byte by
/// byte as imap-codec's fragmentizer follows it. The announcement can be any length
/// (`{0000…5}`), so it can't be read off the end of a line passed on in part.
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

/// QQ Mail sends `INTERNALDATE "2-Oct-2026 …"`, but RFC 3501's `date-day-fixed` is
/// `" 2"` or `"02"`; pad the day with the space the grammar asks for. Inside a quoted
/// string the quote would be escaped (`\"`), so this only matches the FETCH item.
fn pad_internaldate_day(line: &mut Vec<u8>) {
    let mut from = 0;
    while let Some(at) = line[from..]
        .windows(INTERNALDATE.len())
        .position(|w| w.eq_ignore_ascii_case(INTERNALDATE))
    {
        let day = from + at + INTERNALDATE.len();
        if line.get(day).is_some_and(u8::is_ascii_digit) && line.get(day + 1) == Some(&b'-') {
            line.insert(day, b' ');
        }
        from = day;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repair(chunks: &[&[u8]]) -> Vec<u8> {
        let mut quirks = Quirks::default();
        chunks
            .iter()
            .flat_map(|c| quirks.push(c).to_vec())
            .collect()
    }

    #[test]
    fn pads_a_one_digit_day() {
        let line = b"* 1 FETCH (UID 7 INTERNALDATE \"2-Oct-2026 08:13:27 +0800\")\r\n";
        assert_eq!(
            repair(&[line]),
            b"* 1 FETCH (UID 7 INTERNALDATE \" 2-Oct-2026 08:13:27 +0800\")\r\n"
        );
        let lower = b"* 1 FETCH (internaldate \"9-Oct-2026 08:13:27 +0800\")\n";
        assert_eq!(
            repair(&[lower]),
            b"* 1 FETCH (internaldate \" 9-Oct-2026 08:13:27 +0800\")\n"
        );
    }

    #[test]
    fn leaves_valid_dates_alone() {
        for line in [
            &b"* 1 FETCH (INTERNALDATE \"12-Oct-2026 08:13:27 +0800\")\r\n"[..],
            b"* 1 FETCH (INTERNALDATE \" 2-Oct-2026 08:13:27 +0800\")\r\n",
            b"* 1 FETCH (INTERNALDATE \"02-Oct-2026 08:13:27 +0800\")\r\n",
            b"* OK [ALERT] INTERNALDATE \"\r\n",
        ] {
            assert_eq!(repair(&[line]), line);
        }
    }

    #[test]
    fn literals_pass_through_untouched() {
        let literal = b"Subject: INTERNALDATE \"2-Oct-2026\r\n\r\n";
        let mut input = format!("* 1 FETCH (BODY[HEADER] {{{}}}\r\n", literal.len()).into_bytes();
        input.extend_from_slice(literal);
        input.extend_from_slice(b" INTERNALDATE \"3-Oct-2026 08:13:27 +0800\")\r\n");
        let expected = {
            let mut e = input.clone();
            let at = e.len() - b"3-Oct-2026 08:13:27 +0800\")\r\n".len();
            e.insert(at, b' ');
            e
        };
        assert_eq!(repair(&[&input]), expected);

        // However the reads split it.
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(repair(&bytes), expected);
        let (a, b) = input.split_at(30);
        assert_eq!(repair(&[a, b]), expected);
    }

    #[test]
    fn a_line_without_an_end_is_passed_on() {
        let mut quirks = Quirks::default();
        let mut passed = 0;
        for _ in 0..100 {
            passed += quirks.push(&[b'x'; 8192]).len();
        }
        assert!(quirks.line.len() <= LONG_LINE, "{}", quirks.line.len());
        assert_eq!(passed + quirks.line.len(), 100 * 8192);
    }

    #[test]
    fn a_long_line_is_still_repaired_and_its_literal_found() {
        let fill = vec![b'x'; LONG_LINE];
        // The repair falls across where the line is cut, the literal comes after.
        let mut line = b"* 1 FETCH (X ".to_vec();
        line.extend_from_slice(&fill[..LONG_LINE - 20]);
        line.extend_from_slice(
            b" INTERNALDATE \"2-Oct-2026 08:13:27 +0800\" BODY[] {3}\r\nabc)\r\n",
        );
        let expected = String::from_utf8(line.clone())
            .unwrap()
            .replace("\"2-Oct", "\" 2-Oct");
        for cut in [LONG_LINE + 4, LONG_LINE + 15, LONG_LINE + 30] {
            let (a, b) = line.split_at(cut);
            let repaired = repair(&[a, b]);
            assert_eq!(
                String::from_utf8(repaired).unwrap(),
                expected,
                "cut at {cut}"
            );
        }
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
    fn finds_literal_announcements_like_imap_codec() {
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
    fn an_announcement_longer_than_what_a_long_line_keeps() {
        // Leading zeros are allowed: the `{` is long gone by the time the line ends.
        let literal = b"INTERNALDATE \"2-Oct";
        let mut input = b"* 1 FETCH (BODY[] {".to_vec();
        input.extend(std::iter::repeat_n(b'0', 2 * LONG_LINE));
        input.extend(format!("{}}}\r\n", literal.len()).bytes());
        input.extend_from_slice(literal);
        input.extend_from_slice(b")\r\n");
        let chunks: Vec<&[u8]> = input.chunks(4096).collect();
        assert_eq!(repair(&chunks), input);
    }
}
