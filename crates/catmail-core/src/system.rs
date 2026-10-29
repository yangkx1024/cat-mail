//! What the computer the engine runs on is, for the log: enough to tell one user's report
//! from another's (the system and its version, the desktop session, the locale), nothing
//! that names the user or the machine. Read from files where the system has them, so that
//! starting up runs no other program, except on Windows.

/// The system and its version, as its maker names it: "NixOS 26.05 (Zen)", "macOS 26.1
/// (25B78)", "Microsoft Windows [Version 10.0.26100.4652]".
// Windows returns first.
#[cfg_attr(windows, allow(unreachable_code))]
pub(crate) fn os() -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // A console program: without this a GUI app flashes a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new("cmd")
            .args(["/c", "ver"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return (!version.is_empty()).then_some(version);
    }
    if cfg!(target_os = "macos") {
        let plist =
            std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
        let version = plist_string(&plist, "ProductVersion")?;
        return Some(match plist_string(&plist, "ProductBuildVersion") {
            Some(build) => format!("macOS {version} ({build})"),
            None => format!("macOS {version}"),
        });
    }
    // The usual place, and where systemd says to look when it's missing.
    let release = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()?;
    os_release_name(&release)
}

/// The kernel's release, where it differs from the system's version (Linux; `None`
/// elsewhere, which has no such file).
pub(crate) fn kernel() -> Option<String> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    Some(release.trim().to_owned())
}

/// The graphical session (Wayland or X11) and the desktop: where the GUI's windows,
/// tray and notifications differ most.
pub(crate) fn session() -> Option<String> {
    if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
        return None;
    }
    let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
    match (var("XDG_SESSION_TYPE"), var("XDG_CURRENT_DESKTOP")) {
        (None, None) => None,
        (kind, desktop) => Some(format!(
            "{} {}",
            kind.as_deref().unwrap_or("unknown"),
            desktop.as_deref().unwrap_or("unknown")
        )),
    }
}

/// The locale messages are shown in, which decides the fonts and how dates read.
pub(crate) fn locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

/// `PRETTY_NAME` from an os-release file, or else `NAME` and `VERSION_ID`.
fn os_release_name(release: &str) -> Option<String> {
    let field = |key: &str| {
        release.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            (!value.is_empty()).then(|| value.to_owned())
        })
    };
    field("PRETTY_NAME").or_else(|| match (field("NAME"), field("VERSION_ID")) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (name, _) => name,
    })
}

/// The `<string>` after `<key>{key}</key>` in a property list.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = plist.split_once(&format!("<key>{key}</key>"))?.1;
    let value = after.trim_start().strip_prefix("<string>")?;
    Some(value.split_once("</string>")?.0.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_system_from_os_release() {
        let release = "NAME=NixOS\nID=nixos\nVERSION_ID=\"26.05\"\n\
                       PRETTY_NAME=\"NixOS 26.05 (Zen)\"\n";
        assert_eq!(
            os_release_name(release).as_deref(),
            Some("NixOS 26.05 (Zen)")
        );
        let bare = "NAME='Some Linux'\nVERSION_ID=3\n";
        assert_eq!(os_release_name(bare).as_deref(), Some("Some Linux 3"));
        assert_eq!(os_release_name("ID=x\n"), None);
    }

    #[test]
    fn reads_the_macos_version_from_its_property_list() {
        let plist = "<dict>\n\t<key>ProductBuildVersion</key>\n\t<string>25B78</string>\n\
                     \t<key>ProductVersion</key>\n\t<string>26.1</string>\n</dict>";
        assert_eq!(
            plist_string(plist, "ProductVersion").as_deref(),
            Some("26.1")
        );
        assert_eq!(
            plist_string(plist, "ProductBuildVersion").as_deref(),
            Some("25B78")
        );
        assert_eq!(plist_string(plist, "Missing"), None);
    }
}
