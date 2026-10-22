//! The images an HTML body may show: its inline parts, and the remote ones only once the
//! user asked for them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use catmail_core::html::{RemoteImage, remote_image_url};
use catmail_core::{Attachment, Message};
use catmail_render::Bytes;
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

/// The images of one open message, as the text view asks for them. Images that come later
/// make a new one ([`with_inline`](Self::with_inline), [`with_remote`](Self::with_remote)):
/// the views see the change by the `Arc` they get.
#[derive(Default)]
pub(super) struct Images {
    /// Decoded inline parts, by Content-ID.
    inline: HashMap<String, Arc<Image>>,
    /// Remote images shown (asked for, or cached), by [`remote_image_url`].
    remote: HashMap<String, Arc<Image>>,
    /// Every URL, resolved once: the inline part, decoded `data:` image or fetched remote
    /// image, or `None` for one that isn't shown. The text view asks for every image on
    /// every frame.
    by_url: Mutex<HashMap<String, Option<Arc<Image>>>>,
}

impl Images {
    /// These, with the inline parts downloaded.
    pub(super) fn with_inline(&self, inline: HashMap<String, Arc<Image>>) -> Self {
        Self {
            inline,
            remote: self.remote.clone(),
            by_url: Mutex::new(self.resolved()),
        }
    }

    /// These, with remote images that came. Those the pane can't show (SVG, formats GPUI
    /// doesn't know) stay blank.
    pub(super) fn with_remote(&self, fetched: Vec<(String, RemoteImage)>) -> Self {
        let mut remote = self.remote.clone();
        remote.extend(fetched.into_iter().filter_map(|(url, image)| {
            let format = raster_format(&image.mime_type)?;
            Some((url, Arc::new(Image::from_bytes(format, image.bytes))))
        }));
        Self {
            inline: self.inline.clone(),
            remote,
            by_url: Mutex::new(self.resolved()),
        }
    }

    /// The inline parts downloaded, by Content-ID.
    pub(super) fn inline(&self) -> impl Iterator<Item = (&String, &Arc<Image>)> {
        self.inline.iter()
    }

    /// The URLs resolved to an image, which more images don't change: `data:` images
    /// aren't decoded again.
    fn resolved(&self) -> HashMap<String, Option<Arc<Image>>> {
        let by_url = self.by_url.lock().unwrap_or_else(|e| e.into_inner());
        (by_url.iter())
            .filter(|(_, image)| image.is_some())
            .map(|(url, image)| (url.clone(), image.clone()))
            .collect()
    }

    /// The image at `url`, if the reader shows it ([`html::Carried`], or a remote image
    /// fetched): resolved once, as the text view asks for every image on every frame.
    fn image(&self, url: &str) -> Option<Arc<Image>> {
        let mut by_url = self.by_url.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(image) = by_url.get(url) {
            return image.clone();
        }
        let image = match html::Carried::of(url) {
            Some(html::Carried::Inline(content_id)) => self.inline.get(&content_id).cloned(),
            Some(html::Carried::Data(data)) => {
                html::decode(&data).and_then(|(mime_type, bytes)| {
                    let format = raster_format(&mime_type)?;
                    Some(Arc::new(Image::from_bytes(format, bytes)))
                })
            }
            None => remote_image_url(url).and_then(|url| self.remote.get(&url).cloned()),
        };
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

    /// The content of the image at `url` for a page's engine, sharing the image's bytes:
    /// a document asks again when it opens again (the theme, the zoom changed). Not
    /// SVG, even mislabelled: the engines decode with the `image` crate, which has no SVG
    /// decoder.
    pub(super) fn bytes(&self, url: &str) -> Option<Bytes> {
        Some(Bytes::from_owner(SharedImage(self.image(url)?)))
    }
}

/// An image's content, as `Bytes` owning the image.
struct SharedImage(Arc<Image>);

impl AsRef<[u8]> for SharedImage {
    fn as_ref(&self) -> &[u8] {
        self.0.bytes()
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

    #[test]
    fn remote_images_show_once_fetched() {
        let url = "https://x.example/a b.png";
        let blocked = Images::default();
        assert!(matches!(
            blocked.source(&url.into()),
            ImageSource::Custom(_)
        ));
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
        // As the text view (unparsed) and Blitz (parsed) ask for it.
        assert!(matches!(images.source(&url.into()), ImageSource::Image(_)));
        let bytes = images.bytes("https://x.example/a%20b.png").unwrap();
        assert_eq!(bytes, png);
        let image = images.image(url).unwrap();
        assert_eq!(bytes.as_ptr(), image.bytes().as_ptr(), "shared, not copied");

        // More images keep what was resolved: the same image, not decoded again.
        let png_url = "data:image/png;base64,iVBORw0KGgo=";
        let data = images.image(png_url).unwrap();
        let more = images.with_remote(vec![(
            "https://x.example/c.png".into(),
            fetched("image/png"),
        )]);
        assert!(Arc::ptr_eq(&more.image(png_url).unwrap(), &data));
        assert!(more.image("https://x.example/c.png").is_some());
    }
}
