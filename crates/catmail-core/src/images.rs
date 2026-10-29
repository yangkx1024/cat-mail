//! Remote images of a message's HTML, for a reader the user told to show them.

use std::sync::Arc;

use catmail_html::MAX_REMOTE_IMAGES;
use catmail_proto::http::{ImageFetcher, RemoteImage};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::engine::blocking;
use crate::image_cache::{ImageCache, MAX_CACHE_BYTES};
use crate::runtime::Runtime;
use crate::{Error, Result};

/// Images fetched at once, as a browser does per server: a newsletter refers to dozens.
const CONCURRENT_FETCHES: usize = 6;

/// The remote images [`MailClient::fetch_remote_images`](crate::MailClient::fetch_remote_images)
/// fetches, each as it comes. Dropping it stops the fetches still going.
pub struct RemoteImageStream {
    fetched: mpsc::Receiver<(String, Result<RemoteImage>)>,
}

impl RemoteImageStream {
    /// The next image fetched, or why it couldn't be, in the order they finish; `None`
    /// once every one has.
    pub async fn next(&mut self) -> Option<(String, Result<RemoteImage>)> {
        self.fetched.recv().await
    }

    /// The next one if it has already finished, without waiting: for taking in the images
    /// that come together at once.
    pub fn next_ready(&mut self) -> Option<(String, Result<RemoteImage>)> {
        self.fetched.try_recv().ok()
    }
}

pub(crate) fn fetch(
    rt: &Runtime,
    fetcher: ImageFetcher,
    cache: Arc<ImageCache>,
    urls: Vec<String>,
) -> RemoteImageStream {
    let (sender, fetched) = mpsc::channel(CONCURRENT_FETCHES);
    rt.spawn(async move {
        let mut urls: Vec<String> = urls.into_iter().take(MAX_REMOTE_IMAGES).collect();
        // Cached ones first: they come at once, and nothing is fetched for them.
        let cached = {
            let (cache, urls) = (cache.clone(), urls.clone());
            blocking(move || Ok(cache.get_many(&urls)))
                .await
                .unwrap_or_default()
        };
        for (url, image) in cached {
            urls.retain(|u| *u != url);
            if sender.send((url, Ok(image))).await.is_err() {
                return;
            }
        }

        let permits = Arc::new(Semaphore::new(CONCURRENT_FETCHES));
        let mut fetches = JoinSet::new();
        for url in urls {
            let (fetcher, permits, cache) = (fetcher.clone(), permits.clone(), cache.clone());
            fetches.spawn(async move {
                let _permit = permits.acquire_owned().await;
                let image = match fetcher.fetch(&url).await {
                    Ok(image) => keep(cache, url.clone(), image).await,
                    Err(e) => Err(Error::from(e)),
                };
                (url, image)
            });
        }
        let mut stored = false;
        loop {
            // Nobody waits any more: dropping the set aborts the fetches still going.
            let done = tokio::select! {
                done = fetches.join_next() => done,
                () = sender.closed() => break,
            };
            let Some(done) = done else {
                break;
            };
            let fetched = match done {
                Ok(fetched) => fetched,
                Err(e) => {
                    tracing::warn!(error = %e, "remote image fetch failed");
                    continue;
                }
            };
            stored |= fetched.1.is_ok();
            if sender.send(fetched).await.is_err() {
                break;
            }
        }
        drop(fetches);
        if !stored {
            return;
        }
        match blocking(move || Ok(cache.trim(MAX_CACHE_BYTES)?)).await {
            Ok(0) => {}
            Ok(freed) => tracing::debug!(freed, "trimmed the image cache"),
            Err(e) => tracing::warn!(error = %e, "couldn't trim the image cache"),
        }
    });
    RemoteImageStream { fetched }
}

/// Caches `image` for `url`, and hands it back. Not being able to keep it is no reason not
/// to show it.
async fn keep(cache: Arc<ImageCache>, url: String, image: RemoteImage) -> Result<RemoteImage> {
    blocking(move || {
        if let Err(e) = cache.put(&url, &image) {
            tracing::warn!(error = %e, "couldn't cache a remote image");
        }
        Ok(image)
    })
    .await
}
