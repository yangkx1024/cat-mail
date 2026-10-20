//! The Settings dialog: appearance (light or dark, the themes for each, importing a
//! terminal color scheme, text size), accounts, and reading.

mod accounts;
mod themes;

use catmail_core::MailClient;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::sidebar::{SidebarItem as _, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Icon, IndexPath, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, WeakEntity, Window, div, rems,
};

use crate::assets::Hero;
use crate::mailbox::Mailbox;
use crate::settings::{Appearance, Preferences, TEXT_SIZES};
use crate::theme::appearance::ImportCandidate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Appearance,
    Accounts,
    Reading,
}

impl Section {
    const ALL: [(Section, &'static str, Hero); 3] = [
        (Section::Appearance, "Appearance", Hero::Swatch),
        (Section::Accounts, "Accounts", Hero::UserCircle),
        (Section::Reading, "Reading", Hero::EnvelopeOpen),
    ];
}

/// An import waiting to be confirmed, or why it can't be.
enum Import {
    Ready(ImportCandidate),
    Failed { file_name: String, error: String },
}

pub struct SettingsView {
    client: MailClient,
    mailbox: WeakEntity<Mailbox>,
    section: Section,
    text_size: Entity<SelectState<Vec<SharedString>>>,
    import: Option<Import>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the Settings dialog over the mailbox.
pub fn open(client: MailClient, mailbox: WeakEntity<Mailbox>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| SettingsView::new(client, mailbox, window, cx));
    let width = rems(50.).to_pixels(window.rem_size());
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title("Settings").w(width).child(view.clone())
    });
}

impl SettingsView {
    fn new(
        client: MailClient,
        mailbox: WeakEntity<Mailbox>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let labels: Vec<SharedString> = TEXT_SIZES.iter().map(|(_, l)| (*l).into()).collect();
        let current = Preferences::get(cx).text_size;
        let selected = TEXT_SIZES.iter().position(|(size, _)| *size == current);
        let text_size =
            cx.new(|cx| SelectState::new(labels, selected.map(IndexPath::new), window, cx));
        let mut subscriptions = vec![cx.subscribe_in(
            &text_size,
            window,
            |_, _, event: &SelectEvent<Vec<SharedString>>, _, cx| {
                let SelectEvent::Confirm(Some(label)) = event else {
                    return;
                };
                if let Some((size, _)) = TEXT_SIZES.iter().find(|(_, l)| *l == label.as_ref()) {
                    let size = *size;
                    Preferences::update(cx, |s| s.text_size = size);
                }
            },
        )];
        // The accounts listed are the mailbox's, which follows the engine's events.
        if let Some(mailbox) = mailbox.upgrade() {
            subscriptions.push(cx.observe(&mailbox, |_, _, cx| cx.notify()));
        }
        Self {
            client,
            mailbox,
            section: Section::Appearance,
            text_size,
            import: None,
            _subscriptions: subscriptions,
        }
    }

    fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        Preferences::update(cx, |s| s.appearance = appearance);
        cx.notify();
    }

    fn choose_theme(&mut self, name: String, dark: bool, cx: &mut Context<Self>) {
        Preferences::update(cx, |s| {
            if dark {
                s.dark_theme = name;
            } else {
                s.light_theme = name;
            }
        });
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        // As GPUI Kit's own settings page draws its pages. The items are
        // `settings-sections-<index>`.
        let sections = SidebarMenu::new()
            .children(Section::ALL.iter().map(|&(section, label, icon)| {
                SidebarMenuItem::new(label)
                    .icon(Icon::new(icon))
                    .active(self.section == section)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    }))
            }))
            .render("settings-sections", window, cx);
        let nav = div()
            .flex_none()
            .w(rems(10.))
            .pr_3()
            .border_r_1()
            .border_color(border)
            .child(sections);
        let content = match self.section {
            Section::Appearance => self.render_appearance(cx),
            Section::Accounts => self.render_accounts(cx),
            Section::Reading => self.render_reading(cx),
        };
        h_flex().items_start().h(rems(32.)).child(nav).child(
            div()
                .id("settings-content")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .pl_5()
                .pr_1()
                .child(content),
        )
    }
}

fn section_title(title: &'static str, help: &'static str, cx: &App) -> impl IntoElement + use<> {
    v_flex()
        .gap_0p5()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(help),
        )
}

impl SettingsView {
    fn render_reading(&self, cx: &mut Context<Self>) -> AnyElement {
        let mark_read = Preferences::get(cx).mark_read_on_open;
        v_flex()
            .gap_4()
            .child(section_title(
                "Reading",
                "How messages behave as you read them.",
                cx,
            ))
            .child(
                Switch::new("mark-read-on-open")
                    .checked(mark_read)
                    .label("Mark messages as read when opened")
                    .on_change(cx.listener(|_, checked: &bool, _, cx| {
                        let checked = *checked;
                        Preferences::update(cx, |s| s.mark_read_on_open = checked);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
