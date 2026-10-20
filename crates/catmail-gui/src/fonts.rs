//! The UI font. On Linux GPUI picks a weight only among a family's separate faces and
//! draws a variable font (one face holding every weight, as distributions now ship Noto
//! Sans) at its default weight, so bold text came out regular. The app therefore brings
//! static Noto Sans faces (SIL OFL, `assets/fonts/noto-sans/`) and names them on every
//! theme it applies. Elsewhere the system font has its weights, and is used as is.
//!
//! Noto Sans has no CJK. Left to itself, GPUI's Linux backend takes the first font that
//! has the glyph, and on many systems that is the variable Noto Sans CJK: one weight, and
//! not a regular-looking one. So the window names CJK families to fall back to, matched
//! by weight like the UI font, keeping only those installed with separate faces for
//! regular and bold. Only bundling CJK fonts would make this work everywhere, at tens of
//! megabytes; where none qualifies, CJK text keeps GPUI's choice.

use gpui_kit::App;
use gpui_kit::component::ThemeConfig;

/// The family every theme is drawn in, or `None` for the theme's own (the system's).
#[cfg(target_os = "linux")]
const UI_FONT: Option<&str> = Some("Noto Sans");
#[cfg(not(target_os = "linux"))]
const UI_FONT: Option<&str> = None;

/// Makes the bundled faces available and the CJK fallbacks apply to every window. Before
/// any window opens.
pub fn init(cx: &mut App) {
    #[cfg(target_os = "linux")]
    linux::init(cx);
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
}

/// `config`, drawn in the UI font. Applying a theme sets the font it names, so the
/// font is named on each one rather than once on the global theme.
pub fn with_ui_font(config: &ThemeConfig) -> ThemeConfig {
    let mut config = config.clone();
    if let Some(family) = UI_FONT {
        config.font_family = Some(family.into());
    }
    config
}

#[cfg(target_os = "linux")]
mod linux {
    use std::borrow::Cow;

    use gpui_kit::base::{Root, RootPlugin};
    use gpui_kit::{
        App, Context, Div, Empty, Font, FontFallbacks, FontWeight, Global, IntoElement, Render,
        Stateful, Styled as _, Window, font,
    };

    /// CJK families that are often installed in separate weights, the common ones first.
    const CJK_CANDIDATES: &[&str] = &[
        "Noto Sans CJK SC",
        "Source Han Sans SC",
        "MiSans",
        "HarmonyOS Sans SC",
        "Sarasa UI SC",
        "Noto Sans CJK TC",
        "Noto Sans CJK JP",
        "Noto Sans CJK KR",
        "WenQuanYi Micro Hei",
    ];

    pub fn init(cx: &mut App) {
        let faces = vec![
            Cow::Borrowed(&include_bytes!("../assets/fonts/noto-sans/NotoSans-Regular.ttf")[..]),
            Cow::Borrowed(&include_bytes!("../assets/fonts/noto-sans/NotoSans-Medium.ttf")[..]),
            Cow::Borrowed(&include_bytes!("../assets/fonts/noto-sans/NotoSans-SemiBold.ttf")[..]),
            Cow::Borrowed(&include_bytes!("../assets/fonts/noto-sans/NotoSans-Bold.ttf")[..]),
        ];
        // Without them the text is still drawn, in the system's font.
        if let Err(e) = cx.text_system().add_fonts(faces) {
            tracing::warn!(error = %e, "couldn't load the bundled UI font");
        }
        let installed = cx.text_system().all_font_names();
        let families: Vec<String> = CJK_CANDIDATES
            .iter()
            .filter(|family| installed.iter().any(|name| name == *family) && has_bold(family, cx))
            .map(|family| family.to_string())
            .collect();
        tracing::debug!(?families, "CJK fallback fonts");
        cx.set_global(Fallbacks(FontFallbacks::from_fonts(families)));
        Root::register_plugin::<Fallbacks>(cx, |_, cx| cx.global::<Fallbacks>().clone());
    }

    /// Whether the installed `family` has a bold face apart from its regular one. A
    /// variable font is one face, which GPUI draws at a single weight.
    fn has_bold(family: &str, cx: &App) -> bool {
        let text = cx.text_system();
        let regular = font(family);
        let bold = Font {
            weight: FontWeight::BOLD,
            ..regular.clone()
        };
        text.resolve_font(&regular) != text.resolve_font(&bold)
    }

    /// The CJK fallbacks, set on each window's root, which dialogs and notifications are
    /// drawn in too. As a plugin it draws nothing itself.
    #[derive(Clone)]
    struct Fallbacks(FontFallbacks);

    impl Global for Fallbacks {}

    impl Render for Fallbacks {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Empty
        }
    }

    impl RootPlugin for Fallbacks {
        fn style(&self, surface: &mut Stateful<Div>, _: &mut Window, _: &mut App) {
            surface.text_style().font_fallbacks = Some(self.0.clone());
        }
    }
}
