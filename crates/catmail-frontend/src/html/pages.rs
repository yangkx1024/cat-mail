//! The pages the reader's one web view shows, one after another, and their images: the
//! open message's inline parts as they load, and its remote images, cached or asked for.
//! A page refers to them by URLs of the reader's own scheme ([`IMAGE_SCHEME`]), and the
//! web view asks the reader for each, answered once the image is here (where the web
//! view needs it, a few at a time: [`pacer`](super::pacer)). A page shown never loads
//! again for them (it would lose its place), and its images are never copied into a
//! script or a `data:` URL: the web view reads the bytes as they are.

use std::collections::HashMap;

use catmail_core::html::{Image, RemoteImage, Served, ServedImage};

use super::pacer::Pacer;
use super::page::{Colors, blank_page, body_page};

/// The scheme of the URLs the page's images have: `catmail-image:<page>/<path>`, the
/// number of the page that refers to them, then [`ServedImage::path`]. The path stays
/// opaque (no `//`), as `serve_images` needs.
pub const IMAGE_SCHEME: &str = "catmail-image";

/// The base URL of page `number`'s images.
pub(super) fn image_base(number: u64) -> String {
    format!("{IMAGE_SCHEME}:{number}/")
}

/// The page that asks for `url`, and what it asks for; `None` for a URL the reader didn't
/// write.
fn image_of(url: &str) -> Option<(u64, ServedImage)> {
    let rest = url.strip_prefix(IMAGE_SCHEME)?.strip_prefix(':')?;
    let (page, path) = rest.split_once('/')?;
    Some((page.parse().ok()?, ServedImage::from_path(path)?))
}

/// The pages made for the web view, numbered (what a page says and its images' URLs carry
/// the number, so what an earlier one says late is told apart), the open message's
/// images, and the requests of the page shown. A request (`R`, the web view's) is
/// answered by returning it with its image; one dropped is never answered.
pub struct BodyPages<R> {
    /// Its inline parts loaded so far, by Content-ID: in its page, and in the browser's.
    inline: HashMap<String, Image>,
    /// The remote images shown (fetched, or from the cache), by URL.
    remote: HashMap<String, Image>,
    /// The number of the page shown, made last.
    shown: u64,
    /// The requests of the page shown waiting for an image that hasn't come yet: one for a
    /// remote image waits until the user asks for those of the message.
    waiting: Vec<(ServedImage, R)>,
    /// Those whose image is here, sent as the page takes them.
    pacer: Pacer<ServedImage, R>,
    /// Those of the page the shown one replaced, unanswered: it stays on screen until the
    /// new one paints, and a request failed would show a broken image there. They go once
    /// the new page asks for an image (the old one has gone by then).
    replaced: Vec<R>,
}

impl<R> BodyPages<R> {
    /// With at most `in_flight` bytes of images on their way to the page at once, or no
    /// limit ([`pacer`](super::pacer)).
    pub fn new(in_flight: Option<usize>) -> Self {
        Self {
            inline: HashMap::new(),
            remote: HashMap::new(),
            shown: 0,
            waiting: Vec::new(),
            pacer: Pacer::new(in_flight),
            replaced: Vec::new(),
        }
    }

    /// Whether what page `number` says is the page shown's.
    pub fn is_shown(&self, number: u64) -> bool {
        self.shown == number
    }

    /// Another message is opened: the images of the one before go. Its page stays shown
    /// until the next one's paints, so what it waits for keeps waiting, unanswered.
    pub fn forget_message(&mut self) {
        self.inline.clear();
        self.remote.clear();
    }

    /// The page of the open message's body (its sanitized HTML, or the plain text when it
    /// has none), shown from now on, with the remote images it refers to.
    pub fn page(&mut self, html: Option<&str>, text: Option<&str>, colors: &Colors) -> Served {
        let number = self.next();
        body_page(number, html, text, colors)
    }

    /// A page with nothing in it, shown from now on while no message is: the web view's
    /// web process is running when one opens.
    pub fn blank(&mut self, colors: &Colors) -> String {
        let number = self.next();
        blank_page(number, colors)
    }

    /// The number of a page about to be made, which is the one shown from now on.
    fn next(&mut self) -> u64 {
        self.shown += 1;
        let waiting = std::mem::take(&mut self.waiting)
            .into_iter()
            .map(|(_, r)| r);
        self.replaced = waiting.chain(self.pacer.clear()).collect();
        self.shown
    }

    /// The image `url` of the page shown, or `None` for one of another page or a URL the
    /// reader didn't write.
    fn shown_image(&self, url: &str) -> Option<ServedImage> {
        image_of(url)
            .filter(|(page, _)| *page == self.shown)
            .map(|(_, image)| image)
    }

    /// The page asks for its image `url`: answered once the image is here, in its turn. A
    /// request of another page, or for a URL the reader didn't write, is dropped.
    #[must_use]
    pub fn request(&mut self, url: &str, request: R) -> Vec<(R, Image)> {
        let Some(image) = self.shown_image(url) else {
            return Vec::new();
        };
        self.replaced.clear();
        self.waiting.push((image, request));
        self.arrived()
    }

    /// The page says its image `url` has come (or failed): what is next goes.
    #[must_use]
    pub fn loaded(&mut self, url: &str) -> Vec<(R, Image)> {
        match self.shown_image(url) {
            Some(image) => self.pacer.loaded(&image),
            None => Vec::new(),
        }
    }

    /// How far the images have come ([`unstick`](Self::unstick)).
    pub fn progress(&self) -> u64 {
        self.pacer.progress()
    }

    /// Whether images that are here wait for those on their way: never with no limit.
    pub fn holds_back(&self) -> bool {
        self.pacer.holds_back()
    }

    /// If no image has come since `progress` while some are held back, sends the rest
    /// anyway: a page that never says one came would get no more.
    #[must_use]
    pub fn unstick(&mut self, progress: u64) -> Vec<(R, Image)> {
        self.pacer.unstick(progress)
    }

    fn image(&self, image: &ServedImage) -> Option<&Image> {
        match image {
            ServedImage::Inline(content_id) => self.inline.get(content_id),
            ServedImage::Remote(url) => self.remote.get(url),
        }
    }

    pub fn has_inline(&self, content_id: &str) -> bool {
        self.inline.contains_key(content_id)
    }

    /// The inline parts loaded, by Content-ID.
    pub fn inline(&self) -> &HashMap<String, Image> {
        &self.inline
    }

    /// An inline part of the open message has loaded: the requests to answer now.
    #[must_use]
    pub fn add_inline(&mut self, content_id: String, image: Image) -> Vec<(R, Image)> {
        self.inline.insert(content_id, image);
        self.arrived()
    }

    pub fn shows_remote(&self, url: &str) -> bool {
        self.remote.contains_key(url)
    }

    /// Remote images of the open message came (fetched, or from the cache), by URL: the
    /// requests to answer now.
    #[must_use]
    pub fn add_remote(&mut self, images: Vec<(String, RemoteImage)>) -> Vec<(R, Image)> {
        for (url, image) in images {
            let image = Image::new(&image.mime_type, image.bytes);
            self.remote.insert(url, image);
        }
        self.arrived()
    }

    /// The requests waiting for images that are here now go to the pacer, in the order
    /// they were made: those to answer now.
    fn arrived(&mut self) -> Vec<(R, Image)> {
        for (image, request) in std::mem::take(&mut self.waiting) {
            match self.image(&image).cloned() {
                Some(found) => self.pacer.queue(image, request, found),
                None => self.waiting.push((image, request)),
            }
        }
        self.pacer.send()
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
            selection: "#264F784D".into(),
        }
    }

    fn png(bytes: &[u8]) -> Image {
        Image::new("image/png", bytes)
    }

    fn remote(url: &str, bytes: &[u8]) -> (String, RemoteImage) {
        let image = RemoteImage {
            mime_type: "image/gif".into(),
            bytes: bytes.to_vec(),
        };
        (url.to_owned(), image)
    }

    /// The URLs of the images a page refers to, as written.
    fn image_urls(page: &Served) -> Vec<String> {
        page.html
            .split(" src=\"")
            .skip(1)
            .map(|rest| rest[..rest.find('"').unwrap()].to_owned())
            .collect()
    }

    /// The requests answered (named by the test) and the bytes they got.
    fn answered(answers: Vec<(&'static str, Image)>) -> Vec<(&'static str, Vec<u8>)> {
        answers
            .into_iter()
            .map(|(request, image)| (request, image.bytes().to_vec()))
            .collect()
    }

    const HTML: &str =
        r#"<img src="cid:logo@x"><img src="https://x/a.gif"><img src="CID:logo%40x">"#;

    #[test]
    fn the_page_refers_to_its_images_by_the_readers_urls() {
        let mut images = BodyPages::<()>::new(None);
        images.blank(&colors());
        let page = images.page(Some(HTML), None, &colors());
        assert!(images.is_shown(2));
        assert_eq!(
            image_urls(&page),
            [
                "catmail-image:2/cid/logo%40x",
                "catmail-image:2/remote/https%3A%2F%2Fx%2Fa.gif",
                "catmail-image:2/cid/logo%40x",
            ]
        );
        assert_eq!(page.remote_urls, ["https://x/a.gif"]);
        let csp = format!("img-src data: {IMAGE_SCHEME}:;");
        assert!(page.html.contains(&csp), "{}", page.html);
    }

    #[test]
    fn a_request_is_answered_once_its_image_is_here() {
        let mut images = BodyPages::new(None);
        let page = images.page(Some(HTML), None, &colors());
        let [logo, gif, logo_again] = <[String; 3]>::try_from(image_urls(&page)).unwrap();

        assert!(images.request(&logo, "logo").is_empty());
        assert!(images.request(&gif, "gif").is_empty());
        assert_eq!(
            answered(images.add_inline("logo@x".into(), png(b"hi"))),
            [("logo", b"hi".to_vec())]
        );
        // Here already.
        assert_eq!(
            answered(images.request(&logo_again, "logo again")),
            [("logo again", b"hi".to_vec())]
        );

        // A part the page doesn't show answers nothing; the remote image waits until it is
        // fetched (the user asked) or found cached.
        assert!(images.add_inline("other@x".into(), png(b"no")).is_empty());
        assert_eq!(
            answered(images.add_remote(vec![remote("https://x/a.gif", b"gif")])),
            [("gif", b"gif".to_vec())]
        );
        assert!(images.shows_remote("https://x/a.gif"));
    }

    #[test]
    fn a_page_made_again_gets_the_images_at_once() {
        let mut images = BodyPages::new(None);
        images.page(Some(HTML), None, &colors());
        assert!(images.add_inline("logo@x".into(), png(b"hi")).is_empty());
        assert!(
            images
                .add_remote(vec![remote("https://x/a.gif", b"gif")])
                .is_empty()
        );

        // In another appearance.
        let page = images.page(Some(HTML), None, &colors());
        for url in image_urls(&page) {
            assert_eq!(images.request(&url, ()).len(), 1, "{url}");
        }
    }

    #[test]
    fn only_the_page_shown_is_answered() {
        let mut images = BodyPages::new(None);
        let old = images.page(Some(HTML), None, &colors());
        let old = image_urls(&old);
        assert!(images.request(&old[0], "old").is_empty());

        images.forget_message();
        // Kept, unanswered, while the old page is still on screen.
        images.blank(&colors());
        assert_eq!(images.replaced, ["old"]);
        let new = images.page(Some(HTML), None, &colors());
        let new = image_urls(&new);
        assert!(images.request(&new[0], "new").is_empty());
        // The old page has gone; late, from it.
        assert!(images.replaced.is_empty());
        assert!(images.request(&old[2], "late").is_empty());
        assert_eq!(
            answered(images.add_inline("logo@x".into(), png(b"hi"))),
            [("new", b"hi".to_vec())]
        );
        assert!(!images.has_inline("other@x"));
        assert!(images.waiting.is_empty());
        assert!(!images.is_shown(1));
    }

    #[test]
    fn another_message_forgets_the_images_of_the_one_before() {
        let mut images = BodyPages::<()>::new(None);
        images.page(Some(HTML), None, &colors());
        assert!(images.add_inline("logo@x".into(), png(b"hi")).is_empty());
        assert!(
            images
                .add_remote(vec![remote("https://x/a.gif", b"gif")])
                .is_empty()
        );

        images.forget_message();
        assert!(!images.has_inline("logo@x"));
        assert!(images.inline().is_empty());
        assert!(!images.shows_remote("https://x/a.gif"));
        let page = images.page(Some(HTML), None, &colors());
        assert!(images.request(&image_urls(&page)[0], ()).is_empty());
    }

    #[test]
    fn urls_the_reader_didnt_write_are_dropped() {
        let mut images = BodyPages::new(None);
        images.page(Some(HTML), None, &colors());
        assert!(images.add_inline("logo@x".into(), png(b"hi")).is_empty());
        for url in [
            "catmail-image:1/cid/",
            "catmail-image:x/cid/logo%40x",
            "catmail-image:1/file/%2Fetc%2Fpasswd",
            "other:1/cid/logo%40x",
            "catmail-image:1",
        ] {
            assert!(images.request(url, ()).is_empty(), "{url}");
        }
        assert!(images.waiting.is_empty());
    }

    /// A page of `n` remote images, each `size` bytes and here, with a limit of `size` on
    /// their way: one at a time.
    fn photos(n: usize, size: usize) -> (BodyPages<usize>, Vec<String>) {
        let mut images = BodyPages::new(Some(size));
        let html: String = (0..n)
            .map(|i| format!(r#"<img src="https://x/{i}.jpg">"#))
            .collect();
        let page = images.page(Some(&html), None, &colors());
        let fetched = (0..n).map(|i| remote(&format!("https://x/{i}.jpg"), &vec![0; size]));
        assert!(images.add_remote(fetched.collect()).is_empty());
        (images, image_urls(&page))
    }

    fn requests<R>(answers: Vec<(R, Image)>) -> Vec<R> {
        answers.into_iter().map(|(request, _)| request).collect()
    }

    #[test]
    fn images_go_in_the_order_asked_as_the_page_takes_them() {
        let (mut images, urls) = photos(3, 100);
        let sent: Vec<usize> = (0..3)
            .flat_map(|i| requests(images.request(&urls[i], i)))
            .collect();
        assert_eq!(sent, [0]);
        assert!(images.holds_back());

        // Of another page, or not the reader's: nothing goes.
        assert!(images.loaded(&urls[0].replace(":1/", ":0/")).is_empty());
        assert!(images.loaded("data:image/png;base64,aGk=").is_empty());
        assert_eq!(requests(images.loaded(&urls[0])), [1]);
        assert_eq!(requests(images.loaded(&urls[1])), [2]);

        let progress = images.progress();
        assert!(images.unstick(progress).is_empty(), "nothing held back");
    }

    #[test]
    fn a_new_page_gives_back_what_the_last_was_still_to_get() {
        let (mut images, urls) = photos(2, 100);
        assert_eq!(requests(images.request(&urls[0], 0)), [0]);
        assert!(images.request(&urls[1], 1).is_empty());

        // The same message in another appearance: the old page keeps its request, and the
        // new one starts with nothing on its way.
        let page = images.page(Some(r#"<img src="https://x/1.jpg">"#), None, &colors());
        assert_eq!(images.replaced, [1]);
        assert!(!images.holds_back());
        assert_eq!(requests(images.request(&image_urls(&page)[0], 7)), [7]);
    }
}
