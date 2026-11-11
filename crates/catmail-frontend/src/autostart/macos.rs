//! The app's login item (`SMAppService.mainApp`), and its launch's Apple Event.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSBundle};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use super::{Error, State};

/// Four characters as Apple Events spell their codes.
const fn code(chars: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*chars)
}

/// `kAEOpenApplication`.
const OPEN_APPLICATION: u32 = code(b"oapp");
/// `keyAEPropData`, the parameter that says why it was opened.
const PROP_DATA: u32 = code(b"prdt");
/// `keyAELaunchedAsLogInItem`.
const LAUNCHED_AS_LOGIN_ITEM: u32 = code(b"lgit");
/// `keySenderPIDAttr`.
const SENDER_PID: u32 = code(b"spid");

pub fn available() -> bool {
    // macOS 13's: the deployment target is 12, where looking the class up would panic.
    AnyClass::get(c"SMAppService").is_some() && NSBundle::mainBundle().bundleIdentifier().is_some()
}

fn service() -> Option<Retained<SMAppService>> {
    // SAFETY: the class exists (`available`); the main bundle's service, made fresh.
    available().then(|| unsafe { SMAppService::mainAppService() })
}

fn state_of(service: &SMAppService) -> State {
    // SAFETY: a property read, from any thread.
    match unsafe { service.status() } {
        SMAppServiceStatus::Enabled => State::On,
        SMAppServiceStatus::RequiresApproval => State::NeedsApproval,
        _ => State::Off,
    }
}

pub fn state() -> State {
    service().map_or(State::Off, |service| state_of(&service))
}

pub fn set(on: bool) -> (State, Result<(), Error>) {
    let Some(service) = service() else {
        return (State::Off, Err(Error("This app has no login item.".into())));
    };
    // SAFETY: the service is the main app's, valid for as long as it is held.
    let done = unsafe {
        if on {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    let state = state_of(&service);
    if state == State::NeedsApproval {
        // SAFETY: the class exists (`service`); it opens System Settings.
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
    let done = match done {
        Ok(()) => Ok(()),
        // Already as asked: registered or unregistered twice is an error to the system.
        Err(_) if state == if on { State::On } else { State::Off } => Ok(()),
        Err(e) => {
            let why = e.localizedDescription().to_string();
            tracing::warn!(on, error = why, "couldn't change the login item");
            Err(Error(why))
        }
    };
    (state, done)
}

/// Whether this launch was the login's. Legacy login items were opened marked as such.
/// One of `SMAppService.mainApp` is reported to be started by launchd, opened by
/// nobody: so an unopened launch while the login item is on is taken for the login's.
/// A binary started from a shell is unopened too: it opens without a window while the
/// login item is on, and another launch shows it.
pub fn launched_at_login() -> bool {
    let opened = opened_as_login_item();
    let at_login = opened.unwrap_or_else(|| state() == State::On);
    // Said in the log, for telling a login's launch from the others on a real login.
    tracing::info!(?opened, at_login, "launched");
    at_login
}

/// Whether another process opened the app (Finder, the Dock, `open`, the login), and
/// if so whether as a login item (an "open application" with `keyAELaunchedAsLogInItem`):
/// `None` if nobody did. AppKit then sends itself an empty "open application" (a shell
/// or launchd started it), so the sender tells.
fn opened_as_login_item() -> Option<bool> {
    let event = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent()?;
    type Descriptor = Option<Retained<NSAppleEventDescriptor>>;
    // SAFETY: the methods take and return what is declared (objc2 has them behind
    // CoreServices' types, which are these integers).
    let (id, why, sender): (u32, Descriptor, Descriptor) = unsafe {
        (
            msg_send![&*event, eventID],
            msg_send![&*event, paramDescriptorForKeyword: PROP_DATA],
            msg_send![&*event, attributeDescriptorForKeyword: SENDER_PID],
        )
    };
    let opening = id == OPEN_APPLICATION;
    let as_login_item = why.is_some_and(|why| why.enumCodeValue() == LAUNCHED_AS_LOGIN_ITEM);
    let own = sender.is_some_and(|pid| u32::try_from(pid.int32Value()) == Ok(std::process::id()));
    if opening && own && !as_login_item {
        None
    } else {
        Some(opening && as_login_item)
    }
}
