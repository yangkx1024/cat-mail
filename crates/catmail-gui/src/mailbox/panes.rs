//! The mailbox's panes side by side: the sidebar, the list, and the pane for what the
//! list has selected (a message, a draft or an outbox entry), with handles between them
//! to drag their widths.

use catmail_frontend::mailbox::list::RowId;
use nsh_base::{HandleEdge, resize_handle, theme};
use nsh_core::prelude::*;

use super::{draft, outbox, reader};
use crate::mailbox::list;
use crate::mailbox::listing::Selected;
use crate::mailbox::picker::picker;
use crate::mailbox::sidebar;
use crate::motion;

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Reader,
    Draft,
    Outbox,
}

/// How wide the sidebar and the list are, as the user dragged them, for as long as the window
/// is open.
#[derive(Clone, Copy, PartialEq)]
struct PaneWidths {
    sidebar: f32,
    list: f32,
}

impl Default for PaneWidths {
    fn default() -> Self {
        Self {
            sidebar: 240.0,
            list: 368.0,
        }
    }
}

/// The sidebar's range, and the list's while a pane is open beside it (15, 12 to 24 and
/// 23, 16 to 40 rem).
const SIDEBAR_WIDTHS: (f32, f32) = (192.0, 384.0);
const LIST_WIDTHS: (f32, f32) = (256.0, 640.0);

/// The handle on a pane's trailing edge, which drags the width `field` picks within
/// `range`. Each move goes from the width when the drag began, so a drag whose end goes
/// missing leaves nothing behind: the next starts from what is shown.
fn edge_handle(field: fn(&mut PaneWidths) -> &mut f32, range: (f32, f32)) -> impl IntoElement {
    let widths = global::<PaneWidths>();
    let from = stored(0.0_f32);
    resize_handle()
        .edge(HandleEdge::Trailing)
        // A grip on the line while the pointer is there, longer and darker while dragged.
        .thumb_class(
            "h-5 w-[3px] rounded-full bg-muted-foreground opacity-0 \
             group-hover:opacity-35 group-active:h-11 group-active:opacity-90 transition",
        )
        .on_drag_start(move || {
            from.replace(*field(&mut widths.get_untracked()));
        })
        .on_drag_move(move |by: &Points| {
            let mut next = widths.get();
            *field(&mut next) = (from.get() + by.0).clamp(range.0, range.1);
            // `set` skips an unchanged width: one held at its bound restyles nothing.
            widths.set(next);
        })
}

pub fn panes() -> impl IntoElement {
    // By the kind of row alone: going from one message to the next doesn't make the panes
    // again.
    let pane = computed(|| {
        global::<Selected>().with(|s| {
            s.0.map(|row| match row {
                RowId::Message(_) => Pane::Reader,
                RowId::Draft(_) => Pane::Draft,
                RowId::Outbox(_) => Pane::Outbox,
            })
        })
    });
    // Whether the right pane is in, and whether the list is laid out beside it: a row
    // selected changes the list first, then the pane (`list::follow_selection`).
    let open = list::pane_open;
    let list_open = list::list_open;
    // The pane last open: it stays while it slides out.
    let last = stored(None::<Pane>);
    let shown = computed(move || match pane.get() {
        Some(now) => {
            last.replace(Some(now));
            Some(now)
        }
        None => last.get(),
    });
    let widths = global::<PaneWidths>();
    // Apart, so a drag of one edge restyles only what that width places.
    let sidebar_width = computed(move || widths.with(|w| w.sidebar));
    let list_width = computed(move || widths.with(|w| w.list));
    let sidebar_width = move || sidebar_width.get();
    let list_width = move || list_width.get();
    // How wide the right pane was when last open: closed, it keeps that width, so resizing
    // the window or dragging an edge doesn't make its web view lay out a page no one sees.
    // Nothing before it first opens, when the web view is empty: it follows the window.
    let frozen = signal(None::<f32>);
    let measure = move |frame: &Bounds<Points>| {
        if untracked(open) && frozen.get_untracked() != Some(frame.size.width.0) {
            frozen.set(Some(frame.size.width.0));
        }
    };
    // How wide all the panes are, for placing the right pane past their trailing edge.
    let total = signal(None::<f32>);
    let measure_total = move |frame: &Bounds<Points>| total.set(Some(frame.size.width.0));
    let opened_left = move || sidebar_width() + list_width();
    // Where the right pane is, open or not: closed, just past the window's trailing edge,
    // as wide as it was when last open (as wide as it would be open, before it first
    // opens), so opening slides it in from the right, over the room the list left it a
    // frame before, and closing slides it back out, the list widening only once it is
    // out (`list::follow_selection`). Moved, never resized: its web view would lay its page
    // out again at every width it passed (it does once, if the window changed while it
    // was closed). Until the panes are first measured, where it goes when open. One memo
    // for both panes, tracking only what the closed place depends on.
    let placement = computed(move || {
        if open() {
            return None;
        }
        total.get().map(|total| {
            let width = frozen
                .get()
                .unwrap_or_else(|| (total - opened_left()).max(0.0));
            (total, width)
        })
    });
    let right_pane = move |pane: View| {
        pane.absolute()
            .top(px(0.0))
            .bottom(px(0.0))
            .style_with(move |s| match placement.get() {
                Some((left, width)) => s.left(px(left)).w(px(width)),
                None => s.left(px(opened_left())).right(px(0.0)),
            })
            .accessibility_elements_hidden(move || !open())
    };
    motion::glide_when(open);
    view()
        .flex_1()
        .flex_row()
        .on_layout(measure_total)
        // Always there, and out of sight while no message is selected: its web view keeps
        // its web process (100 MB, and a moment to start), so the first message opened
        // shows as quickly as the next. Past the window's trailing edge then, rather than
        // not drawn (`invisible`): a web view that isn't drawn for a while shows a black
        // frame when it is drawn again, before WebKit paints. Not left out of the layout
        // either: nsh shows a native view only with a size, and WebKit loads a page only
        // into one shown. One view whatever pane shows; first, so everything else is
        // drawn over it.
        .child(right_pane(reader::reader()).on_layout(measure))
        // The draft's and the outbox entry's, over the reader.
        .child(
            right_pane(view().bg(move || theme().background).child(dynamic(
                move || match shown.get() {
                    Some(Pane::Draft) => draft::pane().into_element(),
                    Some(Pane::Outbox) => outbox::pane().into_element(),
                    Some(Pane::Reader) | None => view().into_element(),
                },
            )))
            .style_when(
                move || !matches!(shown.get(), Some(Pane::Draft | Pane::Outbox)),
                |s| s.hidden(),
            ),
        )
        .child(
            view()
                .relative()
                .w(move || px(sidebar_width()))
                .flex_shrink_0()
                .border_r_1()
                .border_color(move || theme().border)
                .child(sidebar::sidebar())
                .child(edge_handle(|w| &mut w.sidebar, SIDEBAR_WIDTHS)),
        )
        // The window's width to the list until a row opens; then as wide as it was.
        .child(
            view()
                .relative()
                .flex_row()
                .w(move || {
                    if list_open() {
                        px(list_width()).into()
                    } else {
                        Length::Auto
                    }
                })
                .flex_grow(move || if list_open() { 0.0_f32 } else { 1.0 })
                .flex_shrink_0()
                .bg(move || theme().background)
                .style_when(list_open, |s| {
                    s.border_r_1().border_color(move || theme().border)
                })
                .child(list::list_column())
                .child(when(list_open, || {
                    edge_handle(|w| &mut w.list, LIST_WIDTHS)
                })),
        )
        .child(picker())
}
