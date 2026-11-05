//! The engine, the app's and not a window's: windows come and go, the engine runs until
//! the app quits. It opens while the first window shows; views wait for it with
//! [`opened`].

use std::sync::{Mutex, MutexGuard, PoisonError};

use catmail_core::MailClient;
use futures_channel::oneshot;

enum State {
    /// Still opening; each waiter is told when it's done.
    Opening(Vec<oneshot::Sender<()>>),
    Open(MailClient),
    /// Why it couldn't be opened.
    Failed(String),
    /// Taken to shut down.
    Closed,
}

static STATE: Mutex<State> = Mutex::new(State::Opening(Vec::new()));

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends the opening, with the engine or why there is none.
pub fn opened_with(result: catmail_core::Result<MailClient>) {
    let next = match result {
        Ok(client) => State::Open(client),
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "couldn't open the engine");
            State::Failed(e.to_string())
        }
    };
    let waiters = match std::mem::replace(&mut *state(), next) {
        State::Opening(waiters) => waiters,
        _ => Vec::new(),
    };
    for waiter in waiters {
        // A view that went away meanwhile doesn't wait anymore.
        let _ = waiter.send(());
    }
}

/// The engine once it is open, or why it couldn't be.
pub async fn opened() -> Result<MailClient, String> {
    loop {
        let wait = match &mut *state() {
            State::Open(client) => return Ok(client.clone()),
            State::Failed(e) => return Err(e.clone()),
            State::Closed => return Err("catmail is quitting".into()),
            State::Opening(waiters) => {
                let (sender, wait) = oneshot::channel();
                waiters.push(sender);
                wait
            }
        };
        let _ = wait.await;
    }
}

/// The open engine. The mailbox shows only once [`opened`] gave it, so it is there.
pub fn client() -> MailClient {
    match &*state() {
        State::Open(client) => client.clone(),
        _ => panic!("the mailbox shows only once the engine is open"),
    }
}

/// The engine, to shut it down as the app quits.
pub fn take() -> Option<MailClient> {
    match std::mem::replace(&mut *state(), State::Closed) {
        State::Open(client) => Some(client),
        _ => None,
    }
}
