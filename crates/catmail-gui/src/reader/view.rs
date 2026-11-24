//! How the reading pane looks: the toolbar of commands, the message's header, the web
//! view its body is shown in, and its attachments.

use catmail_frontend::commands::MessageCommand;
use catmail_frontend::format;
use catmail_frontend::list as rows;
use catmail_frontend::reader::{self, Header};
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, menu_button, separator, spinner};
use crate::clock::clock;
use crate::icons::{Icon, icon};
use crate::mailbox::commands;
use crate::mailbox::compose;
use crate::mailbox::list::{self};
use crate::mailbox::panes::back_button;
use crate::theme::SelectableText as _;
use crate::{chrome, web};

use super::{
    BodyHeight, Reading, Retry, ShowDetails, State, open_in_browser, page_painted, page_state,
    save_attachment, show_remote_images,
};

/// The pane: its toolbar over the message. It stays while no message is selected
/// (`mailbox::panes` keeps it out of sight), and so does its web view.
pub fn reader() -> View {
    let failed_ = computed(|| {
        global::<Reading>().with(|r| match &r.state {
            State::Failed(error) => Some(error.clone()),
            _ => None,
        })
    });
    view()
        .flex_1()
        .min_w(px(0.0))
        .child(toolbar())
        .child(dynamic(move || match failed_.get() {
            Some(error) => failed(error).into_element(),
            None => view().into_element(),
        }))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                // Told apart from the message's row by the tests.
                .test_id("reader")
                .style_when(move || failed_.with(Option::is_some), |s| s.hidden())
                .child(message()),
        )
}

fn failed(error: String) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .p(px(16.0))
        .child(
            text(format!("Couldn't open the message: {error}"))
                .text_sm()
                .selectable_text()
                .text_color(move || theme().muted_foreground),
        )
        .child(
            view()
                .h(px(32.0))
                .px(px(14.0))
                .rounded(px(16.0))
                .justify_center()
                .bg(move || theme().secondary)
                .on_press(|| {
                    // Opened again from the top.
                    global::<Reading>().update(|r| r.id = None);
                    global::<Retry>().update(|r| r.0 += 1);
                })
                .child(
                    text("Try again")
                        .text_sm()
                        .text_color(move || theme().secondary_foreground),
                ),
        )
}

/// The commands on the message open, in groups: where it is, its marks, its content,
/// answering it.
fn toolbar() -> impl IntoElement {
    // Looked up once for all the buttons.
    let message = computed(list::selected_message);
    let selected = move || message.get().into_iter().collect::<Vec<_>>();
    let applies = computed(move || commands::applies(&selected()));
    let can_edit_labels =
        move || message.with(|m| m.as_ref().is_some_and(commands::can_edit_labels));
    let offered =
        move || global::<Reading>().with(|r| r.remote.offered().filter(|_| r.state == State::Open));
    let has_html = move || global::<Reading>().with(|r| r.html.is_some());
    chrome::toolbar()
        .gap(px(2.0))
        .child(back_button())
        .child(separator(Size::Bar))
        .child(commands::group(
            &MessageCommand::PLACE,
            selected,
            applies,
            Size::Bar,
        ))
        .child(when(can_edit_labels, || {
            view().flex_row().child(icon_button(
                || Icon::Tag,
                || "Labels".to_owned(),
                || true,
                Size::Bar,
                commands::edit_labels,
            ))
        }))
        .child(separator(Size::Bar))
        .child(commands::group(
            &MessageCommand::MARKS,
            selected,
            applies,
            Size::Bar,
        ))
        .child(separator(Size::Bar))
        .child(web::remote_images_button(
            offered,
            Size::Bar,
            show_remote_images,
        ))
        .child(icon_button(
            || Icon::ArrowTopRightOnSquare,
            || "Open in browser".to_owned(),
            has_html,
            Size::Bar,
            open_in_browser,
        ))
        .child(separator(Size::Bar))
        .child(icon_button(
            || Icon::ArrowUturnLeft,
            || "Reply".to_owned(),
            || true,
            Size::Bar,
            || compose::reply(false),
        ))
        .child(when(
            move || global::<Reading>().with(|r| r.can_reply_all),
            || {
                icon_button(
                    || Icon::ArrowUturnLeftDouble,
                    || "Reply all".to_owned(),
                    || true,
                    Size::Bar,
                    || compose::reply(true),
                )
            },
        ))
        .child(icon_button(
            || Icon::ArrowUturnRight,
            || "Forward".to_owned(),
            || true,
            Size::Bar,
            || compose::forward(false),
        ))
        .child(menu_button(
            || "More ways to forward".to_owned(),
            Size::Bar,
            compose::choose_forward,
        ))
}

/// The message: its title, sender, the body and the attachments, scrolling as one.
fn message() -> impl IntoElement {
    let reading = global::<Reading>();
    let header = computed(move || reading.with(|r| r.header.clone()));
    let field = move |f: fn(&Header) -> String| move || header.with(f);
    let labels = computed(move || {
        list::selected_message()
            .map(|m| list::user_label_names(&m))
            .unwrap_or_default()
    });
    let details = move || global::<ShowDetails>().with(|d| d.0);
    let muted = move || theme().muted_foreground;
    let title = view()
        .gap(px(8.0))
        .child(
            text(field(|h| h.subject.clone()))
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .selectable_text()
                .text_color(move || theme().foreground),
        )
        .child(when(
            move || labels.with(|l| !l.is_empty()),
            move || {
                view().flex_row().flex_wrap().gap(px(4.0)).child(each(
                    move || labels.get(),
                    |name| name.clone(),
                    |name, _| {
                        text(move || name.get())
                            .px(px(6.0))
                            .rounded(px(4.0))
                            .text_xs()
                            .bg(move || theme().muted)
                            .text_color(move || theme().muted_foreground)
                    },
                ))
            },
        ));
    let sender = view()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .child(
            view()
                .w(px(36.0))
                .h(px(36.0))
                .flex_shrink_0()
                .rounded(px(18.0))
                .items_center()
                .justify_center()
                .bg(move || header.with(|h| list::avatar_color(&h.sender_key)))
                .child(
                    text(move || header.with(|h| rows::initial(&h.sender)))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(move || theme().foreground),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .flex_wrap()
                        .gap(px(8.0))
                        .child(
                            text(field(|h| h.sender.clone()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .selectable_text()
                                .text_color(move || theme().foreground),
                        )
                        .child(
                            text(field(|h| h.sender_email.clone().unwrap_or_default()))
                                .text_xs()
                                .selectable_text()
                                .text_color(muted),
                        )
                        .child(view().flex_1())
                        .child(
                            text(field(|h| format::date(h.date, &clock())))
                                .text_xs()
                                .text_color(muted),
                        ),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            text(field(|h| h.recipients.clone()))
                                .number_of_lines(1)
                                .flex_shrink(1.0)
                                .min_w(px(0.0))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(
                            view()
                                .px(px(6.0))
                                .py(px(2.0))
                                .rounded(px(4.0))
                                .on_press(move || global::<ShowDetails>().update(|d| d.0 = !d.0))
                                .child(
                                    text(
                                        move || {
                                            if details() { "Hide details" } else { "Details" }
                                        },
                                    )
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(move || theme().foreground),
                                ),
                        ),
                ),
        );
    let full_header = when(details, move || {
        view()
            .gap(px(4.0))
            .p(px(12.0))
            .rounded(px(10.0))
            .bg(move || theme().muted)
            .child(each(
                move || header.with(|h| h.details(&clock())),
                |(name, _)| *name,
                move |detail, _| {
                    view()
                        .flex_row()
                        .items_start()
                        .gap(px(8.0))
                        .child(
                            text(move || detail.get().0.to_owned())
                                .w(px(64.0))
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(
                            text(move || detail.get().1)
                                .flex_1()
                                .text_xs()
                                .selectable_text()
                                .text_color(move || theme().foreground),
                        )
                },
            ))
    });
    let scroll_ref = element_ref();
    page_state().borrow_mut().scroll = Some(scroll_ref);
    let sideways = web::Sideways::new();
    let opacity = |hidden: bool| if hidden { 0.0_f32 } else { 1.0 };
    // While the message loads, as the list has it.
    let header_shown = move || opacity(reading.with(|r| r.header_hidden));
    // Not the web view's: one not drawn may not paint, and would show what it last had
    // once it is; [`body`] covers it instead.
    let shown = move || opacity(reading.with(|r| r.hidden));
    let pane = scroll().flex_1().element_ref(&scroll_ref).child(
        view()
            .px(px(24.0))
            .pt(px(20.0))
            .pb(px(32.0))
            .gap(px(16.0))
            .child(
                view()
                    .gap(px(16.0))
                    .opacity(header_shown)
                    .child(title)
                    .child(sender)
                    .child(full_header),
            )
            .child(body(sideways))
            .child(view().opacity(shown).child(attachments())),
    );
    // The page's sideways bar at the bottom of the pane, where its own would be.
    sideways.around(pane)
}

fn body(sideways: web::Sideways) -> impl IntoElement {
    let reading = global::<Reading>();
    // One web view for every message, made with the pane: a new page loads into it.
    let hidden = move || reading.with(|r| r.hidden);
    let loading = move || reading.with(|r| r.state == State::Loading);
    // The page pads itself.
    view()
        .mx(px(-16.0))
        .child(web_view(sideways))
        // Over the web view while the page is hidden: WebKit keeps painting under it.
        .child(when(hidden, move || {
            view()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .bottom(px(0.0))
                .z_index(1)
                .bg(page_background)
                .items_center()
                .pt(px(32.0))
                .child(when(loading, loading_message))
        }))
}

/// Where the body will be, while the message loads (from the server, likely).
fn loading_message() -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(spinner(16.0).text_color(move || theme().muted_foreground))
        .child(
            text("Loading the message…")
                .text_sm()
                .text_color(move || theme().muted_foreground),
        )
}

/// The page's background, which shows in the web view until the page paints.
fn page_background() -> AdaptiveColor {
    web::background(global::<Reading>().with(|r| r.page_dark))
}

fn web_view(sideways: web::Sideways) -> impl IntoElement {
    let reading = global::<Reading>();
    // Its own memo: the page (a long message's is large) is copied for the web view only
    // when it changes, not on each change of what else the pane shows.
    let page = computed(move || reading.with(|r| r.page.clone()));
    web::web_view(
        move || page.get(),
        |height| global::<BodyHeight>().set(BodyHeight(height)),
        page_painted,
        sideways,
    )
    .bg(page_background)
    .h(move || px(global::<BodyHeight>().with(|h| h.0).max(web::MIN_HEIGHT)))
}

/// Under the text, where reading it ends.
fn attachments() -> impl IntoElement {
    let attachments = move || global::<Reading>().with(|r| r.attachments.clone());
    when(
        move || global::<Reading>().with(|r| !r.attachments.is_empty()),
        move || {
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .pt(px(16.0))
                .border_t_1()
                .border_color(move || theme().border)
                .child(each(
                    attachments,
                    |a| a.id,
                    |attachment, _| {
                        let image = move || attachment.with(|a| a.mime_type.starts_with("image/"));
                        let names = move || attachment.with(reader::attachment_label);
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(30.0))
                            .px(px(10.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(move || theme().border)
                            .hover(|s| s.bg(move || theme().accent))
                            .on_press(move || save_attachment(attachment.get_untracked()))
                            .tooltip(crate::buttons::tooltip(move || {
                                format!("Save “{}”", names().0)
                            }))
                            .child(
                                icon(
                                    move || if image() { Icon::Photo } else { Icon::Document },
                                    16.0,
                                )
                                .text_color(move || theme().muted_foreground),
                            )
                            .child(
                                text(move || names().1)
                                    .text_sm()
                                    .text_color(move || theme().foreground),
                            )
                    },
                ))
        },
    )
}
