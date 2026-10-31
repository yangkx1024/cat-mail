//! The UI font. On Linux GPUI picks a weight only among a family's separate faces and
//! draws a variable font (one face holding every weight, as distributions now ship Noto
//! Sans) at its default weight, so bold text came out regular. The app therefore brings
//! static Noto Sans faces (SIL OFL, `assets/fonts/noto-sans/`) and names them on every
//! theme it applies. Elsewhere the system font has its weights, and is used as is.
//!
//! Noto Sans has no CJK, and GPUI's own choice for it is often the variable Noto Sans CJK,
//! drawn thin. So the window names CJK families to fall back to: Noto Sans SC when a
//! package installed it with the app, then installed families with separate regular and
//! bold faces. A fallback is used only at the exact weight of the UI font's face, which
//! is why the installed Noto Sans SC is loaded at each of those weights. The details are
//! in docs/architecture/gui.md, "字体（Linux）".

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
    use std::ffi::OsString;
    use std::path::PathBuf;

    use directories::BaseDirs;
    use gpui_kit::base::{Root, RootPlugin};
    use gpui_kit::{
        App, Context, Div, Empty, Font, FontFallbacks, FontWeight, Global, IntoElement, Render,
        Stateful, Styled as _, Window, font,
    };

    /// The CJK family a package can install with the app, in `catmail/fonts` of a data
    /// directory: at 16 MB, it is not compiled in.
    const BUNDLED_CJK: &str = "Noto Sans SC";
    const BUNDLED_CJK_FILES: &[&str] = &["NotoSansSC-Regular.otf", "NotoSansSC-Bold.otf"];

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
        let ui: [&'static [u8]; 4] = [
            include_bytes!("../assets/fonts/noto-sans/NotoSans-Regular.ttf"),
            include_bytes!("../assets/fonts/noto-sans/NotoSans-Medium.ttf"),
            include_bytes!("../assets/fonts/noto-sans/NotoSans-SemiBold.ttf"),
            include_bytes!("../assets/fonts/noto-sans/NotoSans-Bold.ttf"),
        ];
        let weights: Vec<u16> = ui.iter().filter_map(|face| weight_class(face)).collect();
        let dirs = data_dirs(
            BaseDirs::new().map(|dirs| dirs.data_dir().to_owned()),
            std::env::var_os("XDG_DATA_DIRS"),
        );
        let cjk = bundled_cjk(&weights, dirs);
        let bundled = !cjk.is_empty();
        let faces = ui
            .into_iter()
            .map(Cow::Borrowed)
            .chain(cjk.into_iter().map(Cow::Owned))
            .collect();
        // Without them the text is still drawn, in the system's font.
        if let Err(e) = cx.text_system().add_fonts(faces) {
            tracing::warn!(error = %e, "couldn't load the bundled fonts");
        }
        let installed = cx.text_system().all_font_names();
        let families: Vec<String> = bundled
            .then_some(BUNDLED_CJK)
            .into_iter()
            .chain(CJK_CANDIDATES.iter().copied().filter(|family| {
                installed.iter().any(|name| name == family) && has_bold(family, cx)
            }))
            .map(str::to_owned)
            .collect();
        tracing::debug!(?families, "CJK fallback fonts");
        cx.set_global(Fallbacks(FontFallbacks::from_fonts(families)));
        Root::register_plugin::<Fallbacks>(cx, |_, cx| cx.global::<Fallbacks>().clone());
    }

    /// The XDG data directories, the user's (`data_home`) first, then `XDG_DATA_DIRS`.
    /// Relative paths are ignored, as the specification says.
    pub(super) fn data_dirs(
        data_home: Option<PathBuf>,
        data_dirs: Option<OsString>,
    ) -> Vec<PathBuf> {
        let data_dirs = data_dirs
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        data_home
            .into_iter()
            .chain(std::env::split_paths(&data_dirs))
            .filter(|dir| dir.is_absolute())
            .collect()
    }

    /// The faces of [`BUNDLED_CJK`] at `weights` (see [`at_weights`]), from the first of
    /// `dirs` that has them all; none if none has.
    fn bundled_cjk(weights: &[u16], dirs: Vec<PathBuf>) -> Vec<Vec<u8>> {
        for dir in dirs {
            let dir = dir.join("catmail/fonts");
            let paths: Vec<_> = BUNDLED_CJK_FILES
                .iter()
                .map(|file| dir.join(file))
                .collect();
            if !paths.iter().all(|path| path.is_file()) {
                continue;
            }
            let fonts = match paths.iter().map(std::fs::read).collect() {
                Ok(fonts) => fonts,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "couldn't read the CJK fonts");
                    continue;
                }
            };
            match at_weights(fonts, weights) {
                Some(faces) => return faces,
                None => tracing::warn!(dir = %dir.display(), "the CJK fonts aren't OpenType fonts"),
            }
        }
        tracing::debug!("no CJK fonts installed with the app");
        Vec::new()
    }

    /// `fonts`, faces of one family, loaded once at each of `weights` (the UI font's), the
    /// nearest face at each: a fallback is used only at the exact weight of the UI font's
    /// face, and the family may have none there (Noto Sans SC has no 500 or 600). A face
    /// is copied for all but the last weight it stands for. `None` if one isn't a font.
    pub(super) fn at_weights(fonts: Vec<Vec<u8>>, weights: &[u16]) -> Option<Vec<Vec<u8>>> {
        let own: Vec<u16> = fonts
            .iter()
            .map(|font| weight_class(font))
            .collect::<Option<_>>()?;
        let nearest = |weight: u16| (0..own.len()).min_by_key(|&ix| own[ix].abs_diff(weight));
        let mut faces = Vec::new();
        for (ix, mut font) in fonts.into_iter().enumerate() {
            let mine: Vec<u16> = weights
                .iter()
                .copied()
                .filter(|&weight| nearest(weight) == Some(ix))
                .collect();
            let Some((&last, rest)) = mine.split_last() else {
                continue;
            };
            for &weight in rest {
                let mut copy = font.clone();
                set_weight(&mut copy, weight)?;
                faces.push(copy);
            }
            set_weight(&mut font, last)?;
            faces.push(font);
        }
        Some(faces)
    }

    /// The weight class of `font`'s OS/2 table, which is the weight fontdb and cosmic-text
    /// go by.
    pub(super) fn weight_class(font: &[u8]) -> Option<u16> {
        let at = weight_class_at(font)?;
        Some(u16::from_be_bytes([font[at], font[at + 1]]))
    }

    /// Sets the weight class of `font`'s OS/2 table. The table's checksum goes stale:
    /// nothing that reads the font checks it.
    pub(super) fn set_weight(font: &mut [u8], weight: u16) -> Option<()> {
        let at = weight_class_at(font)?;
        font[at..at + 2].copy_from_slice(&weight.to_be_bytes());
        Some(())
    }

    /// Where the OS/2 weight class is in `font`, taken to be a single font (not a
    /// collection).
    fn weight_class_at(font: &[u8]) -> Option<usize> {
        let be16 = |at: usize| Some(u16::from_be_bytes(font.get(at..at + 2)?.try_into().ok()?));
        let be32 = |at: usize| Some(u32::from_be_bytes(font.get(at..at + 4)?.try_into().ok()?));
        // The table records follow the 12-byte header, 16 bytes each: tag, checksum,
        // offset, length. usWeightClass is the third field of OS/2.
        let record = (0..usize::from(be16(4)?))
            .map(|ix| 12 + 16 * ix)
            .find(|&record| font.get(record..record + 4) == Some(b"OS/2"))?;
        let at = usize::try_from(be32(record + 8)?).ok()? + 4;
        (at + 2 <= font.len()).then_some(at)
    }

    /// Whether the installed `family` has a bold face apart from its regular one. A
    /// variable font is one face, which GPUI draws at a single weight. GPUI resolves to
    /// the nearest face here, so this says nothing of whether the faces' weights are the
    /// exact ones a fallback is matched at.
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::linux::*;

    const REGULAR: &[u8] = include_bytes!("../assets/fonts/noto-sans/NotoSans-Regular.ttf");

    #[test]
    fn a_face_takes_the_weight_it_is_given() {
        assert_eq!(weight_class(REGULAR), Some(400));
        let mut font = REGULAR.to_vec();
        set_weight(&mut font, 600).unwrap();
        assert_eq!(weight_class(&font), Some(600));
        assert_eq!(font.len(), REGULAR.len());
        assert_eq!(weight_class(b"not a font"), None);
        assert_eq!(set_weight(&mut b"not a font".to_vec(), 600), None);
    }

    #[test]
    fn each_weight_takes_the_nearest_face() {
        let mut bold = REGULAR.to_vec();
        set_weight(&mut bold, 700).unwrap();
        let faces = at_weights(vec![REGULAR.to_vec(), bold.clone()], &[400, 500, 600, 700]);
        let faces = faces.unwrap();
        let weights: Vec<_> = faces.iter().map(|face| weight_class(face)).collect();
        assert_eq!(weights, [Some(400), Some(500), Some(600), Some(700)]);
        // Medium is the regular face, semibold the bold one (they differ in nothing else
        // here but their weight).
        let mut medium = REGULAR.to_vec();
        set_weight(&mut medium, 500).unwrap();
        assert_eq!(faces[1], medium);
        set_weight(&mut bold, 600).unwrap();
        assert_eq!(faces[2], bold);
        assert_eq!(at_weights(vec![b"not a font".to_vec()], &[400]), None);
    }

    #[test]
    fn data_directories_are_the_users_then_the_systems() {
        let paths = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            data_dirs(Some("/home/a/.local/share".into()), None),
            paths(&["/home/a/.local/share", "/usr/local/share", "/usr/share"])
        );
        assert_eq!(
            data_dirs(
                Some("/data".into()),
                Some(OsString::from("/nix/fonts/share:relative:/usr/share"))
            ),
            paths(&["/data", "/nix/fonts/share", "/usr/share"])
        );
        // Set but empty is unset.
        assert_eq!(
            data_dirs(None, Some(OsString::new())),
            paths(&["/usr/local/share", "/usr/share"])
        );
    }
}
