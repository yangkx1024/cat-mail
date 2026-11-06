//! The images the reader's page shows that come after it: the open message's inline parts
//! as they load, and its remote images, cached or asked for. A page shown doesn't load
//! again for them (it would lose its place); they are sent to it ([`image_message`]),
//! once it has loaded.
//!
//! One at a time: the next goes once the page says it has taken the last
//! ([`FromPage::Taken`](super::FromPage::Taken)). An image is a `data:` URL in a script
//! the web view runs, megabytes each for a newsletter's; the web process runs those in
//! order, so with all of them sent at once the next message's page waits behind them,
//! and turning each into a script holds the main loop as long. Sent one by one, what is
//! left is dropped when another message opens.

use std::collections::{HashMap, VecDeque};

use catmail_core::html::{InlineImage, InlineUrl, RemoteImage, data_url};

use super::page::{Colors, body_page, image_message};

/// The open message's images, and which of them the page shown still needs. Each method
/// that changes it returns the message to post to the page, if one is due.
#[derive(Default)]
pub struct PageImages {
    /// Its inline parts loaded so far, by Content-ID: in its page as made again, sent to
    /// the one shown as they come, and in the browser's.
    inline: HashMap<String, InlineImage>,
    /// The remote images shown, by URL: sent again when the page loads again (the
    /// appearance changed). Made `data:` URLs as they are sent: they are kept for as long
    /// as the message is open, and are larger so.
    remote: Vec<(String, RemoteImage)>,
    /// The `cid:` images of the page shown that it isn't made with.
    inline_urls: Vec<InlineUrl>,
    /// The page shown has loaded: images can be sent to it.
    ready: bool,
    /// What the page shown is still to be sent, in order.
    queue: VecDeque<Pending>,
    /// An image was sent that the page hasn't said it has taken.
    sending: bool,
}

enum Pending {
    Remote(String),
    Inline(InlineUrl),
}

impl PageImages {
    /// Another message is opened: those of the one before go, and so does what was still
    /// to be sent of them.
    pub fn forget_message(&mut self) {
        self.inline.clear();
        self.remote.clear();
        self.queue.clear();
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
        self.queue.clear();
        self.sending = false;
        page.html
    }

    /// The page shown has loaded: it gets the remote images shown so far, and the inline
    /// parts loaded that it isn't made with (those that loaded while it did).
    pub fn ready(&mut self) -> Option<String> {
        self.ready = true;
        let remote = self
            .remote
            .iter()
            .map(|(url, _)| Pending::Remote(url.clone()));
        let inline = (self.inline_urls.iter())
            .filter(|u| self.inline.contains_key(&u.content_id))
            .map(|u| Pending::Inline(u.clone()));
        self.queue.extend(remote.chain(inline));
        self.next()
    }

    /// The page shown has taken the image sent last.
    pub fn taken(&mut self) -> Option<String> {
        self.sending = false;
        self.next()
    }

    pub fn has_inline(&self, content_id: &str) -> bool {
        self.inline.contains_key(content_id)
    }

    /// The inline parts loaded, by Content-ID.
    pub fn inline(&self) -> &HashMap<String, InlineImage> {
        &self.inline
    }

    /// An inline part of the open message has loaded.
    pub fn add_inline(&mut self, content_id: String, image: InlineImage) -> Option<String> {
        if self.ready {
            let urls = self
                .inline_urls
                .iter()
                .filter(|u| u.content_id == content_id);
            self.queue.extend(urls.cloned().map(Pending::Inline));
        }
        self.inline.insert(content_id, image);
        self.next()
    }

    pub fn shows_remote(&self, url: &str) -> bool {
        self.remote.iter().any(|(shown, _)| shown == url)
    }

    /// Remote images of the open message came (fetched, or from the cache), by URL.
    pub fn add_remote(&mut self, images: Vec<(String, RemoteImage)>) -> Option<String> {
        if self.ready {
            let urls = images.iter().map(|(url, _)| Pending::Remote(url.clone()));
            self.queue.extend(urls);
        }
        self.remote.extend(images);
        self.next()
    }

    /// The message of the next image due, unless the page is still to take the last.
    fn next(&mut self) -> Option<String> {
        if self.sending || !self.ready {
            return None;
        }
        while let Some(pending) = self.queue.pop_front() {
            let message = match pending {
                Pending::Remote(url) => {
                    self.remote
                        .iter()
                        .find(|(shown, _)| *shown == url)
                        .map(|(_, image)| {
                            image_message(&url, &data_url(&image.mime_type, &image.bytes))
                        })
                }
                Pending::Inline(u) => (self.inline.get(&u.content_id))
                    .map(|image| image_message(&u.url, image.data_url())),
            };
            if message.is_some() {
                self.sending = true;
                return message;
            }
        }
        None
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

    /// What the page is sent, taking each image as it comes, until nothing is left.
    fn drain(images: &mut PageImages, first: Option<String>) -> Vec<String> {
        let mut sent = Vec::from_iter(first);
        while let Some(message) = images.taken() {
            sent.push(message);
        }
        sent
    }

    const HTML: &str = r#"<img src="cid:logo@x"><img src="cid:chart@x"><img src="CID:logo%40x">"#;

    #[test]
    fn what_comes_before_the_page_has_loaded_waits_for_it() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        assert_eq!(images.add_inline("logo@x".into(), png(b"hi")), None);
        assert_eq!(images.add_remote(vec![remote("https://x/a.gif")]), None);

        let first = images.ready();
        assert_eq!(
            drain(&mut images, first),
            [
                "i:https://x/a.gif\ndata:image/gif;base64,Z2lm",
                "i:cid:logo@x\ndata:image/png;base64,aGk=",
                "i:cid:logo%40x\ndata:image/png;base64,aGk=",
            ]
        );
    }

    #[test]
    fn one_image_at_a_time() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        assert_eq!(images.ready(), None);

        assert_eq!(
            images.add_inline("chart@x".into(), png(b"hi")).as_deref(),
            Some("i:cid:chart@x\ndata:image/png;base64,aGk=")
        );
        // Until the page has taken that one.
        assert_eq!(images.add_remote(vec![remote("https://x/a.gif")]), None);
        assert_eq!(
            images.taken().as_deref(),
            Some("i:https://x/a.gif\ndata:image/gif;base64,Z2lm")
        );
        assert_eq!(images.taken(), None);
        // A part no image of the page shows.
        assert_eq!(images.add_inline("other@x".into(), png(b"hi")), None);
        assert!(images.shows_remote("https://x/a.gif"));
    }

    #[test]
    fn a_page_made_again_is_made_with_the_parts_loaded() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        images.ready();
        images.add_inline("logo@x".into(), png(b"hi"));
        images.add_remote(vec![remote("https://x/a.gif")]);

        // In another appearance, before the page before took its images: the part is
        // embedded, not sent again, and what that page was still to get goes to this one.
        let page = images.page(2, Some(HTML), None, &colors());
        assert!(page.contains("data:image/png;base64,aGk="), "{page}");
        assert!(page.contains("cid:chart@x"), "{page}");
        assert_eq!(images.add_inline("chart@x".into(), png(b"ok")), None);
        let first = images.ready();
        assert_eq!(
            drain(&mut images, first),
            [
                "i:https://x/a.gif\ndata:image/gif;base64,Z2lm",
                "i:cid:chart@x\ndata:image/png;base64,b2s=",
            ]
        );
    }

    #[test]
    fn another_message_stops_what_is_left() {
        let mut images = PageImages::default();
        images.page(1, Some(HTML), None, &colors());
        images.add_inline("logo@x".into(), png(b"hi"));
        images.add_remote(vec![remote("https://x/a.gif"), remote("https://x/b.gif")]);
        assert!(images.ready().is_some());

        images.forget_message();
        assert_eq!(images.taken(), None);
        assert!(!images.has_inline("logo@x"));
        assert!(images.inline().is_empty());
        assert!(!images.shows_remote("https://x/a.gif"));
        images.page(2, Some(HTML), None, &colors());
        assert_eq!(images.ready(), None);
    }
}
