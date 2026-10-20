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
    ParentElement as _, SharedString, Styled as _, Window, div, rems,
};

use super::Mailbox;
use super::folders::FolderNode;
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
        h_resizable("mailbox")
            .child(
                resizable_panel()
                    .size(rems(15.).to_pixels(rem))
                    .size_range(rems(12.).to_pixels(rem)..rems(24.).to_pixels(rem))
                    .child(self.render_sidebar(cx)),
            )
            .child(
                resizable_panel()
                    .size(rems(23.).to_pixels(rem))
                    .size_range(rems(16.).to_pixels(rem)..rems(40.).to_pixels(rem))
                    .child(self.render_list_column(cx)),
            )
            .child(resizable_panel().child(self.render_detail(cx)))
    }

    fn render_list_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.messages.read(cx).delegate().source().cloned();
        let title: SharedString = match &source {
            Some(Source::Search(text)) => format!("Results for “{text}”").into(),
            _ => self.place_label().unwrap_or_default().into(),
        };
        let note = match &source {
            Some(Source::Folder(id)) => self
                .accounts
                .iter()
                .flatten()
                .find_map(|a| a.folders.find(*id))
                .map(FolderNode::unread)
                .filter(|&n| n > 0)
                .map(|n| format!("{n} unread")),
            Some(Source::Outbox(account)) => self
                .account(*account)
                .filter(|a| a.status.send.is_some() || a.status.sync.is_some())
                .map(|_| "Sending resumes when the connection is back.".to_owned()),
            _ => None,
        };
        v_flex()
            .size_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .p_3()
                    .child(
                        Input::new(&self.search)
                            .id("search")
                            .prefix(Icon::new(Hero::MagnifyingGlass).small())
                            .cleanable(true),
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
                    .px_1p5()
                    .key_context(actions::MESSAGE_LIST)
                    .child(List::new(&self.messages)),
            )
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let row = self.messages.read(cx).delegate().selected_row().cloned();
        match row {
            Some(Row::Message(message)) => v_flex()
                .size_full()
                .child(self.render_message_toolbar(&message, cx))
                .child(div().flex_1().min_h_0().child(self.reader.clone()))
                .into_any_element(),
            Some(Row::Draft(draft)) => self.render_draft(&draft, cx).into_any_element(),
            Some(Row::Outbox(entry)) => self.render_outbox_entry(&entry, cx).into_any_element(),
            None => self.reader.clone().into_any_element(),
        }
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
        let can_archive = self.archive_folder(cx).is_some();
        let context = Some(actions::MAILBOX);
        layout::toolbar(cx)
            .child(
                Button::new("reply")
                    .ghost()
                    .small()
                    .icon(Hero::ArrowUturnLeft)
                    .label("Reply")
                    .tooltip_with_action("Reply", &Reply, context)
                    .on_click(cx.listener(|this, _, window, cx| this.reply(false, window, cx))),
            )
            .child(
                Button::new("reply-all")
                    .ghost()
                    .small()
                    .label("Reply all")
                    .tooltip_with_action("Reply all", &ReplyAll, context)
                    .on_click(cx.listener(|this, _, window, cx| this.reply(true, window, cx))),
            )
            .child(
                Button::new("forward")
                    .ghost()
                    .small()
                    .icon(Hero::ArrowUturnRight)
                    .label("Forward")
                    .tooltip_with_action("Forward", &Forward, context)
                    .on_click(cx.listener(|this, _, window, cx| this.forward(window, cx))),
            )
            .child(div().flex_1())
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
                Button::new("move")
                    .ghost()
                    .small()
                    .icon(Hero::FolderArrowDown)
                    .tooltip_with_action("Move to", &MoveToFolder, context)
                    .on_click(cx.listener(|this, _, window, cx| this.move_to_folder(window, cx))),
            )
            .child(
                Button::new("more")
                    .ghost()
                    .small()
                    .icon(Hero::EllipsisHorizontal)
                    .tooltip("More")
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                        menu.action_context(focus.clone())
                            .menu_with_icon(
                                if seen {
                                    "Mark as unread"
                                } else {
                                    "Mark as read"
                                },
                                if seen {
                                    Hero::Envelope
                                } else {
                                    Hero::EnvelopeOpen
                                },
                                Box::new(ToggleRead),
                            )
                            .menu_with_icon(
                                if flagged { "Remove flag" } else { "Flag" },
                                Hero::Flag,
                                Box::new(ToggleFlag),
                            )
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
