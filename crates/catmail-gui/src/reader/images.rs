//! The images an HTML body may show: its inline parts, never anything remote.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use catmail_core::{Attachment, Message};
use gpui_kit::{Image, ImageCacheError, ImageFormat, ImageSource, SharedUri};

use crate::html;

/// The inline parts the HTML can show as images, with their formats.
pub(super) fn inline_images(message: &Message) -> Vec<(Attachment, ImageFormat)> {
    let Some(body) = &message.body else {
        return Vec::new();
    };
    body.attachments
        .iter()
        .filter(|a| a.inline && a.content_id.is_some())
        .filter_map(|a| Some((a.clone(), raster_format(&a.mime_type)?)))
        .collect()
}

/// The image formats the pane renders. Not SVG: GPUI hands SVG to usvg with its default
/// options, whose resolver reads nested `<image href>` paths from the local disk, out of
/// `image_source`'s reach, so a message could show local files or read `/dev/zero`.
pub(super) fn raster_format(mime_type: &str) -> Option<ImageFormat> {
    ImageFormat::from_mime_type(mime_type).filter(|f| !matches!(f, ImageFormat::Svg))
}

/// The images of one open message, as the text view asks for them.
#[derive(Default)]
pub(super) struct Images {
    /// Decoded inline parts, by Content-ID.
    pub(super) inline: HashMap<String, Arc<Image>>,
    /// Every URL, resolved once: the inline part or decoded `data:` image, or `None` for
    /// one that isn't shown. The text view asks for every image on every frame.
    pub(super) by_url: Mutex<HashMap<String, Option<Arc<Image>>>>,
}

impl Images {
    /// The image at `url`, if the reader shows it ([`html::Carried`]): resolved once, as
    /// the text view asks for every image on every frame.
    fn image(&self, url: &str) -> Option<Arc<Image>> {
        let mut by_url = self.by_url.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(image) = by_url.get(url) {
            return image.clone();
        }
        let image = html::Carried::of(url).and_then(|carried| match carried {
            html::Carried::Inline(content_id) => self.inline.get(&content_id).cloned(),
            html::Carried::Data(data) => {
                let (mime_type, bytes) = html::decode(&data)?;
                let format = raster_format(&mime_type)?;
                Some(Arc::new(Image::from_bytes(format, bytes)))
            }
        });
        by_url.insert(url.to_owned(), image.clone());
        image
    }

    /// The image at `url` for the text view.
    pub(super) fn source(&self, url: &SharedUri) -> ImageSource {
        match self.image(url) {
            Some(image) => ImageSource::Image(image),
            None => not_shown(),
        }
    }

    /// The content of the image at `url` for a page's engine. Not SVG, even mislabelled:
    /// the engines decode with the `image` crate, which has no SVG decoder.
    pub(super) fn bytes(&self, url: &str) -> Option<Vec<u8>> {
        Some(self.image(url)?.bytes().to_vec())
    }
}

/// An image source that fails at once, so nothing is fetched and the space stays empty.
pub(super) fn not_shown() -> ImageSource {
    ImageSource::Custom(Arc::new(|_, _| {
        Some(Err(ImageCacheError::Other(Arc::new(anyhow!(
            "image not shown"
        )))))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_never_rendered() {
        assert!(raster_format("image/svg+xml").is_none());
        assert!(matches!(raster_format("image/png"), Some(ImageFormat::Png)));
        assert!(matches!(
            raster_format("image/jpeg"),
            Some(ImageFormat::Jpeg)
        ));

        let images = Images::default();
        let svg = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%3E\
                   %3Cimage%20href='/dev/zero'/%3E%3C/svg%3E";
        assert!(matches!(images.source(&svg.into()), ImageSource::Custom(_)));
        let png = "data:image/png;base64,iVBORw0KGgo=";
        assert!(matches!(images.source(&png.into()), ImageSource::Image(_)));
    }
}
