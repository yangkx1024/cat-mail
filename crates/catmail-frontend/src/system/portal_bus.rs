//! What the clients of the portals share (opening at login's Background portal, the
//! updates' Flatpak portal): the objects a portal makes for its caller, and noticing that
//! the portal that took a call has left the bus.

use futures_util::StreamExt as _;
use futures_util::future::{self, FutureExt as _, LocalBoxFuture};
use zbus::Proxy;
use zbus::proxy::{Builder, CacheProperties};

/// The object a portal makes under `base` for the caller `sender` (its unique name) and the
/// caller's `token`: `base/<sender without ':', '.' as '_'>/token`.
pub fn handle_path(base: &str, sender: &str, token: &str) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    format!("{base}/{sender}/{token}")
}

/// This connection's unique name, which the portals name their objects after.
pub fn unique_name(bus: &zbus::Connection) -> zbus::Result<String> {
    bus.unique_name()
        .map(ToString::to_string)
        .ok_or_else(|| zbus::Error::Failure("no name on the session bus".into()))
}

/// `interface` at `path` of `destination`, its properties never cached: the portals'
/// objects are short-lived, or answer with signals.
pub async fn proxy<'a>(
    bus: &zbus::Connection,
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Builder::new(bus)
        .destination(destination)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// Resolves once the one that sent `reply` has left the bus (crashed, restarted), at once if
/// it has already. A portal that leaves knows nothing any more of what it was asked, and
/// what it was to answer never comes. Watched by the unique name of the one that took the
/// call: the well-known name also changes hands when the bus starts the portal for the
/// call, which is no leaving.
pub async fn left(
    bus: &zbus::Connection,
    reply: &zbus::Message,
) -> zbus::Result<LocalBoxFuture<'static, ()>> {
    let taker = reply
        .header()
        .sender()
        .ok_or_else(|| zbus::Error::Failure("the portal's reply has no sender".into()))?
        .to_owned();
    let names = zbus::fdo::DBusProxy::builder(bus)
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let mut gone = names
        .receive_name_owner_changed_with_args(&[(0, taker.as_str())])
        .await?;
    // It may have left before that listened.
    if !names.name_has_owner(taker.into()).await? {
        return Ok(future::ready(()).boxed_local());
    }
    Ok(async move {
        gone.next().await;
    }
    .boxed_local())
}
