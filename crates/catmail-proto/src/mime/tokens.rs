//! The html5ever tokenizer, as the passes over sanitized HTML drive it: no tree builder,
//! which would move content around again.

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

/// Runs `sink` over `html` and hands it back.
pub(super) fn tokenize<S: TokenSink>(html: &str, sink: S) -> S {
    let input = BufferQueue::default();
    input.push_back(html.into());
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink
}

/// The tokenizer state the tree builder switches to after the start tag `name` (in HTML,
/// not SVG or MathML), so a sink sees `<style>` text, or `<style>` inside `<textarea>`,
/// as a browser would.
pub(super) fn after_start_tag<H>(name: &str) -> TokenSinkResult<H> {
    match name {
        "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
            TokenSinkResult::RawData(RawKind::Rawtext)
        }
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
        "plaintext" => TokenSinkResult::Plaintext,
        _ => TokenSinkResult::Continue,
    }
}
