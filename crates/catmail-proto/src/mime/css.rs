//! Style sheets from a message, rewritten so that they can't fetch anything or end the
//! `<style>` element they are put back in.
//!
//! Rules are written again from cssparser's tokens, never copied as text, so escapes are
//! already decoded when they are judged (`\75rl(` is `url(`). What isn't understood is
//! dropped: at-rules other than `@media` and `@supports`, declarations whose value
//! fetches (`url()`, `image-set()`, …), selectors with `:has()` (matched again on every
//! change, costly for a page built to be costly), nested rules. The output has no `<`
//! (written `\3c `), so neither `</style>` nor an HTML comment can appear in it, however
//! the HTML around it is parsed again: the sheet can't change the document's structure.

use cssparser::{Delimiter, ParseError, Parser, ToCss as _, Token, serialize_identifier};

/// The most of a message's style sheets that is kept, in bytes of output: email sheets
/// are a few kilobytes, a sheet much larger is built to be slow.
pub(super) const MAX_LEN: usize = 64 * 1024;
const MAX_RULES: usize = 2_000;
/// How deep `@media` and `@supports` may nest.
const MAX_DEPTH: usize = 3;

/// Functions that fetch something, or show other content of the page as an image.
const FETCHING: &[&str] = &[
    "url",
    "src",
    "image",
    "image-set",
    "-webkit-image-set",
    "cross-fade",
    "-webkit-cross-fade",
    "element",
    "-moz-element",
    "paint",
];

/// Properties that color text or its background.
const COLORS: &[&str] = &["color", "background", "background-color"];

/// A message's style sheets as kept, and what they say about its colors.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Stylesheet {
    pub(super) css: String,
    /// It has styles of its own for a dark appearance: `prefers-color-scheme: dark`, or
    /// `color-scheme` naming `dark`.
    pub(super) adapts_to_dark: bool,
    /// It colors text or backgrounds, outside its dark styles.
    pub(super) sets_colors: bool,
}

pub(super) fn sanitize(css: &str) -> Stylesheet {
    let mut parser = Parser::new(css);
    let mut sheet = Sheet::new(MAX_LEN);
    sheet.rules(&mut parser, 0, false);
    Stylesheet {
        css: sheet.out.replace('<', "\\3c "),
        adapts_to_dark: sheet.dark,
        sets_colors: sheet.colors,
    }
}

/// Whether CSS (a `style` attribute) would fetch something, anywhere in it: not only in
/// the declarations that parse, since what a browser's error recovery keeps is not
/// judged here. ammonia writes the attribute back as it is.
///
/// Also a fetching name before a parenthesis, `url (…)`: no function by the grammar, but
/// nothing to keep a style for either.
pub(super) fn fetches(css: &str) -> bool {
    fn any(parser: &mut Parser<'_>) -> bool {
        let mut after_fetching_name = false;
        while let Ok(token) = parser.next().cloned() {
            let spaced_call = after_fetching_name && token == Token::ParenthesisBlock;
            if is_fetching(&token) || spaced_call {
                return true;
            }
            after_fetching_name = matches!(&token, Token::Ident(name)
                if FETCHING.iter().any(|f| name.eq_ignore_ascii_case(f)));
            let block = matches!(
                token,
                Token::Function(_)
                    | Token::ParenthesisBlock
                    | Token::SquareBracketBlock
                    | Token::CurlyBracketBlock
            );
            if block
                && parser
                    .parse_nested_block(|inner| Ok::<_, ParseError<()>>(any(inner)))
                    .unwrap_or(true)
            {
                return true;
            }
        }
        false
    }
    any(&mut Parser::new(css))
}

/// A token that fetches: a URL, a function that loads an image or shows other content,
/// or an at-rule (`@import` has no place in a declaration).
fn is_fetching(token: &Token<'_>) -> bool {
    match token {
        Token::Function(name) => FETCHING.iter().any(|f| name.eq_ignore_ascii_case(f)),
        Token::UnquotedUrl(_) | Token::BadUrl(_) | Token::AtKeyword(_) => true,
        _ => false,
    }
}

/// Whether a `style` attribute colors text or its background.
pub(super) fn sets_colors(style: &str) -> bool {
    let mut parser = Parser::new(style);
    let mut declarations = Declarations::default();
    declarations.parse(&mut parser);
    declarations.colors
}

struct Sheet {
    out: String,
    /// Bytes `out` may still take.
    budget: usize,
    rules: usize,
    dark: bool,
    colors: bool,
}

impl Sheet {
    fn new(budget: usize) -> Self {
        Self {
            out: String::new(),
            budget,
            rules: 0,
            dark: false,
            colors: false,
        }
    }

    fn full(&self) -> bool {
        self.out.len() >= self.budget || self.rules >= MAX_RULES
    }

    /// The rules of a sheet or of a conditional block; `dark` inside dark styles.
    fn rules(&mut self, parser: &mut Parser<'_>, depth: usize, dark: bool) {
        while !self.full() {
            let Ok(token) = parser.next().cloned() else {
                return;
            };
            match token {
                Token::AtKeyword(name) => self.at_rule(parser, &name, depth, dark),
                // Left from hiding sheets from old browsers.
                Token::CDO | Token::CDC => {}
                token => self.style_rule(parser, &token, dark),
            }
        }
    }

    fn style_rule<'i>(&mut self, parser: &mut Parser<'i>, first: &Token<'i>, dark: bool) {
        let mut selector = String::new();
        let mut kept = copy(parser, first, &mut selector, Context::Selector).is_ok();
        loop {
            // Ended without a block: not a rule.
            let Ok(token) = parser.next_including_whitespace().cloned() else {
                return;
            };
            match token {
                Token::CurlyBracketBlock => break,
                token => {
                    kept = kept && copy(parser, &token, &mut selector, Context::Selector).is_ok()
                }
            }
        }
        let mut declarations = Declarations::default();
        let _ = parser.parse_nested_block(|block| {
            declarations.parse(block);
            Ok::<_, ParseError<()>>(())
        });
        let rule = format!("{}{{{}}}", selector.trim(), declarations.out);
        if !kept || declarations.out.is_empty() || self.out.len() + rule.len() > self.budget {
            return;
        }
        self.out.push_str(&rule);
        self.rules += 1;
        self.dark |= declarations.dark;
        self.colors |= declarations.colors && !dark;
    }

    fn at_rule(&mut self, parser: &mut Parser<'_>, name: &str, depth: usize, dark: bool) {
        let conditional =
            name.eq_ignore_ascii_case("media") || name.eq_ignore_ascii_case("supports");
        let mut kept = conditional && depth < MAX_DEPTH;
        let mut prelude = String::new();
        loop {
            let Ok(token) = parser.next_including_whitespace().cloned() else {
                return;
            };
            match token {
                // `@import`, `@charset`, `@namespace`: never kept.
                Token::Semicolon => return,
                Token::CurlyBracketBlock => break,
                // A block not entered is skipped whole by the next call.
                token if kept => {
                    kept = copy(parser, &token, &mut prelude, Context::Prelude).is_ok()
                }
                _ => {}
            }
        }
        if !kept {
            return;
        }
        let query = prelude.to_ascii_lowercase();
        let dark_query = name.eq_ignore_ascii_case("media")
            && query.contains("prefers-color-scheme")
            && query.contains("dark");
        let head = format!("@{} {}{{", name.to_ascii_lowercase(), prelude.trim());
        let mut inner = Sheet::new(self.budget.saturating_sub(self.out.len() + head.len() + 1));
        inner.rules = self.rules;
        let _ = parser.parse_nested_block(|block| {
            inner.rules(block, depth + 1, dark || dark_query);
            Ok::<_, ParseError<()>>(())
        });
        self.rules = inner.rules;
        self.colors |= inner.colors;
        self.dark |= inner.dark || dark_query;
        if !inner.out.is_empty() {
            self.out.push_str(&head);
            self.out.push_str(&inner.out);
            self.out.push('}');
        }
    }
}

/// The declarations of a rule or a `style` attribute.
#[derive(Default)]
struct Declarations {
    out: String,
    dark: bool,
    colors: bool,
}

impl Declarations {
    fn parse(&mut self, parser: &mut Parser<'_>) {
        while !parser.is_exhausted() {
            // A declaration that isn't kept is skipped to the next `;`.
            let _ = parser.parse_until_after(Delimiter::Semicolon, |p| {
                let name = p.expect_ident()?.clone();
                p.expect_colon()?;
                let mut value = String::new();
                while let Ok(token) = p.next_including_whitespace().cloned() {
                    copy(p, &token, &mut value, Context::Value)
                        .map_err(|()| ParseError::custom(()))?;
                }
                let value = value.trim();
                if value.is_empty() {
                    return Err(ParseError::custom(()));
                }
                let _ = serialize_identifier(&name, &mut self.out);
                self.out.push(':');
                self.out.push_str(value);
                self.out.push(';');
                if COLORS.iter().any(|c| name.eq_ignore_ascii_case(c)) {
                    self.colors = true;
                }
                if name.eq_ignore_ascii_case("color-scheme")
                    && value.to_ascii_lowercase().contains("dark")
                {
                    self.dark = true;
                }
                Ok::<_, ParseError<()>>(())
            });
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Context {
    Selector,
    Prelude,
    Value,
}

/// Writes `token` (and the block it opens) to `out`, or refuses it: anything that
/// fetches, a nested rule, or what can't be written back faithfully.
fn copy<'i>(
    parser: &mut Parser<'i>,
    token: &Token<'i>,
    out: &mut String,
    context: Context,
) -> Result<(), ()> {
    if is_fetching(token) {
        return Err(());
    }
    let (open, close) = match token {
        Token::Function(name) => {
            if context == Context::Selector && name.eq_ignore_ascii_case("has") {
                return Err(());
            }
            serialize_identifier(name, out).map_err(|_| ())?;
            ("(", ")")
        }
        Token::ParenthesisBlock => ("(", ")"),
        Token::SquareBracketBlock => ("[", "]"),
        Token::CurlyBracketBlock
        | Token::BadString(_)
        | Token::CDO
        | Token::CDC
        | Token::CloseParenthesis
        | Token::CloseSquareBracket
        | Token::CloseCurlyBracket => return Err(()),
        token => return token.to_css(out).map_err(|_| ()),
    };
    out.push_str(open);
    parser
        .parse_nested_block(|block| {
            while let Ok(token) = block.next_including_whitespace().cloned() {
                copy(block, &token, out, context).map_err(|()| ParseError::custom(()))?;
            }
            Ok::<_, ParseError<()>>(())
        })
        .map_err(|_| ())?;
    out.push_str(close);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(source: &str) -> String {
        sanitize(source).css
    }

    #[test]
    fn keeps_rules_and_conditional_blocks() {
        assert_eq!(
            css(".a  p , td.b { color : red ; padding:4px 8px !important }"),
            ".a  p , td.b{color:red;padding:4px 8px !important;}"
        );
        assert_eq!(
            css(
                "@media screen and (max-width:600px){.a{width:100%}} @supports (display:grid){.b{display:grid}}"
            ),
            "@media screen and (max-width:600px){.a{width:100%;}}@supports (display:grid){.b{display:grid;}}"
        );
        assert_eq!(css("<!-- .a{color:red} -->"), ".a{color:red;}");
    }

    #[test]
    fn drops_what_fetches_and_keeps_the_rest_of_the_rule() {
        assert_eq!(
            css(".a{color:red;background:url(https://t.example/p.gif);margin:0}"),
            ".a{color:red;margin:0;}"
        );
        for fetching in [
            r#".a{background:url("https://t.example/p.gif")}"#,
            r".a{background:\75rl(https://t.example/p.gif)}",
            r".a{background:u\72 l(https://t.example/p.gif)}",
            r#".a{background-image:image-set("p.png" 1x)}"#,
            r#".a{background-image:-webkit-image-set("p.png" 1x)}"#,
            r#".a{background:cross-fade(url(a.png),url(b.png))}"#,
            r#".a{list-style:image("p.png")}"#,
            r#".a{cursor:url(c.cur),auto}"#,
            r#".a{background:element(#x)}"#,
            r#".a{--x:url(p.png)}"#,
            r#".a{background:URL(p.png)}"#,
        ] {
            assert_eq!(css(fetching), "", "{fetching}");
        }
        for at_rule in [
            "@import url(https://t.example/s.css);.a{color:red}",
            "@import 'https://t.example/s.css';.a{color:red}",
            "@font-face{font-family:x;src:url(https://t.example/f.woff)}.a{color:red}",
            "@\\69mport 'https://t.example/s.css';.a{color:red}",
            "@namespace svg url(http://www.w3.org/2000/svg);.a{color:red}",
        ] {
            assert_eq!(css(at_rule), ".a{color:red;}", "{at_rule}");
        }
    }

    #[test]
    fn drops_costly_selectors_and_nested_rules() {
        assert_eq!(
            css("div:has(> p){color:red}.b{color:blue}"),
            ".b{color:blue;}"
        );
        assert_eq!(css(".a{color:red;&:hover{color:blue}}"), ".a{color:red;}");
        assert_eq!(
            css("@media a{@media b{@media c{@media d{.x{color:red}}}}}"),
            "",
            "nested too deep"
        );
    }

    #[test]
    fn output_has_no_markup() {
        let sheet = css(r#".a::after{content:"</style><img src=x onerror=alert(1)>"}"#);
        assert!(!sheet.contains('<'), "{sheet}");
        assert!(sheet.contains(r"\3c /style>"), "{sheet}");
    }

    #[test]
    fn rewriting_is_stable() {
        for source in [
            ".a p{color:red;margin:0 auto}@media (max-width:600px){.a{width:100%!important}}",
            r#".a::after{content:"<b>"}"#,
        ] {
            let once = css(source);
            assert_eq!(css(&once), once);
        }
    }

    #[test]
    fn stops_at_the_size_limit() {
        // Long rules reach the size limit; short ones, the number of rules.
        let rule = format!(".a{{content:\"{}\";}}", "x".repeat(100));
        let kept = css(&rule.repeat(2 * MAX_LEN / rule.len()));
        assert!(
            kept.len() <= MAX_LEN && kept.len() > MAX_LEN - rule.len(),
            "{}",
            kept.len()
        );
        assert!(kept.ends_with('}'));
        let kept = css(&".a{color:red;}".repeat(2 * MAX_RULES));
        assert_eq!(kept.matches('{').count(), MAX_RULES);
    }

    #[test]
    fn finds_what_fetches_in_a_style_attribute() {
        for fetching in [
            "background:url(https://t.example/p.gif)",
            r"background:\75rl(https://t.example/p.gif)",
            "background:URL('p.gif')",
            "background-image:image-set('p.png' 1x)",
            "x:(cross-fade(a, b))",
            "padding:0;}{background:url(p.gif)",
            "@import 'x'",
        ] {
            assert!(fetches(fetching), "{fetching}");
        }
        for inert in [
            "padding:4px;color:#333",
            "font-family:'url(x)'",
            "content:'@import'",
            "",
        ] {
            assert!(!fetches(inert), "{inert}");
        }
    }

    #[test]
    fn tells_colors_and_dark_styles_apart() {
        let plain = sanitize(".a{padding:4px;font-size:14px}");
        assert!(!plain.sets_colors && !plain.adapts_to_dark);
        let light = sanitize(".a{color:#333}");
        assert!(light.sets_colors && !light.adapts_to_dark);
        let dark_only = sanitize("@media (prefers-color-scheme: dark){.a{color:#eee}}");
        assert!(!dark_only.sets_colors && dark_only.adapts_to_dark);
        assert!(sanitize(":root{color-scheme:light dark}").adapts_to_dark);

        assert!(sets_colors("padding:0;background-color:#fff"));
        assert!(!sets_colors("padding:0;font-weight:bold"));
    }
}
