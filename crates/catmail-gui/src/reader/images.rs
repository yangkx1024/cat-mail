//! The images an HTML body may show: its inline parts, and the remote ones only once the
//! user asked for them.

use std::collections::HashMap;

use catmail_core::html::{RemoteImage, remote_image_url};
use catmail_core::{Attachment, Message};
use catmail_render::Bytes;
use gpui_kit::ImageFormat;

use crate::html;

/// The inline parts the HTML can show as images; none without HTML.
pub(super) fn inline_images(message: &Message) -> Vec<Attachment> {
    let Some(body) = message.body.as_ref().filter(|body| body.html.is_some()) else {
        return Vec::new();
    };
    body.attachments
        .iter()
        .filter(|a| a.inline && a.content_id.is_some() && is_raster(&a.mime_type))
        .cloned()
        .collect()
}

/// Whether the pane renders images of this type. Not SVG: usvg's default resolver reads
/// nested `<image href>` paths from the local disk, out of the resolver's reach, so a
/// message could show local files or read `/dev/zero`; and the page decodes with the
/// `image` crate, which has no SVG decoder.
pub(super) fn is_raster(mime_type: &str) -> bool {
    ImageFormat::from_mime_type(mime_type).is_some_and(|f| !matches!(f, ImageFormat::Svg))
}

/// An image's type and content, shared with the pages it is handed to.
#[derive(Clone)]
pub(super) struct ImageData {
    pub(super) mime_type: String,
    pub(super) bytes: Bytes,
}

/// The images of one open message, as the page asks for them. Images that come later make
/// a new one ([`with_inline`](Self::with_inline), [`with_remote`](Self::with_remote)).
#[derive(Default)]
pub(super) struct Images {
    /// Inline parts, by Content-ID.
    inline: HashMap<String, ImageData>,
    /// Remote images shown (asked for, or cached), by [`remote_image_url`].
    remote: HashMap<String, ImageData>,
}

impl Images {
    /// These, with the inline parts downloaded.
    pub(super) fn with_inline(&self, inline: HashMap<String, ImageData>) -> Self {
        Self {
            inline,
            remote: self.remote.clone(),
        }
    }

    /// These, with remote images that came. Those the pane can't show (SVG, formats it
    /// doesn't know) stay blank.
    pub(super) fn with_remote(&self, fetched: Vec<(String, RemoteImage)>) -> Self {
        let mut remote = self.remote.clone();
        remote.extend(fetched.into_iter().filter_map(|(url, image)| {
            is_raster(&image.mime_type).then(|| {
                let image = ImageData {
                    mime_type: image.mime_type,
                    bytes: image.bytes.into(),
                };
                (url, image)
            })
        }));
        Self {
            inline: self.inline.clone(),
            remote,
        }
    }

    /// The inline parts downloaded, by Content-ID.
    pub(super) fn inline(&self) -> impl Iterator<Item = (&String, &ImageData)> {
        self.inline.iter()
    }

    /// The content of the image at `url`, if the reader shows it ([`html::Carried`], or a
    /// remote image fetched), sharing the stored bytes: a document asks again when it opens
    /// again (the theme, the zoom changed).
    pub(super) fn bytes(&self, url: &str) -> Option<Bytes> {
        let image = match html::Carried::of(url) {
            Some(html::Carried::Inline(content_id)) => self.inline.get(&content_id),
            Some(html::Carried::Data(data)) => {
                let (mime_type, bytes) = html::decode(&data)?;
                return is_raster(&mime_type).then(|| bytes.into());
            }
            None => self.remote.get(&remote_image_url(url)?),
        };
        image.map(|image| image.bytes.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_never_rendered() {
        assert!(!is_raster("image/svg+xml"));
        assert!(is_raster("image/png"));
        assert!(is_raster("image/jpeg"));

        let images = Images::default();
        let svg = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%3E\
                   %3Cimage%20href='/dev/zero'/%3E%3C/svg%3E";
        assert!(images.bytes(svg).is_none());
        let png = "data:image/png;base64,iVBORw0KGgo=";
        assert!(images.bytes(png).is_some());
    }

    #[test]
    fn remote_images_show_once_fetched() {
        let url = "https://x.example/a b.png";
        let blocked = Images::default();
        assert!(blocked.bytes(url).is_none());

        let png = b"\x89PNG\r\n\x1a\n".to_vec();
        let fetched = |mime_type: &str| RemoteImage {
            mime_type: mime_type.into(),
            bytes: png.clone(),
        };
        let images = blocked.with_remote(vec![
            ("https://x.example/a%20b.png".into(), fetched("image/png")),
            ("https://x.example/b.svg".into(), fetched("image/svg+xml")),
        ]);
        assert_eq!(images.remote.len(), 1, "no SVG");
        // Unparsed, and as Blitz asks for it (parsed).
        let bytes = images.bytes(url).unwrap();
        assert_eq!(bytes, png);
        let parsed = images.bytes("https://x.example/a%20b.png").unwrap();
        assert_eq!(bytes.as_ptr(), parsed.as_ptr(), "shared, not copied");

        // More images keep those that came: the same content.
        let more = images.with_remote(vec![(
            "https://x.example/c.png".into(),
            fetched("image/png"),
        )]);
        assert_eq!(more.bytes(url).unwrap().as_ptr(), bytes.as_ptr());
        assert!(more.bytes("https://x.example/c.png").is_some());
    }
}
