//! The images a message's HTML refers to: which are remote, the URLs a reader that serves
//! them itself points them at, and inline parts embedded for a renderer that can't
//! resolve `cid:`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::sanitize::{clean, sanitizer};

/// What `f` makes of each image URL in HTML that [`sanitize_html`](crate::sanitize_html)
/// produced, in document order, leaving out the `None`s; the URLs themselves (a `data:`
/// one can be large) aren't copied.
pub(crate) fn image_sources<T: Send + 'static>(html: &str, f: fn(&str) -> Option<T>) -> Vec<T> {
    let found = Arc::new(Mutex::new(Vec::new()));
    let sink = found.clone();
    sanitizer(
        Some(Box::new(move |src| {
            if let Some(found) = f(src) {
                sink.lock().unwrap().push(found);
            }
            None
        })),
        None,
    )
    .clean(html);
    std::mem::take(&mut *found.lock().unwrap())
}

/// More remote images than any message needs to show; the rest of a longer list is left
/// out.
pub const MAX_REMOTE_IMAGES: usize = 200;

/// The URL of a remote (`http`, `https`) image, normalized as a URL parser writes it, or
/// `None` for anything else. A reader may ask for an image by its URL as it found it in
/// the HTML or parsed, so remote images are fetched, cached and looked up by this one
/// spelling.
pub fn remote_image_url(url: &str) -> Option<String> {
    let url = url::Url::parse(url.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

/// The Content-ID an image's `src` refers to: what makes a part inline
/// ([`cid_image_ids`]) is what is embedded in its place.
fn cid_image_id(src: &str) -> Option<String> {
    content_id_from_url(src.trim())
}

/// The Content-IDs that the images in HTML that [`sanitize_html`](crate::sanitize_html)
/// produced refer to by `cid:` URL: what a reader shows within the body. Only images: a
/// `cid:` link, or the text `cid:…`, shows nothing, so the part it names stays an
/// attachment.
pub fn cid_image_ids(html: &str) -> HashSet<String> {
    image_sources(html, cid_image_id).into_iter().collect()
}

/// An image a reader shows (an inline part, a remote image): its bytes and their type.
/// Cloning it is cheap, and so is handing its bytes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    mime_type: Arc<str>,
    bytes: Arc<[u8]>,
}

impl Image {
    /// Of the type its bytes start with, if they are a well-known format
    /// ([`sniff_image`]), else of `mime_type`, which comes from the message (or a server)
    /// and so is replaced if it is unusual.
    pub fn new(mime_type: &str, bytes: impl Into<Arc<[u8]>>) -> Self {
        let bytes = bytes.into();
        let mime_type = sniff_image(&bytes).unwrap_or_else(|| media_type(mime_type));
        Self {
            mime_type: mime_type.into(),
            bytes,
        }
    }

    pub fn mime_type(&self) -> &Arc<str> {
        &self.mime_type
    }

    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// As a `data:` URL, for a page that may load nothing else.
    pub(crate) fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime_type,
            STANDARD.encode(&self.bytes)
        )
    }
}

/// An image of a message that a reader serves its page itself ([`serve_images`]).
#[derive(Debug, PartialEq, Eq)]
pub enum ServedImage {
    /// An inline part, by Content-ID.
    Inline(String),
    /// A remote image, by its URL as [`remote_image_url`] writes it.
    Remote(String),
}

impl ServedImage {
    /// What follows the reader's base URL for this image: its kind, then its key with
    /// every byte but the unreserved ones percent-encoded, which a URL parser leaves as
    /// it is in any URL, so the page asks for it with this very spelling.
    pub fn path(&self) -> String {
        let (kind, key) = match self {
            ServedImage::Inline(content_id) => ("cid", content_id),
            ServedImage::Remote(url) => ("remote", url),
        };
        format!("{kind}/{}", utf8_percent_encode(key, RESERVED))
    }

    /// The image a [`path`](Self::path) names, or `None` for anything else.
    pub fn from_path(path: &str) -> Option<Self> {
        let (kind, key) = path.split_once('/')?;
        let key = percent_decode(key);
        match kind {
            "cid" if !key.is_empty() => Some(ServedImage::Inline(key)),
            "remote" => remote_image_url(&key)
                .filter(|url| *url == key)
                .map(ServedImage::Remote),
            _ => None,
        }
    }
}

/// HTML pointed at the images a reader serves its page itself ([`serve_images`]).
pub struct Served {
    pub html: String,
    /// The remote images it refers to, as [`remote_image_url`] writes them, each once, in
    /// document order: what the reader fetches when the user asks for them.
    pub remote_urls: Vec<String>,
}

/// HTML that [`sanitize_html`](crate::sanitize_html) produced, with the images a reader
/// serves its page itself pointed at `base` followed by their [`ServedImage::path`]: the
/// `cid:` images, and the first [`MAX_REMOTE_IMAGES`] remote ones, which the reader
/// shows once they are cached or the user asks for them. Other URLs (`file:`, relative,
/// remote ones past those) stay as they are, for the page to block.
///
/// `base` must leave the path opaque (`scheme:...`, not `scheme://...`): a URL parser
/// resolves dot segments in a hierarchical path, and a Content-ID may be `..`.
pub fn serve_images(html: &str, base: &str) -> Served {
    let base = base.to_owned();
    let remote = Arc::new(Mutex::new(Vec::new()));
    let picked = remote.clone();
    let serving = sanitizer(
        Some(Box::new(move |src| {
            let image = match cid_image_id(src) {
                Some(content_id) => ServedImage::Inline(content_id),
                None => {
                    let url = remote_image_url(src)?;
                    let mut remote = picked.lock().unwrap();
                    if !remote.contains(&url) {
                        if remote.len() == MAX_REMOTE_IMAGES {
                            return None;
                        }
                        remote.push(url.clone());
                    }
                    ServedImage::Remote(url)
                }
            };
            Some(format!("{base}{}", image.path()))
        })),
        None,
    );
    let html = clean(&serving, html);
    let remote_urls = std::mem::take(&mut *remote.lock().unwrap());
    Served { html, remote_urls }
}

/// HTML that [`sanitize_html`](crate::sanitize_html) produced, with each `cid:` image whose Content-ID is in
/// `inline` replaced by a `data:` URL of the part, for a renderer that can't resolve
/// `cid:` (a browser). The values ammonia's filter returns are not checked again, so the
/// URL is built here ([`Image::data_url`]) rather than taken from the caller.
pub fn embed_inline_images(html: &str, inline: HashMap<String, Image>) -> String {
    let embedding = sanitizer(
        Some(Box::new(move |src| {
            let image = inline.get(&cid_image_id(src)?)?;
            Some(image.data_url())
        })),
        None,
    );
    clean(&embedding, html)
}

/// The type of a well-known raster image, from its first bytes. Senders and servers
/// label images loosely (`application/octet-stream`, or the wrong format); what decodes
/// them goes by the type.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    const SIGNATURES: [(&[u8], &str); 6] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"BM", "image/bmp"),
        (b"\0\0\x01\0", "image/x-icon"),
    ];
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    SIGNATURES
        .iter()
        .find(|(magic, _)| bytes.starts_with(magic))
        .map(|&(_, mime_type)| mime_type)
}

/// The MIME type as it may appear in a `data:` URL or a `Content-Type`; it comes from the
/// message, so anything unusual is replaced.
fn media_type(mime_type: &str) -> &str {
    let valid = mime_type.split_once('/').is_some_and(|(t, s)| {
        !t.is_empty()
            && !s.is_empty()
            && mime_type
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/.+-".contains(&b))
    });
    if valid {
        mime_type
    } else {
        "application/octet-stream"
    }
}

/// The Content-ID a `cid:` URL refers to (RFC 2392: the scheme in any case, the rest
/// percent-encoded), or `None` for any other URL.
fn content_id_from_url(url: &str) -> Option<String> {
    let scheme = url.get(..4)?;
    if !scheme.eq_ignore_ascii_case("cid:") {
        return None;
    }
    let id = percent_decode(&url[4..]);
    (!id.is_empty()).then_some(id)
}

/// Every byte but the unreserved ones (RFC 3986).
const RESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `s` with its `%XX` decoded; bytes that aren't UTF-8 become U+FFFD.
fn percent_decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize_html;

    #[test]
    fn content_id_from_url_accepts_only_cid_urls() {
        assert_eq!(content_id_from_url("CID:a%40b").as_deref(), Some("a@b"));
        assert_eq!(content_id_from_url("cid:logo").as_deref(), Some("logo"));
        assert_eq!(content_id_from_url("cid:"), None);
        assert_eq!(content_id_from_url("https://x/cid:logo"), None);
        assert_eq!(content_id_from_url("ci"), None);
        // Multi-byte input shorter than the scheme must not panic on a char boundary.
        assert_eq!(content_id_from_url("图片"), None);
    }

    #[test]
    fn the_browser_page_keeps_the_style_sheet() {
        let html = sanitize_html("<style>.a{color:red}</style><img src=\"cid:logo@x\">");
        let inline = HashMap::from([("logo@x".to_owned(), Image::new("image/png", *b"hi"))]);
        let page = embed_inline_images(&html, inline);
        assert!(page.starts_with("<style>.a{color:red;}</style>"), "{page}");
        assert!(page.contains("data:image/png;base64,aGk="), "{page}");
    }

    #[test]
    fn finds_and_embeds_images_by_parsing() {
        let clean = sanitize_html(
            r#"<p>src="cid:in-text"</p><img src="cid:logo" alt="a"><img data-src="https://x/a.gif">
            <img src="https://tracker.example/p.gif"><a href="cid:logo">link</a>"#,
        );
        assert_eq!(
            image_sources(&clean, |src| Some(src.to_owned())),
            ["cid:logo", "https://tracker.example/p.gif"]
        );

        let inline = HashMap::from([
            ("logo".to_owned(), Image::new("image/png", *b"hi")),
            (
                "odd".to_owned(),
                Image::new("image/gif\"><script>", *b"gif"),
            ),
        ]);
        let clean = format!("{clean}<img src=\"cid:odd\"><img src=\"cid:missing\">");
        let embedded = embed_inline_images(&clean, inline);
        assert!(embedded.contains(r#"<img src="data:image/png;base64,aGk=" alt="a">"#));
        assert!(embedded.contains(r#"<img src="data:application/octet-stream;base64,Z2lm">"#));
        assert!(embedded.contains(r#"<img src="cid:missing">"#));
        assert!(
            embedded.contains(r#"src="cid:in-text""#),
            "text is not an attribute"
        );
        assert!(
            embedded.contains(r#"href="cid:logo""#),
            "only images are replaced"
        );
        assert!(embedded.contains("https://tracker.example/p.gif"));
        assert!(!embedded.contains("<script>"));
    }

    #[test]
    fn only_images_make_parts_inline() {
        let clean = sanitize_html(
            r#"<img src=" cid:logo@x "><img src="CID:a%40b"><a href="cid:report@x">report</a>
            <p>see cid:note@x</p><img src="https://x/cid:remote">"#,
        );
        assert_eq!(
            cid_image_ids(&clean),
            HashSet::from(["logo@x".to_owned(), "a@b".to_owned()])
        );
    }

    /// The `src` attributes of `html`, as written: sanitizing it again to find them would
    /// drop a scheme it doesn't know.
    fn srcs(html: &str) -> Vec<String> {
        html.split(" src=\"")
            .skip(1)
            .map(|rest| rest[..rest.find('"').unwrap()].replace("&amp;", "&"))
            .collect()
    }

    #[test]
    fn serves_inline_and_remote_images_from_the_base() {
        let clean = sanitize_html(
            r#"<img src="cid:logo@x" alt="a"><img src=" CID:a%40b "><img src="Cid:图片@x">
            <img src="cid:a b/c?d#e"><img src="cid:..">
            <img src="HTTPS://x.example/a b.png?q=1&amp;r=%2F"><img src="data:image/png;base64,aGk">
            <img src="cid:"><img src="file:///etc/passwd"><a href="cid:report@x">report</a>"#,
        );
        let served = serve_images(&clean, "app:7/");
        assert_eq!(
            served.remote_urls,
            ["https://x.example/a%20b.png?q=1&r=%2F"]
        );
        let served = served.html;
        let srcs = srcs(&served);
        assert_eq!(
            srcs,
            [
                "app:7/cid/logo%40x",
                "app:7/cid/a%40b",
                "app:7/cid/%E5%9B%BE%E7%89%87%40x",
                "app:7/cid/a%20b%2Fc%3Fd%23e",
                "app:7/cid/..",
                "app:7/remote/https%3A%2F%2Fx.example%2Fa%2520b.png%3Fq%3D1%26r%3D%252F",
                "data:image/png;base64,aGk",
                "cid:",
            ]
        );
        assert!(served.contains(r#"href="cid:report@x""#), "only images");
        let expected = [
            ServedImage::Inline("logo@x".into()),
            ServedImage::Inline("a@b".into()),
            ServedImage::Inline("图片@x".into()),
            ServedImage::Inline("a b/c?d#e".into()),
            ServedImage::Inline("..".into()),
            ServedImage::Remote("https://x.example/a%20b.png?q=1&r=%2F".into()),
        ];
        for (src, image) in srcs.iter().zip(expected) {
            // As a page's URL parser (WHATWG, as WebKit's) writes it: the page asks for
            // the image by this very URL.
            assert_eq!(url::Url::parse(src).unwrap().as_str(), src);
            let path = src.strip_prefix("app:7/").unwrap();
            assert_eq!(ServedImage::from_path(path), Some(image));
        }
    }

    #[test]
    fn serves_as_many_remote_images_as_a_reader_shows() {
        let many: String = (0..MAX_REMOTE_IMAGES + 5)
            .map(|i| format!(r#"<img src="https://x.example/{i}.png">"#))
            .chain([r#"<img src="https://x.example/0.png">"#.to_owned()])
            .collect();
        let served = serve_images(&many, "app:");
        assert_eq!(served.remote_urls.len(), MAX_REMOTE_IMAGES);
        assert_eq!(served.remote_urls[0], "https://x.example/0.png");
        let srcs = srcs(&served.html);
        let served_count = srcs.iter().filter(|s| s.starts_with("app:")).count();
        assert_eq!(served_count, MAX_REMOTE_IMAGES + 1, "and the first again");
        assert_eq!(srcs[MAX_REMOTE_IMAGES], "https://x.example/200.png");
    }

    #[test]
    fn a_served_path_names_only_what_could_be_served() {
        for path in [
            "",
            "cid",
            "cid/",
            "other/x",
            "remote/file%3A%2F%2F%2Fetc%2Fpasswd",
            // Not as `remote_image_url` writes it.
            "remote/HTTPS%3A%2F%2Fx.example%2F",
        ] {
            assert_eq!(ServedImage::from_path(path), None, "{path}");
        }
    }

    #[test]
    fn sniffs_raster_formats() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(sniff_image(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a\x01"), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg xmlns="), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn an_image_is_of_the_type_its_bytes_are() {
        let png = Image::new("application/octet-stream", *b"\x89PNG\r\n\x1a\n...");
        assert!(png.data_url().starts_with("data:image/png;base64,"));
        let mislabelled = Image::new("image/jpeg", *b"GIF89a");
        assert_eq!(&**mislabelled.mime_type(), "image/gif");
        let svg = Image::new("image/svg+xml", *b"<svg/>");
        assert_eq!(&**svg.mime_type(), "image/svg+xml");
        let odd = Image::new("image/png\r\nX-Evil: 1", *b"?");
        assert_eq!(&**odd.mime_type(), "application/octet-stream");
        assert_eq!(&**odd.bytes(), b"?");
    }

    #[test]
    fn finds_remote_images() {
        let remote_urls = |html: &str| serve_images(html, "app:").remote_urls;
        assert_eq!(
            remote_urls(
                r#"<p>hi</p><img src="https://Tracker.example/p.gif?a=1&amp;b=2" alt="">
                <img src="cid:logo"><img src="data:image/png;base64,aGk"><a href="https://x">x</a>
                <img src="http://x.example/a b.png"><img src="file:///etc/passwd"><img src="a.png">
                <img src="https://tracker.example/p.gif?a=1&b=2">"#
            ),
            [
                "https://tracker.example/p.gif?a=1&b=2",
                "http://x.example/a%20b.png"
            ]
        );
        assert!(remote_urls(r#"<img src="cid:logo"><img src="//x/a.png">"#).is_empty());
        assert_eq!(remote_image_url("cid:logo"), None);
        assert_eq!(
            remote_image_url("HTTPS://x.example/a b.png").as_deref(),
            Some("https://x.example/a%20b.png")
        );
    }
}
