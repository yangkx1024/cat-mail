//! The events that tell about a change to messages: what a [`Touched`] says changed,
//! announced after the commit, as only ids (see `docs/architecture.md`). Local changes,
//! sync, sending and the replay of queued changes all announce what they touched this
//! way.

use std::collections::BTreeSet;

use catmail_store::Touched;
use catmail_types::{AccountId, Event};

use crate::engine::Engine;

/// Emits the events for `touched`, and wakes the accounts' background sync to make any
/// ops queued on the server. Returns its accounts.
pub(crate) fn announce(engine: &Engine, touched: &Touched) -> Vec<AccountId> {
    emit_lists(engine, touched);
    let accounts: BTreeSet<AccountId> = touched
        .lists()
        .map(|(a, _)| a)
        .chain(touched.refused.iter().copied())
        .collect();
    for &account in &accounts {
        engine.emit(Event::PendingOpsChanged { account });
        // Sync's own conflicts come through here too; that costs a needless look at the
        // queue, rarely.
        engine.wake(account);
    }
    emit_refused(engine, touched);
    accounts.into_iter().collect()
}

fn emit_lists(engine: &Engine, touched: &Touched) {
    for (account, folder) in touched.lists() {
        engine.emit(Event::MessagesChanged { account, folder });
    }
}

/// Tells about the changes `touched` refused, after the events that show them undone.
fn emit_refused(engine: &Engine, touched: &Touched) {
    for &account in &touched.refused {
        engine.emit(Event::ChangesRefused { account });
    }
}

/// Emits the events for a replay's changes: to the account's queue, and to the messages
/// of the folders in `touched`. Unlike [`announce`] it wakes nobody: the replay is what
/// a wake would ask for.
pub(crate) fn replayed(engine: &Engine, account: AccountId, touched: &Touched) {
    emit_lists(engine, touched);
    engine.emit(Event::PendingOpsChanged { account });
    emit_refused(engine, touched);
}
