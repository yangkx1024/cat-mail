//! Remote images of a message's HTML, fetched over HTTP(S) when the user asks to see them.
//!
//! Loading one tells the sender the message was read, and from where; that is why the
//! reader asks first. What it sends beyond that is kept to the URL itself: no cookies, no
//! `Referer` (not even on redirects), and nothing but `GET`. TLS is the same rustls
//! configuration as the mail servers'.

use std::time::Duration;

use catmail_types::ErrorKind;
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};

use crate::TlsConfig;

/// Larger answers are given up on: a message's image is rarely more than a few MiB, and
/// nothing stops a server from sending without end.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Per image, from the request to the last byte.
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Only `http` and `https` are fetched: not `file:`, not relative URLs.
    #[error("not a web address: {0}")]
    NotWeb(String),

    #[error("{0}")]
    Request(#[source] reqwest::Error),

    #[error("the server answered {0}")]
    Status(u16),

    #[error("not an image")]
    NotAnImage,

    #[error("larger than {} MiB", MAX_IMAGE_BYTES / (1024 * 1024))]
    TooLarge,

    #[error("couldn't set up the HTTP client: {0}")]
    Setup(#[source] reqwest::Error),
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Request(_) => ErrorKind::Network,
            Self::Status(404 | 410) => ErrorKind::NotFound,
            Self::Status(_) | Self::NotAnImage | Self::TooLarge => ErrorKind::Refused,
            Self::NotWeb(_) => ErrorKind::Invalid,
            Self::Setup(_) => ErrorKind::Internal,
        }
    }
}

/// An image as the server sent it, not decoded.
#[derive(Debug, Clone)]
pub struct RemoteImage {
    /// Its type, from its first bytes when they are a well-known raster format, else the
    /// server's `Content-Type`.
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// Fetches remote images. Cheap to clone; clones share the connections.
#[derive(Clone)]
pub struct ImageFetcher {
    client: Client,
    max_bytes: usize,
}

impl ImageFetcher {
    pub fn new(tls: &TlsConfig) -> Result<Self, Error> {
        Self::with_timeout(tls, TIMEOUT)
    }

    fn with_timeout(tls: &TlsConfig, timeout: Duration) -> Result<Self, Error> {
        let client = Client::builder()
            .use_preconfigured_tls(tls.client_config())
            .user_agent(concat!("cat-mail/", env!("CARGO_PKG_VERSION")))
            .referer(false)
            .redirect(Policy::limited(MAX_REDIRECTS))
            .timeout(timeout)
            .connect_timeout(CONNECT_TIMEOUT.min(timeout))
            .build()
            .map_err(Error::Setup)?;
        Ok(Self {
            client,
            max_bytes: MAX_IMAGE_BYTES,
        })
    }

    /// The image at `url`, an `http` or `https` URL; redirects to anything else fail.
    pub async fn fetch(&self, url: &str) -> Result<RemoteImage, Error> {
        let parsed = Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| Error::NotWeb(url.to_owned()))?;
        let mut response = self
            .client
            .get(parsed)
            .send()
            .await
            .map_err(Error::Request)?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > self.max_bytes as u64)
        {
            return Err(Error::TooLarge);
        }
        let declared = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(|v| v.trim().to_ascii_lowercase())
            // SVG never shows (renderers' SVG decoders read local files): a failure, so it
            // is neither kept nor shown.
            .filter(|v| v.strip_prefix("image/").is_some_and(|s| !s.is_empty()))
            .filter(|v| v != "image/svg+xml");
        let capacity = response.content_length().unwrap_or(0) as usize;
        let mut bytes = Vec::with_capacity(capacity.min(self.max_bytes));
        while let Some(chunk) = response.chunk().await.map_err(Error::Request)? {
            if bytes.len() + chunk.len() > self.max_bytes {
                return Err(Error::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        // Servers label images loosely (`application/octet-stream`, or the wrong format);
        // what decodes them goes by the type.
        let mime_type = sniff(&bytes)
            .map(str::to_owned)
            .or(declared)
            .ok_or(Error::NotAnImage)?;
        Ok(RemoteImage { mime_type, bytes })
    }
}

/// The type of a well-known raster image, from its first bytes.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
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

#[cfg(test)]
mod tests {
    use catmail_testkit::{FakeHttp, Reply, response};

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn send(status: &str, headers: &[&str], body: &[u8]) -> Reply {
        Reply::Send(response(status, headers, body))
    }

    fn fetcher() -> ImageFetcher {
        ImageFetcher::new(&TlsConfig::with_roots_only([]).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn fetches_an_image_without_cookies_or_referrer() {
        let server = FakeHttp::start(|path| match path {
            "/moved" => send("302 Found", &["Location: /p.png", "Set-Cookie: id=1"], b""),
            _ => send("200 OK", &["Content-Type: application/octet-stream"], PNG),
        })
        .await;
        let image = fetcher().fetch(&server.url("/moved")).await.unwrap();
        assert_eq!(image.mime_type, "image/png", "sniffed, not as labelled");
        assert_eq!(image.bytes, PNG);

        let heads = server.heads();
        assert_eq!(heads.len(), 2, "{heads:?}");
        assert!(heads[1].starts_with("GET /p.png "), "{}", heads[1]);
        for head in &heads {
            let head = head.to_ascii_lowercase();
            assert!(!head.contains("\r\nreferer:"), "{head}");
            assert!(!head.contains("\r\ncookie:"), "{head}");
        }
    }

    #[tokio::test]
    async fn keeps_the_declared_type_of_what_it_cannot_sniff_but_svg() {
        let server = FakeHttp::start(|path| match path {
            "/a" => send("200 OK", &["Content-Type: IMAGE/AVIF; q=1"], b"x"),
            _ => send("200 OK", &["Content-Type: image/svg+xml"], b"<svg/>"),
        })
        .await;
        let image = fetcher().fetch(&server.url("/a")).await.unwrap();
        assert_eq!(image.mime_type, "image/avif");
        let svg = fetcher().fetch(&server.url("/b.svg")).await;
        assert!(matches!(svg, Err(Error::NotAnImage)), "{svg:?}");
    }

    #[tokio::test]
    async fn refuses_what_is_not_an_image() {
        let server = FakeHttp::start(|path| match path {
            "/page" => send("200 OK", &["Content-Type: text/html"], b"<p>hi</p>"),
            "/gone" => send("404 Not Found", &["Content-Type: image/png"], PNG),
            _ => send("302 Found", &["Location: file:///etc/passwd"], b""),
        })
        .await;
        let fetcher = fetcher();
        let fetch = async |path: &str| fetcher.fetch(&server.url(path)).await;
        assert!(matches!(fetch("/page").await, Err(Error::NotAnImage)));
        assert!(matches!(fetch("/gone").await, Err(Error::Status(404))));
        // Not followed: the redirect stays an answer that isn't an image.
        assert!(matches!(
            fetch("/local").await,
            Err(Error::Status(302) | Error::Request(_))
        ));
        for url in ["file:///etc/passwd", "cid:logo", "a.png", "//x/a.png"] {
            assert!(
                matches!(fetcher.fetch(url).await, Err(Error::NotWeb(_))),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn gives_up_on_large_or_slow_answers() {
        let server = FakeHttp::start(|path| match path {
            "/declared" => send("200 OK", &["Content-Type: image/png"], &[0; 64]),
            "/slow" => Reply::Held(response("200 OK", &["Content-Type: image/png"], PNG)),
            // Chunked, so only counting tells.
            _ => Reply::Send(
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n\
                     40\r\n{}\r\n0\r\n\r\n",
                    "x".repeat(64)
                )
                .into_bytes(),
            ),
        })
        .await;
        let mut fetcher = fetcher();
        fetcher.max_bytes = 32;
        let fetch = async |path: &str| fetcher.fetch(&server.url(path)).await;
        assert!(matches!(fetch("/declared").await, Err(Error::TooLarge)));
        assert!(matches!(fetch("/chunked").await, Err(Error::TooLarge)));

        let tls = TlsConfig::with_roots_only([]).unwrap();
        let fetcher = ImageFetcher::with_timeout(&tls, Duration::from_millis(100)).unwrap();
        let slow = fetcher.fetch(&server.url("/slow")).await;
        assert!(
            matches!(&slow, Err(Error::Request(e)) if e.is_timeout()),
            "{slow:?}"
        );
    }

    #[test]
    fn sniffs_raster_formats() {
        assert_eq!(sniff(PNG), Some("image/png"));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a\x01"), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"<svg xmlns="), None);
        assert_eq!(sniff(b""), None);
    }
}
