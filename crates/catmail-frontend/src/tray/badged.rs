//! The tray icon where the count is drawn on it: the app's icon, in the desktop's
//! appearance, with a red badge in the corner.

use std::borrow::Cow;

/// The sizes it is drawn at; the host picks what fits its panel.
pub const SIZES: [u32; 3] = [22, 32, 64];

/// `icon` (the app's, a 512 × 512 SVG) with the badge for `unread` on top, as one SVG;
/// the icon keeps its own canvas as a nested `<svg>`.
pub fn svg(icon: &'static str, unread: u32) -> Cow<'static, str> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512"/>"#;

    #[test]
    fn no_unread_mail_is_the_icon_alone() {
        assert_eq!(svg(ICON, 0), ICON);
    }

    #[test]
    fn the_badge_widens_with_the_count() {
        let one = svg(ICON, 7);
        assert!(one.contains(ICON) && one.contains(">7</text>"), "{one}");
        assert!(one.contains(r#"width="260""#), "{one}");
        let many = svg(ICON, 120);
        assert!(many.contains(">99+</text>"), "{many}");
        assert!(many.contains(r#"width="480""#), "{many}");
    }
}
