//! The tray icon where the count is drawn on it: the app's icon, in the desktop's
//! appearance, with a red badge in the corner.

use std::borrow::Cow;

/// The sizes it is drawn at; the host picks what fits its panel.
pub const SIZES: [u32; 3] = [22, 32, 64];

const LIGHT: &str = include_str!("../../assets/app-icon/catmail-light.svg");
const DARK: &str = include_str!("../../assets/app-icon/catmail-dark.svg");

/// The app's icon with the badge on top, as one SVG; the icon keeps its own canvas
/// (512 × 512) as a nested `<svg>`.
pub fn svg(dark: bool, unread: u32) -> Cow<'static, str> {
    let icon = if dark { DARK } else { LIGHT };
    let Some(text) = super::badge(unread) else {
        return Cow::Borrowed(icon);
    };
    // Big enough to read at a panel's 22 pixels; wider for each digit past the first.
    let (height, size) = (260, 210);
    let width = height + (text.chars().count() as u32 - 1) * 110;
    let (x, radius) = (512 - width, height / 2);
    Cow::Owned(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
{icon}
<rect x="{x}" y="0" width="{width}" height="{height}" rx="{radius}" fill="#d93025"/>
<text x="{cx}" y="{radius}" font-family="sans-serif" font-weight="bold" font-size="{size}" fill="#ffffff" text-anchor="middle" dominant-baseline="central">{text}</text>
</svg>"##,
        cx = x + width / 2,
    ))
}
