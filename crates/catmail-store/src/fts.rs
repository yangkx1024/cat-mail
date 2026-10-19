//! Text preparation for the `messages_fts` index.
//!
//! FTS5's unicode61 tokenizer splits on spaces and punctuation, which leaves a run of
//! Chinese, Japanese or Korean characters as one long token, and the trigram tokenizer
//! can't match two-character words. So CJK runs are segmented here, identically for
//! indexing and for queries: a run is indexed as its overlapping character pairs,
//! followed by its single characters.
//!
//! ```text
//! 季度报告  →  季度 度报 报告 季 度 报 告
//! ```
//!
//! A one-character query matches a single character, a two-character query matches a
//! pair, and a longer one becomes a phrase of consecutive pairs (`季度 度报 报告`).
//! Because the pairs of a run sit at consecutive positions, the phrase matches exactly
//! the original substring. Everything else is left to unicode61.

/// Rewrites `text` for indexing: CJK runs segmented, everything else unchanged.
pub(crate) fn segment(text: &str) -> String {
    if !text.chars().any(|c| is_cjk(c) || is_wide(c)) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() * 2);
    let mut run = Vec::new();
    for c in text.chars().map(fold_width) {
        if is_cjk(c) {
            run.push(c);
            continue;
        }
        flush_run(&mut out, &mut run, true);
        out.push(c);
    }
    flush_run(&mut out, &mut run, true);
    out
}

/// Builds an FTS5 MATCH expression from user input (see `SearchQuery` for the syntax).
/// Returns `None` if nothing in it is searchable. Every term is quoted, so input can
/// never be interpreted as FTS5 syntax.
pub(crate) fn match_expression(input: &str) -> Option<String> {
    let terms: Vec<String> = parse_terms(input)
        .into_iter()
        .filter_map(|(column, text)| {
            let phrases = phrases(&text);
            if phrases.is_empty() {
                return None;
            }
            let expr = phrases.join(" AND ");
            Some(match column {
                Some(column) => format!("{column} : ({expr})"),
                None if phrases.len() > 1 => format!("({expr})"),
                None => expr,
            })
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// Splits input into (column filter, text) terms: whitespace-separated words or
/// `"quoted phrases"`, each optionally prefixed with `from:`, `to:` or `subject:`.
fn parse_terms(input: &str) -> Vec<(Option<&'static str>, String)> {
    let mut terms = Vec::new();
    let mut chars = input.chars().peekable();
    loop {
        // Control characters (a pasted NUL ends an FTS5 string) separate terms too.
        while chars
            .next_if(|c| c.is_whitespace() || c.is_control())
            .is_some()
        {}
        if chars.peek().is_none() {
            return terms;
        }
        let mut word = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
            } else if (c.is_whitespace() || c.is_control()) && !quoted {
                break;
            } else if c.is_control() {
                word.push(' '); // inside a phrase
            } else {
                word.push(c);
            }
            chars.next();
        }
        let filter = COLUMN_FILTERS.iter().find_map(|(prefix, column)| {
            let head = word.get(..prefix.len())?;
            head.eq_ignore_ascii_case(prefix)
                .then(|| (*column, word[prefix.len()..].to_owned()))
        });
        terms.push(match filter {
            Some((column, text)) => (Some(column), text),
            None => (None, word),
        });
    }
}

/// Query prefixes and the index columns they restrict a term to.
const COLUMN_FILTERS: [(&str, &str); 3] = [
    ("from:", "from_text"),
    ("to:", "to_text"),
    ("subject:", "subject"),
];

/// The quoted FTS5 strings a term must all match: one per stretch of CJK and of other
/// text. Mixed stretches are matched separately rather than as one phrase, since a CJK
/// run's single characters sit between its pairs and whatever follows it.
fn phrases(term: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut other = String::new();
    let mut run = Vec::new();
    let push_run = |phrases: &mut Vec<String>, run: &mut Vec<char>| {
        let mut segmented = String::new();
        flush_run(&mut segmented, run, false);
        push_phrase(phrases, segmented);
    };
    for c in term.chars().map(fold_width) {
        if is_cjk(c) {
            push_phrase(&mut phrases, std::mem::take(&mut other));
            run.push(c);
        } else {
            push_run(&mut phrases, &mut run);
            other.push(c);
        }
    }
    push_run(&mut phrases, &mut run);
    push_phrase(&mut phrases, other);
    phrases
}

/// Quotes `text` as an FTS5 string, unless it holds nothing the tokenizer would index
/// (an empty phrase is an error).
fn push_phrase(phrases: &mut Vec<String>, text: String) {
    if text.chars().any(char::is_alphanumeric) {
        phrases.push(format!("\"{}\"", text.trim().replace('"', "\"\"")));
    }
}

/// Writes a CJK run as space-separated pairs, then (when indexing) its single
/// characters. A one-character run is just that character.
fn flush_run(out: &mut String, run: &mut Vec<char>, index: bool) {
    if run.is_empty() {
        return;
    }
    out.push(' ');
    if run.len() == 1 {
        out.push(run[0]);
    } else {
        for pair in run.windows(2) {
            out.extend(pair);
            out.push(' ');
        }
        if index {
            for &c in run.iter() {
                out.push(c);
                out.push(' ');
            }
        }
        out.pop();
    }
    out.push(' ');
    run.clear();
}

fn is_wide(c: char) -> bool {
    fold_width(c) != c
}

/// Full-width ASCII (`ｉＰｈｏｎｅ`, `，`) as typed with CJK input methods, folded to
/// its ASCII form so it matches either way.
fn fold_width(c: char) -> char {
    match c {
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xff01 + 0x21).unwrap_or(c),
        '\u{3000}' => ' ',
        _ => c,
    }
}

/// Han, kana, Hangul and Bopomofo: scripts written without spaces between words.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11ff}'     // Hangul Jamo
        | '\u{3005}'                // 々
        | '\u{3040}'..='\u{30ff}'   // Hiragana, Katakana
        | '\u{3100}'..='\u{312f}'   // Bopomofo
        | '\u{3130}'..='\u{318f}'   // Hangul Compatibility Jamo
        | '\u{31f0}'..='\u{31ff}'   // Katakana Phonetic Extensions
        | '\u{3400}'..='\u{4dbf}'   // CJK Extension A
        | '\u{4e00}'..='\u{9fff}'   // CJK Unified Ideographs
        | '\u{ac00}'..='\u{d7af}'   // Hangul Syllables
        | '\u{f900}'..='\u{faff}'   // CJK Compatibility Ideographs
        | '\u{ff66}'..='\u{ff9f}'   // Half-width Katakana
        | '\u{20000}'..='\u{3134f}' // CJK Extensions B–H, Compatibility Supplement
    )
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }

    #[test]
    fn segments_cjk_runs() {
        assert_eq!(
            words(&segment("季度报告")),
            ["季度", "度报", "报告", "季", "度", "报", "告"]
        );
        assert_eq!(
            words(&segment("Re: 你好，world 我")),
            ["Re:", "你好", "你", "好", ",world", "我"]
        );
        assert_eq!(
            words(&segment("iPhone手机")),
            ["iPhone", "手机", "手", "机"]
        );
        assert_eq!(segment("plain text"), "plain text");
        assert_eq!(words(&segment("ｉＰｈｏｎｅ")), ["iPhone"]);
    }

    #[test]
    fn builds_quoted_match_expressions() {
        assert_eq!(match_expression("  "), None);
        assert_eq!(match_expression("*** ()"), None);
        assert_eq!(match_expression("hello").unwrap(), r#""hello""#);
        assert_eq!(
            match_expression(r#"from:alice "quarterly report" 季度报告"#).unwrap(),
            r#"from_text : ("alice") AND "quarterly report" AND "季度 度报 报告""#
        );
        assert_eq!(
            match_expression("Subject:手机iPhone").unwrap(),
            r#"subject : ("手机" AND "iPhone")"#
        );
        assert_eq!(match_expression("hel\0lo").unwrap(), r#""hel" AND "lo""#);
        assert_eq!(match_expression("\"a\0b\"").unwrap(), r#""a b""#);
        // Injection attempts stay inside one string.
        assert_eq!(
            match_expression(r#"a"OR"b NEAR(x)"#).unwrap(),
            r#""aORb" AND "NEAR(x)""#
        );
    }

    /// Runs queries against a real FTS5 table, as configured in the schema.
    fn search(docs: &[&str], query: &str) -> Vec<i64> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5 (body, content = '', contentless_delete = 1,
                                               tokenize = 'unicode61 remove_diacritics 2')",
        )
        .unwrap();
        for (i, doc) in docs.iter().enumerate() {
            conn.execute(
                "INSERT INTO t (rowid, body) VALUES (?1, ?2)",
                rusqlite::params![i as i64, segment(doc)],
            )
            .unwrap();
        }
        let Some(expr) = match_expression(query) else {
            return Vec::new();
        };
        let mut stmt = conn
            .prepare("SELECT rowid FROM t WHERE t MATCH ?1 ORDER BY rowid")
            .unwrap();
        stmt.query_map([expr], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn cjk_queries_match_substrings() {
        let docs = [
            "第三季度报告已发送", // 0
            "报告季度",           // 1
            "Café 季度会议",      // 2
            "我们的iPhone手机壳", // 3
        ];
        assert_eq!(
            search(&docs, "季度报告"),
            [0],
            "phrase of consecutive pairs"
        );
        assert_eq!(search(&docs, "季度"), [0, 1, 2]);
        assert_eq!(
            search(&docs, "度报"),
            [0],
            "pairs don't cross into other runs"
        );
        assert_eq!(search(&docs, "报"), [0, 1]);
        assert_eq!(search(&docs, "三季度报告已发"), [0]);
        assert_eq!(search(&docs, "报告 季度"), [0, 1], "terms are ANDed");
        assert_eq!(search(&docs, "cafe"), [2], "diacritics are ignored");
        assert_eq!(search(&docs, "iphone手机"), [3]);
        assert_eq!(search(&docs, "ｉｐｈｏｎｅ"), [3]);
        assert_eq!(search(&docs, "手机壳"), [3]);
        assert_eq!(search(&docs, "季度报告会议"), [] as [i64; 0]);
    }

    #[test]
    fn other_scripts_match_whole_words() {
        let docs = [
            "Quarterly report attached",
            "reporting tools",
            "mail from alice@example.com",
        ];
        assert_eq!(search(&docs, "report"), [0]);
        assert_eq!(search(&docs, "\"report attached\""), [0]);
        assert_eq!(search(&docs, "\"attached report\""), [] as [i64; 0]);
        assert_eq!(search(&docs, "alice@example.com"), [2]);
        assert_eq!(search(&docs, "QUARTERLY"), [0]);
    }
}
