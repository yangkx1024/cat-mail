//! Saving, sending, closing and discarding, one engine call at a time: an older save
//! never lands after what was sent.

use gpui_kit::{App, Context, SharedString, Task, Window};

use super::*;
use crate::layout;

impl Composer {
    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.dirty || self.finishing.is_some() {
            return;
        }
        let (content, _) = self.content(cx);
        self.dirty = false;
        if content == self.saved {
            return;
        }
        self.saving = true;
        let client = self.client.clone();
        let id = self.draft;
        self._save = cx.spawn_in(window, async move |this, cx| {
            let result = client.update_draft(id, content.clone()).await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(_) => {
                        this.saved = content;
                        this.save_state = SaveState::Saved;
                    }
                    Err(e) => {
                        this.dirty = true;
                        this.save_state = SaveState::Failed(error_text(&e));
                    }
                }
                if this.finishing.is_some() {
                    if !this.busy() {
                        this.run_finish(window, cx);
                    }
                } else if this.dirty {
                    this.changed(window, cx);
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(super) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (content, problem) = self.content(cx);
        if let Some(problem) = problem {
            self.show_field_error(problem, window, cx);
            return;
        }
        if content.recipients().next().is_none() {
            self.show_field_error(
                (Field::To, "Add at least one recipient.".into()),
                window,
                cx,
            );
            return;
        }
        self.finish(Finish::Send, window, cx);
    }

    pub(super) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    /// Whether the window can close now. If there is something to save first, that starts,
    /// and the window closes itself when it is done.
    pub(super) fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.finishing.is_some() {
            return false;
        }
        // A field that doesn't parse was never saved (what is saved always parses back,
        // see `format_addresses`): closing would lose what was typed there.
        let (content, problem) = self.content(cx);
        if let Some(problem) = problem {
            self.show_field_error(problem, window, cx);
            self.stop_quitting(cx);
            return false;
        }
        // What the fields hold, not `dirty`: the last keys may not have been seen yet.
        if !self.busy() && content == self.saved && !self.untouched() {
            self.unregister(cx);
            return true;
        }
        self.finish(Finish::Close, window, cx);
        false
    }

    pub(super) fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        layout::confirm_danger(
            "Discard this draft?",
            "It is deleted here and from the server's Drafts folder.",
            "Discard",
            window,
            cx,
            move |window, cx| {
                this.update(cx, |this, cx| this.finish(Finish::Discard, window, cx))
                    .ok();
            },
        );
    }

    fn finish(&mut self, how: Finish, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing.is_some() {
            return;
        }
        self.finishing = Some(how);
        self.error = None;
        self._debounce = Task::ready(());
        if !self.busy() {
            self.run_finish(window, cx);
        }
        cx.notify();
    }

    /// Saves what is left, then sends, deletes or just closes; the window goes when that
    /// worked.
    pub(super) fn run_finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(how) = self.finishing else {
            return;
        };
        let (content, _) = self.content(cx);
        // Sending an untouched draft (a reply that needs no words) still sends.
        let delete = how == Finish::Discard || (how == Finish::Close && self.untouched());
        let save = !delete && content != self.saved;
        let client = self.client.clone();
        let id = self.draft;
        self.saving = true;
        self._save = cx.spawn_in(window, async move |this, cx| {
            let result = async {
                if delete {
                    return client.delete_draft(id).await;
                }
                if save {
                    client.update_draft(id, content).await?;
                }
                if how == Finish::Send {
                    client.send_draft(id).await?;
                }
                Ok(())
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.unregister(cx);
                        window.remove_window();
                    }
                    Err(e) => {
                        this.finishing = None;
                        this.stop_quitting(cx);
                        this.error = Some(
                            match how {
                                Finish::Send => format!("Couldn't send: {e}"),
                                Finish::Close => format!("Couldn't save the draft: {e}"),
                                Finish::Discard => format!("Couldn't discard the draft: {e}"),
                            }
                            .into(),
                        );
                        cx.notify();
                    }
                }
            })
            .ok();
        });
    }

    fn unregister(&self, cx: &mut App) {
        let open = cx.default_global::<OpenComposers>();
        open.windows.remove(&self.draft);
        if open.quitting && open.windows.is_empty() {
            cx.quit();
        }
    }

    /// The window stays open to be dealt with, so the app does too.
    pub(super) fn stop_quitting(&self, cx: &mut App) {
        cx.default_global::<OpenComposers>().quitting = false;
    }

    pub(super) fn show_field_error(
        &mut self,
        (field, message): (Field, SharedString),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(field, Field::Cc | Field::Bcc) {
            self.show_cc_bcc = true;
        }
        let input = match field {
            Field::To => &self.to,
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        self.field_error = Some((field, message));
        cx.notify();
    }
}
