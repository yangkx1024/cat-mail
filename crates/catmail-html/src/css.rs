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
//!
//! The same rewriting moves a message's colors for a dark page ([`dark`](crate::dark)),
//! given a palette: colors in the values of properties that take them.

use cssparser::{
    Delimiter, ParseError, Parser, ToCss as _, Token, TokenSerializationType, serialize_identifier,
};

use crate::dark::{self, Color, DarkPalette, Role};

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

/// A message's style sheets as kept, and whether they adapt to a dark page.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Stylesheet {
    pub(super) css: String,
    /// It has styles of its own for a dark appearance: `prefers-color-scheme: dark`, or
    /// `color-scheme` naming `dark`.
    pub(super) adapts_to_dark: bool,
}

/// A `<style>` element of a message: its text, and its `media` attribute.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Element {
    pub(super) media: Option<String>,
    pub(super) css: String,
}

/// A message's style sheets, kept as one: each one parsed on its own (a rule left open
/// ends with its sheet), the rules of one for some media only inside an `@media` block.
/// With a palette, their colors are moved for a dark page.
pub(super) fn sanitize(elements: &[Element], recolor: Option<DarkPalette>) -> Stylesheet {
    let mut sheet = Sheet::new(MAX_LEN, recolor);
    for element in elements {
        if sheet.full() {
            break;
        }
        sheet.element(element);
    }
    Stylesheet {
        css: sheet.out.replace('<', "\\3c "),
        adapts_to_dark: sheet.dark,
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

/// A `style` attribute with its colors moved for a dark page.
pub(super) fn recolor_style(style: &str, palette: DarkPalette) -> String {
    let mut declarations = Declarations {
        recolor: Some(palette),
        ..Declarations::default()
    };
    declarations.parse(&mut Parser::new(style));
    declarations.out
}

struct Sheet {
    out: String,
    /// Bytes `out` may still take.
    budget: usize,
    rules: usize,
    dark: bool,
    recolor: Option<DarkPalette>,
}

impl Sheet {
    fn new(budget: usize, recolor: Option<DarkPalette>) -> Self {
        Self {
            out: String::new(),
            budget,
            rules: 0,
            dark: false,
            recolor,
        }
    }

    fn full(&self) -> bool {
        self.out.len() >= self.budget || self.rules >= MAX_RULES
    }

    /// A `<style>` element's rules, under its `media` attribute if it has one. One whose
    /// attribute isn't a media query list applies nowhere, as in a browser.
    fn element(&mut self, element: &Element) {
        let mut rules = Parser::new(&element.css);
        let Some(media) = element.media.as_deref().filter(|m| !m.trim().is_empty()) else {
            self.rules(&mut rules, 0);
            return;
        };
        let mut query = Parser::new(media);
        let mut prelude = Css::default();
        while let Ok(token) = query.next_including_whitespace().cloned() {
            // A `;` would end the at-rule it is put back in.
            if token == Token::Semicolon
                || copy(&mut query, &token, &mut prelude, Context::Prelude).is_err()
            {
                return;
            }
        }
        self.conditional("media", &prelude.text, 0, &mut rules);
    }

    /// The rules of a sheet or of a conditional block.
    fn rules(&mut self, parser: &mut Parser<'_>, depth: usize) {
        while !self.full() {
            let Ok(token) = parser.next().cloned() else {
                return;
            };
            match token {
                Token::AtKeyword(name) => self.at_rule(parser, &name, depth),
                // Left from hiding sheets from old browsers.
                Token::CDO | Token::CDC => {}
                token => self.style_rule(parser, &token),
            }
        }
    }

    fn style_rule<'i>(&mut self, parser: &mut Parser<'i>, first: &Token<'i>) {
        let mut selector = Css::default();
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
        let mut declarations = Declarations {
            recolor: self.recolor,
            ..Declarations::default()
        };
        let _ = parser.parse_nested_block(|block| {
            declarations.parse(block);
            Ok::<_, ParseError<()>>(())
        });
        let rule = format!("{}{{{}}}", selector.text.trim(), declarations.out);
        if !kept || declarations.out.is_empty() || self.out.len() + rule.len() > self.budget {
            return;
        }
        self.out.push_str(&rule);
        self.rules += 1;
        self.dark |= declarations.dark;
    }

    fn at_rule(&mut self, parser: &mut Parser<'_>, name: &str, depth: usize) {
        let conditional =
            name.eq_ignore_ascii_case("media") || name.eq_ignore_ascii_case("supports");
        let mut kept = conditional && depth < MAX_DEPTH;
        let mut prelude = Css::default();
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
        let name = name.to_ascii_lowercase();
        let _ = parser.parse_nested_block(|block| {
            self.conditional(&name, &prelude.text, depth, block);
            Ok::<_, ParseError<()>>(())
        });
    }

    /// `@name prelude { … }`, `@media` or `@supports`, with the rules in `block`.
    fn conditional(&mut self, name: &str, prelude: &str, depth: usize, block: &mut Parser<'_>) {
        let query = prelude.to_ascii_lowercase();
        let dark_query =
            name == "media" && query.contains("prefers-color-scheme") && query.contains("dark");
        let head = format!("@{name} {}{{", prelude.trim());
        let budget = self.budget.saturating_sub(self.out.len() + head.len() + 1);
        let mut inner = Sheet::new(budget, self.recolor);
        inner.rules = self.rules;
        inner.rules(block, depth + 1);
        self.rules = inner.rules;
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
    recolor: Option<DarkPalette>,
}

impl Declarations {
    fn parse(&mut self, parser: &mut Parser<'_>) {
        while !parser.is_exhausted() {
            // A declaration that isn't kept is skipped to the next `;`.
            let _ = parser.parse_until_after(Delimiter::Semicolon, |p| {
                let name = p.expect_ident()?.clone();
                p.expect_colon()?;
                let mut value = Css {
                    recolor: self.recolor.and_then(|p| Some((p, Role::of(&name)?))),
                    ..Css::default()
                };
                copy_rest(p, &mut value, Context::Value).map_err(|()| ParseError::custom(()))?;
                let value = value.text.trim();
                if value.is_empty() {
                    return Err(ParseError::custom(()));
                }
                let _ = serialize_identifier(&name, &mut self.out);
                self.out.push(':');
                self.out.push_str(value);
                self.out.push(';');
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

/// CSS written back token by token. What separated two tokens may be gone (a comment),
/// and written next to each other they could read as one: `u/**/rl(` as `url(`, `1px/**/2px`
/// as one dimension. `/**/` keeps them apart where the grammar needs it.
#[derive(Default)]
struct Css {
    text: String,
    last: TokenSerializationType,
    /// Moves the colors written, as used in the value of the property being written.
    recolor: Option<(DarkPalette, Role)>,
}

impl Css {
    fn push(&mut self, token: &Token<'_>) -> Result<(), ()> {
        let kind = token.serialization_type();
        if self.last.needs_separator_when_before(kind) {
            self.text.push_str("/**/");
        }
        self.last = kind;
        token.to_css(&mut self.text).map_err(|_| ())
    }

    fn color(&mut self, color: Color) -> Result<(), ()> {
        self.push(&Token::Hash(dark::hex(color).into()))
    }
}

/// Writes `token` (and the block it opens) to `out`, or refuses it: anything that
/// fetches, a nested rule, or what can't be written back faithfully.
fn copy<'i>(
    parser: &mut Parser<'i>,
    token: &Token<'i>,
    out: &mut Css,
    context: Context,
) -> Result<(), ()> {
    if is_fetching(token) {
        return Err(());
    }
    let close = match token {
        Token::Function(name) => {
            if context == Context::Selector && name.eq_ignore_ascii_case("has") {
                return Err(());
            }
            Token::CloseParenthesis
        }
        Token::ParenthesisBlock => Token::CloseParenthesis,
        Token::SquareBracketBlock => Token::CloseSquareBracket,
        Token::CurlyBracketBlock
        | Token::BadString(_)
        | Token::CDO
        | Token::CDC
        | Token::CloseParenthesis
        | Token::CloseSquareBracket
        | Token::CloseCurlyBracket => return Err(()),
        token => return out.push(token),
    };
    out.push(token)?;
    parser
        .parse_nested_block(|block| {
            copy_rest(block, out, context).map_err(|()| ParseError::custom(()))
        })
        .map_err(|_: ParseError<()>| ())?;
    out.push(&close)
}

/// Writes the rest of `parser` to `out` as [`copy`] does, the colors moved if `out`
/// recolors.
fn copy_rest(parser: &mut Parser<'_>, out: &mut Css, context: Context) -> Result<(), ()> {
    loop {
        if let Some((palette, role)) = out.recolor
            && let Ok(color) = parser.try_parse(parse_color)
        {
            out.color(palette.recolor(color, role))?;
            continue;
        }
        let Ok(token) = parser.next_including_whitespace().cloned() else {
            return Ok(());
        };
        copy(parser, &token, out, context)?;
    }
}

/// A color as CSS writes it, read from its source text: a hex color, a name, or a color
/// function (`rgb()`, `hsl()`, `oklch()`, …) with its arguments.
fn parse_color(parser: &mut Parser<'_>) -> Result<Color, ParseError<()>> {
    let start = parser.position();
    let function = match parser.next_including_whitespace()? {
        Token::Hash(_) | Token::IDHash(_) | Token::Ident(_) => false,
        Token::Function(_) => true,
        _ => return Err(ParseError::custom(())),
    };
    if function {
        parser.parse_nested_block(|block| {
            while block.next().is_ok() {}
            Ok::<_, ParseError<()>>(())
        })?;
    }
    dark::parse(parser.slice_from(start)).ok_or_else(|| ParseError::custom(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(source: &str) -> Stylesheet {
        sanitize(
            &[Element {
                media: None,
                css: source.into(),
            }],
            None,
        )
    }

    fn css(source: &str) -> String {
        sheet(source).css
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
    fn keeps_tokens_apart_that_a_comment_separated() {
        // Joined, `u` and `rl(` would be `url(`: a fetch the filter never saw.
        assert_eq!(
            css(r#".a{background:u/**/rl("https://t.example/p.gif")}"#),
            r#".a{background:u/**/rl("https://t.example/p.gif");}"#
        );
        assert_eq!(css(".a{margin:1px/**/2px}"), ".a{margin:1px/**/2px;}");
        assert_eq!(css("div/**/p{color:red}"), "div/**/p{color:red;}");
        for source in [
            r#".a{background:u/**/rl("https://t.example/p.gif")}"#,
            ".a{width:calc(1px/**/+2px)}",
            "@media scr/**/een{.a{color:red}}",
        ] {
            let once = css(source);
            assert_eq!(css(&once), once, "{source}");
        }
    }

    #[test]
    fn a_sheet_for_some_media_applies_only_to_them() {
        let element = |media: &str, css: &str| Element {
            media: Some(media.into()),
            css: css.into(),
        };
        let sheets = sanitize(
            &[
                element("print", "p{display:none}"),
                element(" ", ".a{color:red}"),
                element("(prefers-color-scheme: dark)", ".b{color:#eee}"),
            ],
            None,
        );
        assert_eq!(
            sheets.css,
            "@media print{p{display:none;}}.a{color:red;}\
             @media (prefers-color-scheme: dark){.b{color:#eee;}}"
        );
        assert!(sheets.adapts_to_dark);
        // A rule left open ends with its sheet: it doesn't take in the next one.
        let sheets = sanitize(
            &[
                element("print", "p{display:none"),
                Element {
                    media: None,
                    css: ".a{color:red}".into(),
                },
            ],
            None,
        );
        assert_eq!(sheets.css, "@media print{p{display:none;}}.a{color:red;}");
        // Not a media query list: the sheet applies nowhere.
        for media in ["print;", "print{}.x", "@import x", "url(x)"] {
            assert_eq!(
                sanitize(&[element(media, "p{display:none}")], None).css,
                "",
                "{media}"
            );
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
    fn finds_dark_styles() {
        assert!(!sheet(".a{color:#333}").adapts_to_dark);
        assert!(sheet("@media (prefers-color-scheme: dark){.a{color:#eee}}").adapts_to_dark);
        assert!(sheet(":root{color-scheme:light dark}").adapts_to_dark);
    }

    const PALETTE: DarkPalette = DarkPalette {
        background: [0x20; 3],
        foreground: [0xe0; 3],
    };

    fn recolored(source: &str) -> String {
        let element = Element {
            media: None,
            css: source.into(),
        };
        sanitize(&[element], Some(PALETTE)).css
    }

    #[test]
    fn moves_colors_for_a_dark_page() {
        // White goes to the page and black text to the reader's text, in any notation.
        assert_eq!(
            recolored(".a{background:#fff;color:black;padding:4px}"),
            ".a{background:#202020;color:#e0e0e0;padding:4px;}"
        );
        for white in [
            "WHITE",
            "#ffffff",
            "rgb(255, 255, 255)",
            "rgb(100% 100% 100%)",
            "hsl(0, 0%, 100%)",
            "hsl(120deg 0% 100%)",
            "oklch(1 0 0)",
        ] {
            assert_eq!(
                recolored(&format!(".a{{background-color:{white}}}")),
                ".a{background-color:#202020;}",
                "{white}"
            );
        }
        assert_eq!(
            recolored(".a{color:rgba(0,0,0,.5)}.b{color:rgb(0 0 0 / 25%)}"),
            ".a{color:#e0e0e080;}.b{color:#e0e0e040;}"
        );
        // Inside a value, a gradient, a conditional block; what is around stays.
        assert_eq!(
            recolored(
                "@media (max-width:600px){.a{border:1px solid #000!important;\
                 background-image:linear-gradient(#fff,#000)}}"
            ),
            "@media (max-width:600px){.a{border:1px solid #e0e0e0!important;\
             background-image:linear-gradient(#202020,#000000);}}"
        );
    }

    #[test]
    fn leaves_what_is_no_color_alone() {
        for kept in [
            "#fff{margin:0;}",
            ".a{font-family:Tomato,sans-serif;}",
            ".a{color:currentColor;background:transparent;}",
            ".a{color:inherit;}",
            ".a{color:rgb(0,0,0,0,0);}",
            ".a{color:#12345;}",
            ".a{content:\"#000\";}",
        ] {
            assert_eq!(recolored(kept), kept);
        }
    }

    #[test]
    fn moves_colors_in_a_style_attribute() {
        assert_eq!(
            recolor_style("color: #000; background-color :white;width:1px", PALETTE),
            "color:#e0e0e0;background-color:#202020;width:1px;"
        );
    }
}
