//! The images the reader's page shows that come after it: the open message's inline parts
//! as they load, and its remote images, cached or asked for. A page shown doesn't load
//! again for them (it would lose its place); they are sent to it ([`image_message`]),
//! once it has loaded.

use std::collections::HashMap;

use catmail_core::html::{InlineImage, InlineUrl, RemoteImage, data_url};

use super::page::{Colors, body_page, image_message};

/// The open message's images, and which of them the page shown still needs. Each method
/// that changes it returns the messages to post to the page.
#[derive(Default)]
pub struct PageImages {
    /// Its inline parts loaded so far, by Content-ID: in its page as made again, sent to
    /// the one shown as they come, and in the browser's.
    inline: HashMap<String, InlineImage>,
    /// The remote images shown, as `data:` URLs by URL: sent again when the page loads
    /// again (the appearance changed).
    remote: Vec<(String, String)>,
    /// The `cid:` images of the page shown that it isn't made with.
    inline_urls: Vec<InlineUrl>,
    /// The page shown has loaded: images can be sent to it.
    ready: bool,
}

impl PageImages {
    /// Another message is opened: those of the one before go.
    pub fn forget_message(&mut self) {
        self.inline.clear();
        self.remote.clear();
    }

    /// The page of the open message's body (its sanitized HTML, or the plain text when it
    /// has none), shown from now on: the inline parts loaded so far are in it, the others
    /// go in as they load. `number` is in what the page says
    /// ([`FromPage`](super::FromPage)).
    pub fn page(
        &mut self,
        number: u64,
        html: Option<&str>,
        text: Option<&str>,
        colors: &Colors,
    ) -> String {
        let page = body_page(number, html, text, self.inline.clone(), colors);
        self.inline_urls = page.inline_urls;
        self.ready = false;
        page.html
    }

    /// The page shown has loaded: it gets the remote images shown so far, and the inline
    /// parts loaded that it isn't made with (those that loaded while it did).
    pub fn ready(&mut self) -> Vec<String> {
        self.ready = true;
        let remote = self
            .remote
            .iter()
            .map(|(url, data)| image_message(url, data));
        let inline = self.inline_urls.iter().filter_map(|u| {
            let image = self.inline.get(&u.content_id)?;
            Some(image_message(&u.url, image.data_url()))
        });
        remote.chain(inline).collect()
    }

    pub fn has_inline(&self, content_id: &str) -> bool {
        self.inline.contains_key(content_id)
    }

    /// The inline parts loaded, by Content-ID.
    pub fn inline(&self) -> &HashMap<String, InlineImage> {
        &self.inline
    }

    /// An inline part of the open message has loaded.
    pub fn add_inline(&mut self, content_id: String, image: InlineImage) -> Vec<String> {
        let messages = if self.ready {
            self.inline_urls
                .iter()
                .filter(|u| u.content_id == content_id)
                .map(|u| image_message(&u.url, image.data_url()))
                .collect()
        } else {
            Vec::new()
        };
        self.inline.insert(content_id, image);
        messages
    }

    pub fn shows_remote(&self, url: &str) -> bool {
        self.remote.iter().any(|(shown, _)| shown == url)
    }

    /// Remote images of the open message came (fetched, or from the cache), by URL.
    pub fn add_remote(&mut self, images: Vec<(String, RemoteImage)>) -> Vec<String> {
        let images = images
            .into_iter()
            .map(|(url, image)| (url, data_url(&image.mime_type, &image.bytes)));
        let start = self.remote.len();
        self.remote.extend(images);
        if !self.ready {
            return Vec::new();
        }
        self.remote[start..]
            .iter()
            .map(|(url, data)| image_message(url, data))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        Colors {
            foreground: "#111".into(),
            background: "#fff".into(),
            link: "#06c".into(),
        }
    }

    fn png(bytes: &[u8]) -> InlineImage {
        InlineImage::new("image/png", bytes)
    }

    fn remote(url: &str) -> (String, RemoteImage) {
        let image = RemoteImage {
            mime_type: "image/gif".into(),
            bytes: b"gif".to_vec(),
        };
        (url.to_owned(), image)
    }

    const HTML: &str = r#"<img src="cid:logo@x"><img src="cid:chart@x"><img src="CID:logo%40x">"#;

    #[test]
    fn what_comes_before_the_page_has_loaded_waits_for_it() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        assert!(images.add_inline("logo@x".into(), png(b"hi")).is_empty());
        assert!(
            images
                .add_remote(vec![remote("https://x/a.gif")])
                .is_empty()
        );

        assert_eq!(
            images.ready(),
            [
                "i:https://x/a.gif\ndata:image/gif;base64,Z2lm",
                "i:cid:logo@x\ndata:image/png;base64,aGk=",
                "i:cid:logo%40x\ndata:image/png;base64,aGk=",
            ]
        );
    }

    #[test]
    fn what_comes_after_goes_to_the_page_at_once() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        assert!(images.ready().is_empty());

        assert_eq!(
            images.add_inline("chart@x".into(), png(b"hi")),
            ["i:cid:chart@x\ndata:image/png;base64,aGk="]
        );
        // A part no image of the page shows.
        assert!(images.add_inline("other@x".into(), png(b"hi")).is_empty());
        assert_eq!(
            images.add_remote(vec![remote("https://x/a.gif")]),
            ["i:https://x/a.gif\ndata:image/gif;base64,Z2lm"]
        );
        assert!(images.shows_remote("https://x/a.gif"));
    }

    #[test]
    fn a_page_made_again_is_made_with_the_parts_loaded() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        images.ready();
        images.add_inline("logo@x".into(), png(b"hi"));
        images.add_remote(vec![remote("https://x/a.gif")]);

        // In another appearance: the part is embedded, not sent again.
        let page = images.page(2, Some(HTML), None, &colors());
        assert!(page.contains("data:image/png;base64,aGk="), "{page}");
        assert!(page.contains("cid:chart@x"), "{page}");
        assert!(images.add_inline("chart@x".into(), png(b"ok")).is_empty());
        assert_eq!(
            images.ready(),
            [
                "i:https://x/a.gif\ndata:image/gif;base64,Z2lm",
                "i:cid:chart@x\ndata:image/png;base64,b2s=",
            ]
        );
    }

    #[test]
    fn another_message_starts_with_none() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        images.add_inline("logo@x".into(), png(b"hi"));
        images.add_remote(vec![remote("https://x/a.gif")]);

        images.forget_message();
        assert!(!images.has_inline("logo@x"));
        assert!(images.inline().is_empty());
        assert!(!images.shows_remote("https://x/a.gif"));
        images.page(2, Some(HTML), None, &colors());
        assert!(images.ready().is_empty());
    }
}
