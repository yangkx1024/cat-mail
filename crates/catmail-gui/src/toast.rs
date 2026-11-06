//! Notifications: a stack at the window's bottom right, the newest nearest the corner, each
//! going away by itself after a while. On nsh-base's unstyled toast primitives, styled
//! here.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use nsh_base::{
    Anchor, ToastManager, ToastMotion, ToastOptions, ToastStackState, ToastTransitionStatus, theme,
    toast, toast_stack,
};
use nsh_core::prelude::*;
use nsh_core::time::now;

use crate::icons::{Icon, icon};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    /// How long it shows: what went wrong is read more slowly than a "Saved".
    fn timeout(self) -> Duration {
        match self {
            Level::Info => Duration::from_secs(4),
            Level::Warning | Level::Error => Duration::from_secs(8),
        }
    }
}

type Note = (Level, String);

/// The toasts and their lifecycle. Not reactive: [`Shown`] is what the stack shows.
#[derive(Default)]
struct Manager {
    toasts: Option<ToastManager<u64, Note>>,
    next: u64,
    /// The lifecycle is being advanced: a task runs while there are toasts.
    ticking: bool,
}

#[derive(Clone, Default)]
struct Toasts(Rc<RefCell<Manager>>);

/// The toasts the stack shows, oldest first.
#[derive(Clone, Default)]
struct Shown(Vec<(u64, Note, ToastTransitionStatus)>);

/// Whether the pointer is on the stack, which pauses the timeouts.
#[derive(Clone, Default)]
struct Expanded(Option<ToastStackState>);

fn manager() -> Rc<RefCell<Manager>> {
    global::<Toasts>().with_untracked(|t| t.0.clone())
}

/// Shows `text` for a while.
pub fn notify(level: Level, text: impl Into<String>) {
    let manager = manager();
    {
        let mut m = manager.borrow_mut();
        m.next += 1;
        let id = m.next;
        m.toasts
            .get_or_insert_with(|| ToastManager::new(ToastMotion::sonner()))
            .push(
                id,
                (level, text.into()),
                ToastOptions::timeout(Some(level.timeout())),
                now(),
            );
    }
    sync();
    tick();
}

fn sync() {
    let shown = manager().borrow().toasts.as_ref().map(|t| {
        t.visible(3)
            .map(|(id, note, status)| (*id, note.clone(), status))
            .collect()
    });
    global::<Shown>().set(Shown(shown.unwrap_or_default()));
}

/// Advances the toasts' lifecycle while there are any: no timer runs when there are
/// none.
fn tick() {
    if std::mem::replace(&mut manager().borrow_mut().ticking, true) {
        return;
    }
    crate::app::in_mailbox(async {
        loop {
            timer(Duration::from_millis(50)).await;
            let paused = global::<Expanded>()
                .with_untracked(|e| e.0.is_some_and(|s| untracked(|| s.is_expanded())));
            let manager = manager();
            let (changed, empty) = {
                let mut m = manager.borrow_mut();
                let Some(toasts) = m.toasts.as_mut() else {
                    break;
                };
                let changed = toasts.advance(now(), paused).changed;
                (changed, toasts.is_empty())
            };
            if changed {
                sync();
            }
            if empty {
                break;
            }
        }
        manager().borrow_mut().ticking = false;
    });
}

fn dismiss(id: u64) {
    if let Some(toasts) = manager().borrow_mut().toasts.as_mut() {
        toasts.dismiss(&id, now());
    }
    sync();
    tick();
}

/// The stack, over the window.
pub fn toasts() -> impl IntoElement {
    let state = ToastStackState::new();
    global::<Expanded>().set(Expanded(Some(state)));
    overlay().absolute().right(px(16.0)).bottom(px(16.0)).child(
        toast_stack()
            .placement(Anchor::BottomRight)
            // Escape closes the message open.
            .close_on_escape(false)
            .state(state)
            .w(px(360.0))
            .child(each(
                move || global::<Shown>().with(|s| s.0.clone()),
                |(id, _, _)| *id,
                |item, _| {
                    let level = move || item.with(|(_, (level, _), _)| *level);
                    toast()
                        .transition_status(move || item.get().2)
                        .accessibility_label(move || Some(SharedString::from(item.get().1.1)))
                        .on_dismiss_request(move || dismiss(item.get().0))
                        .flex_row()
                        .items_start()
                        .gap(px(10.0))
                        .px(px(14.0))
                        .py(px(12.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(move || theme().border)
                        .bg(move || theme().popover)
                        .shadow_lg()
                        .transition_all()
                        .duration(200.0)
                        .starting_class("opacity-0 translate-y-4")
                        .ending_class("opacity-0 scale-95")
                        .child(
                            icon(
                                move || match level() {
                                    Level::Info => Icon::InformationCircle,
                                    Level::Warning => Icon::ExclamationTriangle,
                                    Level::Error => Icon::XCircle,
                                },
                                18.0,
                            )
                            .flex_shrink_0()
                            .text_color(move || match level() {
                                Level::Info => theme().muted_foreground,
                                Level::Warning | Level::Error => theme().destructive,
                            }),
                        )
                        .child(
                            text(move || item.get().1.1)
                                .flex_1()
                                .text_sm()
                                .selectable(true)
                                .text_color(move || theme().popover_foreground),
                        )
                },
            )),
    )
}
