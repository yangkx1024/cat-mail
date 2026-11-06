//! Where the work a view starts runs: owned by the view (it ends with the window), or on a
//! thread of its own.

use nsh_core::element::Owner;
use nsh_core::prelude::*;

/// The view's owner (the mailbox's, or a composer window's), which the work started from
/// anywhere in it belongs to: a row's press, say, starts what outlives the row.
#[derive(Clone, Default)]
struct ViewOwner(Option<Owner>);

/// The work started anywhere in the view from now on belongs to `owner`: the
/// mailbox's, or a composer window's (whose notifications time out with a task too).
pub fn own_tasks(owner: Owner) {
    global::<ViewOwner>().set(ViewOwner(Some(owner)));
}

/// Runs `future` for as long as the mailbox (or the composer, in its window) is shown.
pub fn in_view(future: impl Future<Output = ()> + 'static) {
    let owner = global::<ViewOwner>().with_untracked(|o| o.0);
    match owner {
        Some(owner) => {
            owner.run(|| spawn(future));
        }
        None => {
            spawn(future);
        }
    }
}

/// Runs `f` on a thread of its own (reading a file, say), for its result; `None` if the
/// thread panicked.
pub async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (done, result) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = done.send(f());
    });
    result.await.ok()
}
