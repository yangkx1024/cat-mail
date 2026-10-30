//! A message's HTML: made safe to render, and what readers need from it.
//!
//! [`sanitize_html`] is the one cleaning, applied when a body is parsed, before it is
//! stored: no scripts or active content, style sheets and `style` attributes rewritten so
//! that they can't fetch anything (`css.rs`). The other passes run on what it produced, with the same sanitizer,
//! so they can't let through what it didn't: the images a reader resolves or blocks, the
//! inline parts embedded for a browser, and the colors moved for a dark page (`dark.rs`).
//!
//! Pure functions over strings: no IO, nothing async, nothing of mail protocols.

mod css;
mod dark;
mod images;
mod sanitize;
mod tokens;

pub use dark::DarkPalette;
pub use images::{
    InlineImage, MAX_REMOTE_IMAGES, content_id_from_url, embed_inline_images, remote_image_url,
    remote_image_urls,
};
pub use sanitize::{recolor_html, sanitize_html};
