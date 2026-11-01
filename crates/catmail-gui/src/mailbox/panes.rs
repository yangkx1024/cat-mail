//! The mailbox's layout: the list under its heading, which has the commands on the
//! messages checked, and the right pane for what the list has selected: a message with
//! the toolbar of commands on it, a draft (`draft.rs`) or an outbox entry (`outbox.rs`).

use catmail_core::MessageSummary;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::list::List;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, rems,
};

use super::Mailbox;
use super::folders;
use super::message_commands::{self, Applies, Buttons};
use super::message_list::{self, Check, Row, Source};
use crate::actions::{self, *};
use crate::assets::Hero;
use crate::layout;

impl Mailbox {
    pub(crate) fn render_panes(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rem = window.rem_size();
        // The right pane only while the list has something selected; until then the list
        // takes its width. Hidden rather than left out, so the group keeps the width
        // the list was given for when the pane is back.
        let row = self.messages.read(cx).delegate().selected_row().cloned();
        let reading = row.is_some();
        let widest_list = if reading {
            rems(40.).to_pixels(rem)
        } else {
            Pixels::MAX
        };
        h_resizable("mailbox")
            .child(
                resizable_panel()
                    .size(rems(15.).to_pixels(rem))
                    // Its own width, not a share of the reader's while that is hidden.
                    .flex_none()
                    .size_range(rems(12.).to_pixels(rem)..rems(24.).to_pixels(rem))
                    .child(self.render_sidebar(cx)),
            )
            .child(
                resizable_panel()
                    .size(rems(23.).to_pixels(rem))
                    .size_range(rems(16.).to_pixels(rem)..widest_list)
                    .child(self.render_list_column(reading, cx)),
            )
            .child(
                resizable_panel()
                    .visible(reading)
                    .children(row.map(|row| self.render_detail(row, cx))),
            )
    }

    fn render_list_column(&self, reading: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.messages.read(cx).delegate().source().cloned();
        let title: SharedString = match &source {
            Some(Source::Search(text)) => format!("Results for “{text}”").into(),
            _ => self.place_label().unwrap_or_default().into(),
        };
        let note = match &source {
            Some(Source::Inboxes | Source::Folder(_)) => {
                let unread = folders::unread(self.shown_folders());
                (unread > 0).then(|| format!("{unread} unread"))
            }
            Some(Source::Outbox(account)) => self
                .account(*account)
                .filter(|a| !a.status.send.is_empty() || a.status.sync.is_some())
                .map(|_| "Sending resumes when the connection is back.".to_owned()),
            _ => None,
        };
        let list = self.messages.read(cx).delegate();
        // What the commands on the messages checked do, while there are some.
        let checked = list
            .has_checked()
            .then(|| self.applies(list.checked_messages()));
        let (all_checked, has_messages) = (list.all_checked(), list.has_messages());
        let lists_messages = source.as_ref().is_some_and(Source::lists_messages);
        v_flex()
            .size_full()
            .when(reading, |this| {
                this.border_r_1().border_color(cx.theme().border)
            })
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .p_3()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("sync")
                                    .ghost()
                                    .rounded_full()
                                    .icon(Hero::ArrowPath)
                                    .accessibility_label("Sync now")
                                    .loading(self.syncing())
                                    .disabled(
                                        self.accounts.as_deref().unwrap_or_default().is_empty(),
                                    )
                                    .tooltip_with_action("Sync now", &Sync, Some(actions::MAILBOX))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.sync(window, cx)),
                                    ),
                            )
                            .child(
                                Input::new(&self.search)
                                    .id("search")
                                    .flex_1()
                                    // As round as "Compose" across from it.
                                    .rounded_full()
                                    .prefix(Icon::new(Hero::MagnifyingGlass).small())
                                    .cleanable(true),
                            ),
                    )
                    .child(
                        h_flex()
                            .px_1()
                            .gap_2()
                            .when(lists_messages, |this| {
                                this.child(self.render_check_all(all_checked, has_messages, cx))
                            })
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2()
                                    .items_baseline()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(title),
                                    )
                                    // The commands on the messages checked take its room.
                                    .when(checked.is_none(), |this| {
                                        this.children(note.map(|note| {
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(note)
                                        }))
                                    }),
                            )
                            .when_some(checked, |this, applies| {
                                this.child(self.render_checked_toolbar(applies, cx))
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .key_context(actions::MESSAGE_LIST)
                    .child(
                        // The sidebar's margins, inside the scrolling: rows as far in, room
                        // below the last, and the scrollbar in the margin, off the rows.
                        List::new(&self.messages).scroll_bounce(true).px_3().pb_3(),
                    ),
            )
    }

    /// Checks every message listed, or none; its menu checks those read, flagged and so on.
    fn render_check_all(
        &self,
        all_checked: bool,
        has_messages: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let this = cx.entity().downgrade();
        h_flex()
            .flex_none()
            .gap_0p5()
            .child(
                Checkbox::new("check-all")
                    .checked(all_checked)
                    .disabled(!has_messages)
                    .accessibility_label("Select all")
                    .on_click(cx.listener(|this, &check: &bool, _, cx| {
                        this.messages.update(cx, |list, cx| {
                            if check {
                                message_list::check(list, Check::All, cx);
                            } else {
                                message_list::uncheck_all(list, cx);
                            }
                        })
                    })),
            )
            .child(
                Button::new("check-menu")
                    .ghost()
                    .xsmall()
                    .icon(Hero::ChevronDown)
                    .disabled(!has_messages)
                    .tooltip("Select")
                    .dropdown_menu(move |menu, _, _| {
                        Check::MENU.into_iter().fold(menu, |menu, which| {
                            menu.item(PopupMenuItem::new(which.label()).on_click(
                                layout::after_menu(this.clone(), move |this, _, cx| {
                                    this.messages
                                        .update(cx, |list, cx| message_list::check(list, which, cx))
                                }),
                            ))
                        })
                    }),
            )
    }

    /// The commands on the messages checked, as the message toolbar has them.
    fn render_checked_toolbar(&self, applies: Applies, cx: &mut Context<Self>) -> impl IntoElement {
        let Buttons { place, marks } = message_commands::buttons(
            applies,
            |name| format!("checked-{name}").into(),
            true,
            cx.listener(|this, &command, window, cx| {
                this.run(command, this.checked(cx), window, cx)
            }),
        );
        h_flex()
            .flex_none()
            .gap_1()
            .children(place.into_iter().map(|b| b.small()))
            .child(separator())
            .children(marks.into_iter().map(|b| b.small()))
    }

    fn render_detail(&self, row: Row, cx: &mut Context<Self>) -> AnyElement {
        match row {
            Row::Message(message) => v_flex()
                .size_full()
                .child(self.render_message_toolbar(&message, cx))
                .child(div().flex_1().min_h_0().child(self.reader.clone()))
                .into_any_element(),
            Row::Draft(draft) => self.render_draft(&draft, cx).into_any_element(),
            Row::Outbox(entry) => self.render_outbox_entry(&entry, cx).into_any_element(),
        }
    }

    /// Closes the right pane, back to the list across the window, as Escape in the
    /// list does.
    pub(crate) fn back_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("back")
            .ghost()
            .small()
            .icon(Hero::ArrowLeft)
            .tooltip("Back")
            .on_click(cx.listener(|this, _, window, cx| this.close_detail(window, cx)))
    }

    pub(super) fn close_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.messages
            .update(cx, |list, cx| list.set_selected_index(None, window, cx));
        self.reader.update(cx, |reader, cx| reader.clear(cx));
        cx.notify();
    }

    fn render_message_toolbar(
        &self,
        message: &MessageSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reader = self.reader.read(cx);
        let has_html = reader.has_html();
        let can_reply_all = reader.can_reply_all();
        let blocked_images = reader.blocked_remote_images();
        let can_edit_labels = self.can_edit_labels(cx);
        let context = Some(actions::MAILBOX);
        let Buttons { place, marks } = message_commands::buttons(
            self.applies([message]),
            |name| name.into(),
            true,
            cx.listener(|this, &command, window, cx| {
                this.run(command, this.selected(cx), window, cx)
            }),
        );
        // In groups: where the message is, its marks, its content, and answering it.
        layout::toolbar(cx)
            .child(self.back_button(cx))
            .child(separator())
            .children(place.into_iter().map(|b| b.small()))
            .when(can_edit_labels, |this| {
                this.child(
                    Button::new("labels")
                        .ghost()
                        .small()
                        .icon(Hero::Tag)
                        .tooltip_with_action("Labels", &EditLabels, Some(actions::MESSAGE_LIST))
                        .on_click(cx.listener(|this, _, window, cx| this.edit_labels(window, cx))),
                )
            })
            .child(separator())
            .children(marks.into_iter().map(|b| b.small()))
            .child(separator())
            .when_some(blocked_images, |this, loading| {
                this.child(
                    Button::new("show-remote-images")
                        .ghost()
                        .small()
                        .icon(Hero::Photo)
                        .tooltip("Show remote images")
                        .loading(loading)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reader
                                .update(cx, |reader, cx| reader.show_remote_images(window, cx))
                        })),
                )
            })
            .child(
                Button::new("open-in-browser")
                    .ghost()
                    .small()
                    .icon(Hero::ArrowTopRightOnSquare)
                    .disabled(!has_html)
                    .tooltip("Open in browser")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reader
                            .update(cx, |reader, cx| reader.open_in_browser(window, cx))
                    })),
            )
            .child(separator())
            .child(
                Button::new("reply")
                    .ghost()
                    .small()
                    .icon(Hero::ArrowUturnLeft)
                    .tooltip_with_action("Reply", &Reply, context)
                    .on_click(cx.listener(|this, _, window, cx| this.reply(false, window, cx))),
            )
            .when(can_reply_all, |this| {
                this.child(
                    Button::new("reply-all")
                        .ghost()
                        .small()
                        .icon(Hero::ArrowUturnLeftDouble)
                        .tooltip_with_action("Reply all", &ReplyAll, context)
                        .on_click(cx.listener(|this, _, window, cx| this.reply(true, window, cx))),
                )
            })
            .child(
                Button::new("forward")
                    .ghost()
                    .small()
                    .icon(Hero::ArrowUturnRight)
                    .tooltip_with_action("Forward", &Forward, context)
                    .on_click(cx.listener(|this, _, window, cx| this.forward(window, cx))),
            )
    }
}

/// Between groups of a toolbar's buttons.
fn separator() -> Separator {
    Separator::vertical().h_5().mx_1()
}
