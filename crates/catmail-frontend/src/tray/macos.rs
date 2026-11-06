//! The status item on macOS: a template image in the menu bar, which the system tints
//! to suit the menu bar, with the unread count beside it and on the Dock icon. Clicking
//! it opens the menu, as status items do on macOS. AppKit's, and no UI toolkit's: the
//! frontend draws the image ([`Template`]) and runs it on the main thread.

use futures_channel::mpsc::UnboundedSender;
use objc2::rc::Retained;
use objc2::{
    AnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    sel,
};
use objc2_app_kit::{
    NSApplication, NSBitmapFormat, NSBitmapImageRep, NSCellImagePosition, NSDeviceRGBColorSpace,
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSSize, NSString, ns_string};

use super::{Chosen, MENU, MENU_BAR_SIZES, Template};

/// The image's size in points: the menu bar's icons are 18 points high, and the first
/// size is for 1x displays.
const POINTS: u32 = MENU_BAR_SIZES[0];

define_class!(
    /// Sends what is chosen in the menu to the app: each item's tag is its place in
    /// [`MENU`].
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CatmailStatusMenuTarget"]
    #[ivars = UnboundedSender<Chosen>]
    struct Target;

    impl Target {
        #[unsafe(method(choose:))]
        fn choose(&self, item: &NSMenuItem) {
            let entry = usize::try_from(item.tag()).ok().and_then(|i| MENU.get(i));
            if let Some(Some((_, command))) = entry {
                self.ivars().unbounded_send((*command, None)).ok();
            }
        }
    }
);

impl Target {
    fn new(chosen: UnboundedSender<Chosen>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(chosen);
        // SAFETY: NSObject's `init`, on an instance of a subclass that adds only ivars.
        unsafe { msg_send![super(this), init] }
    }
}

#[derive(Clone)]
pub struct Item {
    status: Retained<NSStatusItem>,
    /// What the menu's items send to, which they don't retain.
    _target: Retained<Target>,
}

impl Item {
    /// Puts the item in the menu bar, which macOS always has: what is chosen in its menu
    /// goes to `chosen`. On the main thread; `None` elsewhere.
    pub fn spawn(chosen: UnboundedSender<Chosen>) -> Option<Self> {
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::error!("no status item: not on the main thread");
            return None;
        };
        let target = Target::new(chosen, mtm);
        let menu = NSMenu::new(mtm);
        for (tag, entry) in MENU.iter().enumerate() {
            let item = match entry {
                Some((label, _)) => {
                    // SAFETY: `choose:` takes the menu item, as a menu action does.
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(label),
                            Some(sel!(choose:)),
                            ns_string!(""),
                        )
                    };
                    // SAFETY: `target` responds to `choose:`, and `Item` keeps it alive
                    // as long as the menu.
                    unsafe { item.setTarget(Some(&target)) };
                    item.setTag(tag as isize);
                    item
                }
                None => NSMenuItem::separatorItem(mtm),
            };
            menu.addItem(&item);
        }
        let status =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        status.setMenu(Some(&menu));
        if let Some(button) = status.button(mtm) {
            button.setImagePosition(NSCellImagePosition::ImageLeading);
        }
        Some(Self {
            status,
            _target: target,
        })
    }

    /// Shows the count, and `icon` (the template at each of its sizes) unless it is
    /// `None`: the one there looks the same. On the main thread.
    pub fn show(&self, icon: Option<&[Template]>, unread: u32) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let badge = super::badge(unread);
        if let Some(button) = self.status.button(mtm) {
            if let Some(image) = icon.and_then(image) {
                button.setImage(Some(&image));
            }
            button.setTitle(&NSString::from_str(badge.as_deref().unwrap_or_default()));
            button.setToolTip(Some(&NSString::from_str(&super::tool_tip(unread))));
        }
        // Kept while the app has no Dock icon, for when it has one again.
        let badge = badge.map(|badge| NSString::from_str(&badge));
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(badge.as_deref());
    }
}

/// A template image of [`POINTS`] from the icon at each of its sizes; only its alpha
/// counts. `None` if none was drawn, which keeps the image there was.
fn image(templates: &[Template]) -> Option<Retained<NSImage>> {
    let points = NSSize::new(POINTS.into(), POINTS.into());
    let image = NSImage::initWithSize(NSImage::alloc(), points);
    let mut drawn = false;
    for template in templates {
        let side = template.size as isize;
        if template.rgba.len() != (side * side * 4) as usize {
            continue;
        }
        // SAFETY: no planes, so the representation allocates its own buffer, described
        // by the rest: 8-bit RGBA, premultiplied, rows without padding.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                side,
                side,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                NSBitmapFormat::empty(),
                side * 4,
                32,
            )
        };
        let Some(rep) = rep else {
            continue;
        };
        let data = rep.bitmapData();
        if data.is_null() {
            continue;
        }
        // SAFETY: the buffer holds `side * side * 4` bytes, as asked for above, and so
        // does the template's.
        unsafe { std::ptr::copy_nonoverlapping(template.rgba.as_ptr(), data, template.rgba.len()) };
        rep.setSize(points);
        image.addRepresentation(&rep);
        drawn = true;
    }
    image.setTemplate(true);
    drawn.then_some(image)
}
