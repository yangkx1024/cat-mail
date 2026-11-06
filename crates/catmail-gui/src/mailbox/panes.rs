//! The mailbox's panes side by side: the sidebar, the list, and the pane for what the
//! list has selected (a message, a draft or an outbox entry), with handles between them
//! to drag their widths.

use catmail_frontend::list::RowId;
use nsh_base::{HandleEdge, resize_handle, theme};
use nsh_core::prelude::*;

use super::{draft, outbox};
use crate::buttons::{Size, icon_button};
use crate::icons::Icon;
use crate::mailbox::list::{self, Selected};
use crate::mailbox::picker::picker;
use crate::mailbox::sidebar;
use crate::reader;

/// Which pane the row selected opens.
#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Reader,
    Draft,
    Outbox,
}

/// How wide the sidebar and the list are, as the user dragged them, for as long as the app
/// runs.
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
        // As GPUI Kit's: a grip on the line while the pointer is there.
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
    let open = move || pane.get().is_some();
    let reading = move || pane.get() == Some(Pane::Reader);
    let widths = global::<PaneWidths>();
    // Apart, so a drag of one edge restyles only what that width places.
    let sidebar_width = computed(move || widths.with(|w| w.sidebar));
    let list_width = computed(move || widths.with(|w| w.list));
    let sidebar_width = move || sidebar_width.get();
    let list_width = move || list_width.get();
    view()
        .flex_1()
        .flex_row()
        // Always there, and out of sight while no message is selected: its web view keeps
        // its web process (100 MB, and a moment to start), so the first message opened
        // shows as quickly as the next. Under the list then, which covers it, rather than
        // not drawn (`invisible`): a web view that isn't drawn for a while shows a black
        // frame when it is drawn again, before WebKit paints. Not left out of the layout
        // either: nsh shows a native view only with a size, and WebKit loads a page only
        // into one shown. Placed by the widths, so it stays one view whatever pane shows;
        // first, so everything else is drawn over it.
        .child(
            reader::reader()
                .absolute()
                .top(px(0.0))
                .bottom(px(0.0))
                .left(move || px(sidebar_width() + if reading() { list_width() } else { 0.0 }))
                .style_when(reading, |s| s.right(px(0.0)))
                // Under the list: as wide as the list can be narrow, so dragging the
                // list's edge doesn't resize a web view no one sees.
                .style_when(move || !reading(), |s| s.w(px(LIST_WIDTHS.0))),
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
                    if open() {
                        px(list_width()).into()
                    } else {
                        Length::Auto
                    }
                })
                .flex_grow(move || if open() { 0.0_f32 } else { 1.0 })
                .flex_shrink_0()
                // Opaque: the reader may be under it.
                .bg(move || theme().background)
                .style_when(open, |s| {
                    s.border_r_1().border_color(move || theme().border)
                })
                .child(list::list_column())
                .child(when(open, || edge_handle(|w| &mut w.list, LIST_WIDTHS))),
        )
        .child(dynamic(move || match pane.get() {
            Some(Pane::Reader) => view().into_element(),
            Some(Pane::Draft) => draft::pane().into_element(),
            Some(Pane::Outbox) => outbox::pane().into_element(),
            None => view().into_element(),
        }))
        .child(picker())
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

/// A fact of a draft or an outbox entry, its name in a column of its own: "To", the
/// addresses.
pub fn fact(name: &'static str, value: String) -> View {
    view()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .child(
            text(name)
                .w(px(64.0))
                .flex_shrink_0()
                .text_sm()
                .text_color(move || theme().muted_foreground),
        )
        .child(
            text(value)
                .flex_1()
                .text_sm()
                .selectable(true)
                .text_color(move || theme().foreground),
        )
}
