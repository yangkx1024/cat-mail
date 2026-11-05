//! Saving, sending, closing and discarding, one engine call at a time: an older save
//! never lands after what was sent.

use catmail_frontend::commands::Confirm;
use catmail_frontend::composer::{
    AfterSave, Closing, Finish, discard_confirm, error_text, unsendable,
};
use gpui_kit::{App, Context, Task, Window};

use super::*;
use crate::layout;

impl Composer {
    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.life.save_due() {
            return;
        }
        let (written, _) = self.written(cx);
        let Some(written) = self.life.begin_save(written) else {
            return;
        };
        let client = self.client.clone();
        let id = self.draft;
        self._save = cx.spawn_in(window, async move |this, cx| {
            let Written { from, content } = written.clone();
            let result = client.update_draft(id, from, content).await;
            this.update_in(cx, |this, window, cx| {
                let result = result.map(drop).map_err(|e| error_text(&e));
                match this.life.save_ended(written, result) {
                    AfterSave::Finish => this.run_finish(window, cx),
                    AfterSave::SaveLater => this.changed(window, cx),
                    AfterSave::Nothing => {}
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(super) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (written, problem) = self.written(cx);
        if let Some(problem) = unsendable(&written, problem) {
            self.show_field_error(problem, window, cx);
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
        let (written, problem) = self.written(cx);
        match self.life.close(&written, problem.is_some()) {
            Closing::Wait => false,
            Closing::Fix => {
                if let Some(problem) = problem {
                    self.show_field_error(problem, window, cx);
                }
                self.stop_closing_all(cx);
                false
            }
            Closing::Now => {
                self.unregister(cx);
                true
            }
            Closing::Finish => {
                self.finish(Finish::Close, window, cx);
                false
            }
        }
    }

    pub(super) fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let Confirm { title, description } = discard_confirm();
        layout::confirm_danger(
            title,
            description,
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
        if self.life.ending().is_some() {
            return;
        }
        self.error = None;
        self._debounce = Task::ready(());
        if self.life.finish(how) {
            self.run_finish(window, cx);
        }
        cx.notify();
    }

    /// Saves what is left, then sends, deletes or just closes; the window goes when that
    /// worked.
    pub(super) fn run_finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (written, _) = self.written(cx);
        let Some(steps) = self.life.begin_finish(&written) else {
            return;
        };
        let client = self.client.clone();
        let id = self.draft;
        self._save = cx.spawn_in(window, async move |this, cx| {
            let result = steps.run(&client, id, written).await;
            this.update_in(cx, |this, window, cx| {
                match this.life.finish_ended(result.map_err(|e| e.to_string())) {
                    Ok(()) => {
                        this.unregister(cx);
                        window.remove_window();
                    }
                    Err(e) => {
                        this.stop_closing_all(cx);
                        this.error = Some(e.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        });
    }

    fn unregister(&self, cx: &mut App) {
        open_composers(cx).remove(self.draft);
    }

    /// The window stays open to be dealt with: closing them all failed.
    pub(super) fn stop_closing_all(&self, cx: &mut App) {
        open_composers(cx).stop_closing_all();
    }

    pub(super) fn show_field_error(
        &mut self,
        (field, message): FieldError,
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
