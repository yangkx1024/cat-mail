//! The middle pane: what the selected place lists, or search results, newest first under a
//! heading for each day; a row on one line while nothing is open, the subject under the
//! sender while the reading pane is; the messages checked for a command on them all. What a
//! source lists, the days and lines, the reads and where the selection goes after them, and
//! what a row says are `catmail_frontend::mailbox::list`'s (`Listing`, `ListModel`); they are read
//! and kept in nsh's signals by `listing.rs`, and drawn here.

mod rows;

use catmail_core::FlagFilter;
use catmail_frontend::composer::ComposeFrom;
use catmail_frontend::mailbox::commands::MessageCommand;
use catmail_frontend::mailbox::list::{self, Listing, RowId};
use catmail_frontend::mailbox::{self, Source};
use nsh_base::theme;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::{ElementRef, input};
use nsh_core::geometry::Percentage;
use nsh_core::prelude::*;

use crate::accounts::{self, Accounts};
use crate::buttons::{Size, icon_button};
use crate::clock;
use crate::engine::client;
use crate::form;
use crate::icons::{Icon, icon};
use crate::mailbox::commands;
use crate::mailbox::listing::{
    Checked, ListRef, Loading, SearchText, Selected, SelectedPlace, checked_messages, clear_search,
    filter, keyed, load_more, settle, toggle_checked, typed,
};
use crate::motion;
use crate::shell::chrome;
use crate::shell::tasks::in_view;

use rows::{ROW_MARGIN_Y, item};
pub use rows::{avatar_color, user_label_names};

/// Whether the list is laid out beside the right pane: narrowed to its column, its rows
/// in two lines. Opening, it changes before [`PaneOpen`]; closing, after
/// ([`follow_selection`]).
#[derive(Clone, Copy, Default, PartialEq)]
struct ListOpen(bool);

/// Whether the right pane is in ([`follow_selection`]).
#[derive(Clone, Copy, Default, PartialEq)]
struct PaneOpen(bool);

/// The row last opened or closed, which [`follow_selection`] measures: a view in it
/// carries [`AnchorRef`].
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Anchored(pub(super) Option<RowId>);

#[derive(Clone, Default)]
pub(super) struct AnchorRef(pub(super) Option<ElementRef>);

#[derive(Clone, Default)]
struct SearchRef(Option<ElementRef>);

pub fn focus_search() {
    if let Some(search) = global::<SearchRef>().with_untracked(|r| r.0) {
        search.focus();
    }
}

/// Whether the list is laid out beside the right pane ([`ListOpen`]).
pub fn list_open() -> bool {
    global::<ListOpen>().get().0
}

/// Whether the right pane is in ([`PaneOpen`]).
pub fn pane_open() -> bool {
    global::<PaneOpen>().get().0
}

/// The right pane's first button, which closes it: the selection goes.
pub fn back_button() -> View {
    icon_button(
        || Icon::ArrowLeft,
        || "Back".to_owned(),
        || true,
        Size::Bar,
        || global::<Selected>().set(Selected(None)),
    )
}

/// A draft opens in its window: on Enter, or a second click on its row.
fn edit_selected_draft() {
    if let Some(RowId::Draft(id)) = global::<Selected>().with_untracked(|s| s.0) {
        super::compose::open(ComposeFrom::Draft(id));
    }
}

/// The search field took the focus: logs in to the servers a search asks, for the first
/// one not to wait for the login (`MailClient::connect_for_search`). An account already
/// connected, or that a search wouldn't ask, is left alone.
fn connect_ahead() {
    in_view(async {
        if let Err(e) = client().connect_for_search(None).await {
            tracing::warn!(error = %e, "couldn't connect ahead of a search");
        }
    });
}

/// The search field's clear button: nothing typed, and the place selected shown again
/// now rather than after a pause; the keys go back to the list.
fn clear_search_field(search: ElementRef) {
    settle(clear_search(), "");
    search.blur();
}

/// The keys of the commands on messages: on those checked, or else
/// the selected one.
fn bind_command_keys() {
    let keys = [
        ("e", MessageCommand::Archive),
        ("delete", MessageCommand::Delete),
        ("backspace", MessageCommand::Delete),
        ("mod+shift+m", MessageCommand::Move),
        ("mod+shift+u", MessageCommand::ToggleRead),
        ("mod+shift+l", MessageCommand::ToggleFlag),
    ];
    for (key, command) in keys {
        on_key(key, move |_| commands::run(command, keyed()));
    }
    // Gmail's key for them.
    on_key("l", |_| commands::edit_labels());
}

/// X, as Gmail's: checks the selected message, or unchecks it.
fn toggle_selected() {
    if let Some(RowId::Message(id)) = global::<Selected>().with_untracked(|s| s.0) {
        toggle_checked(id);
    }
}

pub fn list_column() -> impl IntoElement {
    let list = element_ref();
    global::<ListRef>().set(ListRef(Some(list)));
    form::keys_without_dialog(|| {
        on_key("x", move |_| toggle_selected());
        on_key("enter", move |_| edit_selected_draft());
        bind_command_keys();
    });
    follow_the_calendar();
    follow_selection(list);
    view().flex_1().min_w(px(0.0)).child(header()).child(
        virtual_list(move || global::<Listing>().with(Listing::len), item)
            .content_padding_x(px(12.0))
            .keys(move || global::<Listing>().with(Listing::keys))
            .on_load_more(load_more)
            .empty(
                view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p(px(24.0))
                    .child(
                        text(move || {
                            // Nothing while the first page is on its way.
                            if global::<Loading>().get().0 {
                                String::new()
                            } else {
                                global::<Listing>()
                                    .with(|l| list::empty_text(l.source(), l.filter()))
                                    .to_owned()
                            }
                        })
                        .text_sm()
                        .text_color(move || theme().muted_foreground),
                    ),
            )
            .estimated_item_size(px(48.0))
            .scrollbar(motion::indicators_shown)
            .element_ref(&list)
            .test_id("list")
            .flex_1(),
    )
}

/// How near the viewport's bottom a row keeps its bottom rather than its top
/// ([`follow_selection`]).
const ROOM_TO_GROW: f32 = 40.0;

/// Lays the panes out for the selection, keeping the row opened (or closed) where it shows
/// on the list though every row changes its height.
///
/// The list changes at once, not gliding: its rows in their new shape and a scroll that
/// puts the row back where it was, in one commit, so nothing on the list moves but the
/// rows around it. Opening, the list first, then (a frame later) the right pane sliding
/// in over the room the list left it; closing, the pane sliding out first, then (once it
/// is out) the list: the list widening while the pane slides would run under it. Not the
/// rows gliding with a scroll: nsh moves them along with the scrolls it makes itself
/// (`list_motion.rs`), not with a scroll command, which either jumps them under the glide
/// or (animated, on macOS) runs on a curve of its own, moved along again by the list's own
/// scroll in the same batch, so the row wandered before it settled whatever was tried.
///
/// The row's place is measured first (a view in it carries [`AnchorRef`], laid out a frame
/// after it is made): its top stays there, or, near the viewport's bottom, its bottom, so
/// that it grows upwards and stays in sight. A row partly out of sight comes into it; one
/// opened with no view (far down the list) comes to the middle of the viewport.
fn follow_selection(list: ElementRef) {
    let anchor = element_ref();
    global::<AnchorRef>().set(AnchorRef(Some(anchor)));
    let last_selected = stored(None::<RowId>);
    // Which toggle is under way: one toggled back before it lands drops it.
    let toggles = stored(0_u64);
    effect(move || {
        let selected = global::<Selected>().with(|s| s.0);
        let before = last_selected.replace(selected);
        let open = selected.is_some();
        if open == before.is_some() {
            return;
        }
        // One of them is: the row opened, or the one closed.
        let Some(row) = selected.or(before) else {
            return;
        };
        let toggle = toggles.get().wrapping_add(1);
        toggles.replace(toggle);
        let current = move || toggles.get() == toggle;
        // From the toggle until the commit that switches the list has gone.
        motion::still_rows(true);
        global::<Anchored>().set(Anchored(Some(row)));
        // The view's, not this effect's: the effect runs again at each selection (a second
        // click, Escape pressed twice), which would cancel the switch halfway, leaving the
        // pane out or the list narrow. A toggle back drops it instead (`current`).
        in_view(async move {
            if open {
                // The view in the row, made with this turn's commit, is laid out in the
                // next.
                next_frame().await;
            } else {
                // The pane out first, the list keeping its shape beside it.
                global::<PaneOpen>().set(PaneOpen(false));
                motion::glide_over().await;
            }
            // Both asked for at once: answered from the same commit's layout.
            let (at, of) = (anchor.measure(), list.measure());
            let (at, of) = (at.await, of.await);
            if !current() {
                return;
            }
            let ix = global::<Listing>().with_untracked(|l| l.line_of(row));
            global::<ListOpen>().set(ListOpen(open));
            let place = ix.and_then(|ix| {
                let (position, offset) = match (at, of) {
                    (Some(at), Some(of)) => {
                        // The cell's, around the row's margins (`rows::row_box`).
                        let top = at.page.y.0 - ROW_MARGIN_Y - of.page.y.0;
                        let bottom = top + at.frame.size.height.0 + 2.0 * ROW_MARGIN_Y;
                        let viewport = of.frame.size.height.0;
                        // Its top where it was, or its bottom near the viewport's: a row
                        // there grows upwards, staying in sight (a subject on two lines
                        // adds 34 points).
                        if bottom + ROOM_TO_GROW <= viewport {
                            (0.0, top.max(0.0))
                        } else {
                            (1.0, (bottom - viewport).min(0.0))
                        }
                    }
                    // Not made, far from the viewport (a notification's message, opened
                    // where it is listed): into the middle of it. Closing, the list stays
                    // where the user scrolled it.
                    _ if open => (0.5, 0.0),
                    _ => return None,
                };
                Some(Place {
                    index: ix as u32,
                    position,
                    offset,
                })
            });
            if let Some(place) = place {
                place.scroll(list);
            }
            // The switch's commit gone, rows read again may glide.
            next_frame().await;
            if !current() {
                return;
            }
            motion::still_rows(false);
            if let Some(place) = place {
                hold(list, place).await;
            }
            if !current() {
                return;
            }
            if open {
                // Then the pane in, beside the list in its new shape.
                global::<PaneOpen>().set(PaneOpen(true));
            }
        });
    });
}

/// Where [`follow_selection`] puts the row it keeps in place: `scroll_to_index`'s
/// arguments.
#[derive(Clone, Copy)]
struct Place {
    index: u32,
    position: f32,
    offset: f32,
}

impl Place {
    fn scroll(self, list: ElementRef) {
        list.scroll_to_index(
            self.index,
            Percentage(self.position),
            px(self.offset),
            false,
        );
    }
}

/// Scrolls the list to `place` again for the two frames after [`follow_selection`] did,
/// before the pane slides in: nsh 0.7 makes a scroll to a row a plain offset with the
/// sizes it knows in that commit, and on macOS the row can still end up elsewhere a frame
/// later: a row opened near the bottom, the list scrolled a little first, showed 124 to
/// 180 points higher, the list reporting the scroll position it was sent to. Each scroll
/// sends the offset again, from the sizes known then. Nothing moves when it is there.
///
/// ⚠️ At the list's start, the start again as a plain offset. A scroll to a row that comes
/// to rest there (clamped, closing near the top) leaves nsh following it a frame on from a
/// stale offset: the core sends a `ScrollBy` (-144 points, measured) that the platform,
/// already at the start, applies past it (macOS doesn't clamp a `ScrollBy`), then puts
/// itself back at the start without telling the platform, which stays scrolled before the
/// first row: a blank band above it. The plain offset ends the core's following. Until
/// longbridge/nsh#3235 is fixed.
async fn hold(list: ElementRef, place: Place) {
    for _ in 0..2 {
        place.scroll(list);
        // Answered from the commit the scroll lands in.
        if let Some(at) = list.measure_scroll_position().await
            && at.index == 0
        {
            list.scroll_to_offset(px(at.offset.0.max(0.0)), false);
        }
        next_frame().await;
    }
}

/// Puts the rows under the days of the user's zone again when it changes, and when a day
/// is over there: what was Today is Yesterday.
fn follow_the_calendar() {
    effect(move || {
        let calendar = clock::calendar();
        if !global::<Listing>().with_untracked(|l| l.grouped_by(&calendar)) {
            global::<Listing>().update(|l| l.regroup(&calendar));
        }
    });
}

fn sync_button() -> impl IntoElement {
    // Only when it starts or stops: the accounts are read again many times meanwhile, and
    // starting the spinner again each time would jerk it back.
    let syncing = computed(accounts::syncing);
    crate::buttons::busy_icon_button(
        || Icon::ArrowPath,
        move || {
            if syncing.get() {
                "Syncing".to_owned()
            } else {
                "Sync now".to_owned()
            }
        },
        accounts::has_accounts,
        move || syncing.get(),
        Size::Bar,
        accounts::sync,
    )
}

fn header() -> impl IntoElement {
    let title = computed(move || {
        let source = global::<Listing>().with(|l| l.source().cloned());
        let place = global::<SelectedPlace>().with(|p| p.0);
        global::<Accounts>().with(|a| {
            let accounts = a.list();
            (
                mailbox::list_title(source.as_ref(), place, accounts),
                mailbox::list_note(source.as_ref(), place, accounts),
            )
        })
    });
    let lists_messages =
        move || global::<Listing>().with(|l| l.source().is_some_and(Source::lists_messages));
    let search = element_ref();
    global::<SearchRef>().set(SearchRef(Some(search)));
    view()
        .px(px(12.0))
        .pb(px(8.0))
        .gap(px(4.0))
        .child(
            // As tall as the toolbar across from it, and lined up with it: the row that
            // moves the window, on macOS.
            chrome::move_row()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(sync_button())
                .child(search_field(search)),
        )
        .child(
            // As tall as the commands that take the note's place, so checking a message
            // doesn't move the rows. Lined up with the sync button's icon above and the
            // days' headings below.
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .h(px(Size::Bar.box_size()))
                .pl(px(8.0))
                .pr(px(4.0))
                .child(when(lists_messages, check_all))
                .child(
                    text(move || title.with(|t| t.0.clone()))
                        .number_of_lines(1)
                        .flex_shrink(1.0)
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(move || theme().foreground),
                )
                .child(dynamic(move || {
                    // The commands on the messages checked take the note's place.
                    if has_checked() {
                        commands::buttons(checked_messages, Size::Bar).into_element()
                    } else {
                        text(move || title.with(|t| t.1.clone().unwrap_or_default()))
                            .text_xs()
                            .text_color(move || theme().muted_foreground)
                            .into_element()
                    }
                })),
        )
}

/// The search field, round as "Compose" across from it.
fn search_field(search: ElementRef) -> impl IntoElement {
    view()
        .flex_1()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(36.0))
        .px(px(12.0))
        .rounded(px(18.0))
        .border_1()
        .border_color(move || theme().input)
        .child(icon(|| Icon::MagnifyingGlass, 16.0).text_color(move || theme().muted_foreground))
        .child(
            input()
                .value(move || global::<SearchText>().with(|t| t.0.clone()))
                .label("Search")
                .placeholder("Search".to_owned())
                .on_change(move |text: &str| typed(text.to_owned()))
                .on_focus(connect_ahead)
                .element_ref(&search)
                .flex_1()
                .text_sm()
                .text_color(move || theme().foreground),
        )
        // As AppKit's search field: only with something typed.
        .child(when(
            move || global::<SearchText>().with(|t| !t.0.is_empty()),
            move || {
                view()
                    .flex_shrink_0()
                    .mr(px(-4.0))
                    .p(px(2.0))
                    .rounded(px(10.0))
                    .text_color(move || theme().muted_foreground)
                    .hover(|s| s.text_color(move || theme().foreground))
                    .label("Clear search")
                    .on_press(move || clear_search_field(search))
                    .child(icon(|| Icon::XCircleSolid, 16.0))
            },
        ))
}

fn has_checked() -> bool {
    global::<Checked>().with(|c| !c.0.is_empty())
}

/// Checks every message listed, or none; beside it, the menu of the list's views: only
/// those read, flagged and so on.
fn check_all() -> impl IntoElement {
    let counts = computed(move || {
        let listed = global::<Listing>().with(|l| l.messages().count());
        let checked = global::<Checked>().with(|c| c.0.len());
        (listed, checked)
    });
    let all = move || counts.with(|&(listed, checked)| checked > 0 && checked == listed);
    let shown = computed(move || global::<Listing>().with(Listing::filter));
    let filtered = move || shown.get() != FlagFilter::All;
    view()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .child(
            view()
                .w(px(16.0))
                .h(px(16.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .border_1()
                .items_center()
                .justify_center()
                .border_color(move || {
                    if all() {
                        theme().primary
                    } else {
                        theme().input
                    }
                })
                .bg(move || {
                    if all() {
                        theme().primary
                    } else {
                        AdaptiveColor::TRANSPARENT
                    }
                })
                .label("Select all")
                .on_press(move || {
                    if all() {
                        global::<Checked>().set(Checked::default());
                    } else {
                        // Those the filter shows, of the pages read.
                        let listed = global::<Listing>()
                            .with_untracked(|l| l.messages().map(|m| m.id).collect());
                        global::<Checked>().set(Checked(listed));
                    }
                })
                .child(when(all, || {
                    icon(|| Icon::Check, 12.0).text_color(move || theme().primary_foreground)
                })),
        )
        .child(
            // A funnel, filled and named while it filters.
            view()
                .flex_row()
                .items_center()
                .flex_shrink_0()
                .gap(px(4.0))
                .h(px(24.0))
                .px(px(4.0))
                .rounded(px(6.0))
                .hover(|s| s.bg(move || theme().accent))
                .label("Filter")
                .tooltip(crate::buttons::tooltip(|| "Filter".to_owned()))
                .on_press(move || {
                    let current = shown.get_untracked();
                    let items = list::FILTERS
                        .into_iter()
                        .map(|which| {
                            MenuItem::new(list::filter_label(which))
                                .checked(which == current)
                                .on_select(move || {
                                    if which != current {
                                        filter(which);
                                    }
                                })
                        })
                        .collect();
                    // Nothing waits on it closing.
                    drop(popup_menu(items));
                })
                .child(
                    icon(
                        move || {
                            if filtered() {
                                Icon::FunnelSolid
                            } else {
                                Icon::Funnel
                            }
                        },
                        15.0,
                    )
                    .text_color(move || {
                        if filtered() {
                            theme().primary
                        } else {
                            theme().muted_foreground
                        }
                    }),
                )
                .child(when(filtered, move || {
                    text(move || list::filter_label(shown.get()).to_owned())
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme().primary)
                })),
        )
}
