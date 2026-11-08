//! The images a web view is sent, a few at a time where it needs it.
//!
//! WebKitGTK reads a URL scheme's answer 8 KiB at a time, each piece a message to the web
//! process, and an image the page shows is drawn again for each piece. Dozens of photos
//! of several MB each answered at once keep the web process busy for a second or more,
//! and the next message's page waits behind them; over HTTP (larger pieces) it doesn't.
//! With a limit, at most that many bytes are on their way, in the order the page asked,
//! and the next go once the page says one has come: the first image shows after its own
//! bytes, and what another page waits behind is about one image. A web view that takes
//! an answer whole (WebKit's `WKURLSchemeHandler`) needs no limit.
//!
//! A stopgap: once WebKitGTK reads larger pieces
//! (<https://bugs.webkit.org/show_bug.cgi?id=330624>), this goes, and every image is
//! answered as it comes. It stays in the app rather than the toolkit: what matters as
//! much as the limit is to stop sending a page's images the moment another replaces it
//! (one more photo sent then is decoded in full before the navigation goes on), and only
//! the reader knows which page that is ([`BodyPages`](super::BodyPages) clears this as it
//! makes the next).

use std::collections::VecDeque;

use catmail_core::html::Image;

/// Images ready for their requests (`R`), sent as the page takes them. Images are told
/// apart by a key (`K`), the one the page says has come by.
pub(super) struct Pacer<K, R> {
    /// The bytes on their way past which no other image is sent until one has come; one
    /// larger goes alone. `None`: every image goes as it comes.
    limit: Option<usize>,
    /// Ready, in the order they are to go.
    ready: VecDeque<(K, R, Image)>,
    /// Sent, and not said to have come, with their sizes.
    sending: Vec<(K, usize)>,
    /// Bumped as images come: one that stays still long while some are held back means the
    /// page has stopped saying so ([`unstick`](Self::unstick)).
    progress: u64,
}

impl<K: PartialEq, R> Pacer<K, R> {
    pub fn new(limit: Option<usize>) -> Self {
        Self {
            limit,
            ready: VecDeque::new(),
            sending: Vec::new(),
            progress: 0,
        }
    }

    /// `request`'s image is here; it goes in its turn ([`send`](Self::send)).
    pub fn queue(&mut self, key: K, request: R, image: Image) {
        self.ready.push_back((key, request, image));
    }

    /// The requests to answer now: while less than the limit is on its way.
    pub fn send(&mut self) -> Vec<(R, Image)> {
        let Some(limit) = self.limit else {
            return self.ready.drain(..).map(|(_, r, i)| (r, i)).collect();
        };
        let mut on_way: usize = self.sending.iter().map(|(_, size)| size).sum();
        let mut answers = Vec::new();
        while on_way < limit {
            let Some((key, request, image)) = self.ready.pop_front() else {
                break;
            };
            let size = image.bytes().len();
            on_way += size;
            self.sending.push((key, size));
            answers.push((request, image));
        }
        answers
    }

    /// The page says the image `key` has come (or failed): what is next goes. An image the
    /// page shows twice comes once, and says so twice.
    pub fn loaded(&mut self, key: &K) -> Vec<(R, Image)> {
        if let Some(at) = self.sending.iter().position(|(sent, _)| sent == key) {
            self.sending.remove(at);
            self.progress += 1;
        }
        self.send()
    }

    pub fn progress(&self) -> u64 {
        self.progress
    }

    /// Whether images that are here wait for those on their way: never with no limit.
    pub fn holds_back(&self) -> bool {
        !self.ready.is_empty()
    }

    /// If nothing has come since `progress` while images are held back, takes those on
    /// their way as come (a page that never says so would get no more): what is next goes.
    pub fn unstick(&mut self, progress: u64) -> Vec<(R, Image)> {
        if self.progress != progress || !self.holds_back() {
            return Vec::new();
        }
        self.sending.clear();
        self.progress += 1;
        self.send()
    }

    /// Another page: nothing is on its way to it; the requests not sent yet are given back.
    pub fn clear(&mut self) -> Vec<R> {
        self.sending.clear();
        self.ready
            .drain(..)
            .map(|(_, request, _)| request)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1 << 20;

    fn image(size: usize) -> Image {
        Image::new("image/jpeg", vec![0; size])
    }

    fn sent(answers: Vec<(u32, Image)>) -> Vec<u32> {
        answers.into_iter().map(|(request, _)| request).collect()
    }

    fn queued(limit: Option<usize>, sizes: &[usize]) -> Pacer<u32, u32> {
        let mut pacer = Pacer::new(limit);
        for (i, size) in (0..).zip(sizes) {
            pacer.queue(i, i, image(*size));
        }
        pacer
    }

    #[test]
    fn large_images_go_one_at_a_time_in_order() {
        let mut pacer = queued(Some(LIMIT), &[LIMIT, LIMIT, LIMIT]);
        assert_eq!(sent(pacer.send()), [0]);
        assert!(pacer.holds_back());
        // Not on its way: nothing goes.
        assert!(pacer.loaded(&1).is_empty());
        assert_eq!(sent(pacer.loaded(&0)), [1]);
        // Said twice.
        assert!(pacer.loaded(&0).is_empty());
        assert_eq!(sent(pacer.loaded(&1)), [2]);
        assert!(!pacer.holds_back());
    }

    #[test]
    fn small_images_go_together() {
        let mut pacer = queued(Some(LIMIT), &[LIMIT / 4; 5]);
        assert_eq!(sent(pacer.send()), [0, 1, 2, 3]);
        assert_eq!(sent(pacer.loaded(&2)), [4]);
    }

    #[test]
    fn with_no_limit_every_image_goes_as_it_comes() {
        let mut pacer = queued(None, &[LIMIT, LIMIT, LIMIT]);
        assert_eq!(sent(pacer.send()), [0, 1, 2]);
        assert!(!pacer.holds_back());
    }

    #[test]
    fn images_held_back_go_once_nothing_has_come_for_long() {
        let mut pacer = queued(Some(LIMIT), &[LIMIT, LIMIT, LIMIT]);
        assert_eq!(sent(pacer.send()), [0]);
        let progress = pacer.progress();
        assert_eq!(sent(pacer.unstick(progress)), [1]);
        // Something came since: not stuck.
        let progress = pacer.progress();
        assert!(pacer.loaded(&1).len() == 1);
        assert!(pacer.unstick(progress).is_empty());
    }

    #[test]
    fn another_page_gives_back_what_wasnt_sent() {
        let mut pacer = queued(Some(LIMIT), &[LIMIT, LIMIT]);
        assert_eq!(sent(pacer.send()), [0]);
        assert_eq!(pacer.clear(), [1]);
        assert!(!pacer.holds_back());
        pacer.queue(7, 7, image(LIMIT));
        assert_eq!(sent(pacer.send()), [7]);
    }
}
