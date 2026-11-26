//! A value of the app's that views follow, each on its own thread: the settings, where
//! the MCP server is. Reading it and starting to follow it happen under one lock, and so
//! do changing it and telling the followers: a follower misses no change, and hears the
//! changes in the order they were made, however many threads make them.

use std::sync::{Mutex, MutexGuard, PoisonError};

use futures_channel::mpsc;

pub struct Watched<T> {
    inner: Mutex<Inner<T>>,
}

struct Inner<T> {
    value: T,
    followers: Vec<mpsc::UnboundedSender<T>>,
}

impl<T: Clone + PartialEq> Watched<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(Inner {
                value,
                followers: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner<T>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self) -> T {
        self.lock().value.clone()
    }

    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.lock().value)
    }

    /// The value now, and each time it changes from then on.
    pub fn follow(&self) -> (T, mpsc::UnboundedReceiver<T>) {
        let mut inner = self.lock();
        let (follower, changes) = mpsc::unbounded();
        inner.followers.push(follower);
        (inner.value.clone(), changes)
    }

    pub fn set(&self, value: T) {
        self.update(|v| *v = value, |_| {});
    }

    /// Changes the value with `change`; if that changed it, `changed` runs with the new
    /// one, still in order with other changes (to queue it for writing, say), and the
    /// followers hear of it. The new value, or `None` if it stayed the same.
    pub fn update(&self, change: impl FnOnce(&mut T), changed: impl FnOnce(&T)) -> Option<T> {
        let mut inner = self.lock();
        let before = inner.value.clone();
        change(&mut inner.value);
        if inner.value == before {
            return None;
        }
        let Inner { value, followers } = &mut *inner;
        changed(value);
        // Those no longer following go.
        followers.retain(|follower| follower.unbounded_send(value.clone()).is_ok());
        Some(value.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn heard(changes: &mut mpsc::UnboundedReceiver<u32>) -> Vec<u32> {
        std::iter::from_fn(|| changes.try_recv().ok()).collect()
    }

    #[test]
    fn followers_hear_every_change_after_what_they_read() {
        let watched = Watched::new(0);
        watched.set(1);
        let (now, mut changes) = watched.follow();
        assert_eq!(now, 1);
        watched.set(2);
        // No change, nothing to hear.
        assert_eq!(watched.update(|v| *v = 2, |_| unreachable!()), None);
        assert_eq!(watched.update(|v| *v += 1, |_| {}), Some(3));
        assert_eq!(heard(&mut changes), [2, 3]);
        assert_eq!(watched.get(), 3);
    }

    #[test]
    fn those_no_longer_following_are_let_go() {
        let watched = Watched::new(0);
        drop(watched.follow());
        watched.set(1);
        assert!(watched.lock().followers.is_empty());
    }

    #[test]
    fn changes_from_many_threads_are_heard_in_order() {
        let watched = Arc::new(Watched::new(0_u32));
        let (_, mut changes) = watched.follow();
        let written = Arc::new(Mutex::new(Vec::new()));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (watched, written) = (watched.clone(), written.clone());
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        watched.update(|v| *v += 1, |v| written.lock().unwrap().push(*v));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let in_order: Vec<u32> = (1..=4000).collect();
        // The follower ends on the last value, and so would a file written in order.
        assert_eq!(heard(&mut changes), in_order);
        assert_eq!(*written.lock().unwrap(), in_order);
    }
}
