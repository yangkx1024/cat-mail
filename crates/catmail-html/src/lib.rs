//! A message's HTML: made safe to render, and what readers need from it.
//!
//! [`sanitize_html`] is the one cleaning, applied when a body is parsed, before it is
//! stored: no scripts or active content, style sheets and `style` attributes rewritten so
//! that they can't fetch anything (`css.rs`). The other passes run on what it produced, with the same sanitizer,
//! so they can't let through what it didn't: the images a reader resolves or blocks, the
//! inline parts embedded for a browser, and the colors moved for a dark page (`dark.rs`).
//!
//! Pure functions over strings: no IO, nothing async, nothing of mail protocols.
//!
//! Stored bodies keep what [`sanitize_html`] made of them when they were parsed. A change
//! to its output fails a fingerprint test in `catmail-proto`, which then decides whether
//! stored bodies are made again (`catmail_proto::mime::CONTENT_VERSION`).

mod css;
mod dark;
mod images;
mod sanitize;
mod tokens;

pub use dark::DarkPalette;
pub use images::{
    Image, MAX_REMOTE_IMAGES, Served, ServedImage, cid_image_ids, embed_inline_images,
    remote_image_url, serve_images, sniff_image,
};
pub use sanitize::sanitize_html;
