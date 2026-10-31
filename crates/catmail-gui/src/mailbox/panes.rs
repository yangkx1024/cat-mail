//! The mailbox's layout, and its right pane for what the list has selected: a message
//! with the toolbar of commands on it, a draft (`draft.rs`) or an outbox entry
//! (`outbox.rs`).

use catmail_core::{Flags, MessageSummary};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::list::List;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, Focusable as _, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, rems,
};

use super::Mailbox;
use super::folders;
use super::message_list::{Row, Source};
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
                            .items_baseline()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .children(note.map(|note| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(note)
                            })),
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
        let seen = message.flags.contains(Flags::SEEN);
        let flagged = message.flags.contains(Flags::FLAGGED);
        let has_html = self.reader.read(cx).has_html();
        let focus = self.focus_handle(cx);
        let can_archive = self.can_archive(message);
        let can_edit_labels = self.can_edit_labels(cx);
        let context = Some(actions::MAILBOX);
        let (read_icon, read_label) = if seen {
            (Hero::Envelope, "Mark as unread")
        } else {
            (Hero::EnvelopeOpen, "Mark as read")
        };
        layout::toolbar(cx)
            .child(self.back_button(cx))
            .child(
                Button::new("archive")
                    .ghost()
                    .small()
                    .icon(Hero::ArchiveBox)
                    .disabled(!can_archive)
                    .tooltip_with_action("Archive", &Archive, Some(actions::MESSAGE_LIST))
                    .on_click(cx.listener(|this, _, window, cx| this.archive(window, cx))),
            )
            .child(
                Button::new("delete")
                    .ghost()
                    .small()
                    .icon(Hero::Trash)
                    .tooltip_with_action("Delete", &Delete, Some(actions::MESSAGE_LIST))
                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
            )
            .child(
                Button::new("toggle-read")
                    .ghost()
                    .small()
                    .icon(read_icon)
                    .tooltip_with_action(read_label, &ToggleRead, context)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle(Flags::SEEN, window, cx)),
                    ),
            )
            .child(
                Button::new("move")
                    .ghost()
                    .small()
                    .icon(Hero::FolderArrowRight)
                    .tooltip_with_action("Move to", &MoveToFolder, context)
                    .on_click(cx.listener(|this, _, window, cx| this.move_to_folder(window, cx))),
            )
            .child(
                Button::new("more")
                    .ghost()
                    .small()
                    .icon(Hero::EllipsisHorizontal)
                    .tooltip("More")
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |menu, _, _| {
                        menu.action_context(focus.clone())
                            .menu_with_icon(
                                if flagged { "Remove flag" } else { "Flag" },
                                Hero::Flag,
                                Box::new(ToggleFlag),
                            )
                            .when(can_edit_labels, |menu| {
                                menu.menu_with_icon("Labels", Hero::Tag, Box::new(EditLabels))
                            })
                            .separator()
                            .menu_with_icon_and_disabled(
                                "Open in browser",
                                Hero::ArrowTopRightOnSquare,
                                Box::new(OpenInBrowser),
                                !has_html,
                            )
                    }),
            )
    }
}
