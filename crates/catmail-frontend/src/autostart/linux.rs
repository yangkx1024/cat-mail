//! Opening at login on Linux: an XDG autostart entry,
//! `$XDG_CONFIG_HOME/autostart/net.yangkx.catmail.desktop`, which the desktop (GNOME,
//! KDE Plasma, ...) starts at login with `--background` and lists in its autostart
//! settings. The entry is the setting: one the desktop turned off (`Hidden=true`,
//! `X-GNOME-Autostart-enabled=false`) reads as off, and turning it on writes it whole
//! again. It is the app's, whichever copy wrote it, as a login item is the bundle's on
//! macOS. The Flatpak's sandbox can't write there: the Background portal writes it
//! ([`portal`]).

use std::io;
use std::path::{Path, PathBuf};

use super::{Error, State, portal};

/// The entry's `Name`, as the desktop's autostart settings list it.
const NAME: &str = "catmail";

pub fn available() -> bool {
    if crate::in_flatpak() {
        return portal::available();
    }
    entry_path().is_some()
}

pub fn state() -> State {
    if crate::in_flatpak() {
        return portal::state();
    }
    entry_path().map_or(State::Off, |path| state_of(&path))
}

pub fn set(on: bool) -> (State, Result<(), Error>) {
    if crate::in_flatpak() {
        return portal::set(on);
    }
    let Some(path) = entry_path() else {
        let why = Error("There's no configuration folder to put the autostart entry in.".into());
        return (State::Off, Err(why));
    };
    let done = if on {
        std::env::current_exe().and_then(|exe| enable(&path, &exe))
    } else {
        crate::settings::remove_if_present(&path)
    };
    let done = done.map_err(|e| {
        tracing::warn!(on, error = %e, path = %path.display(), "couldn't change the autostart entry");
        Error(e.to_string())
    });
    (state_of(&path), done)
}

pub fn launched_at_login() -> bool {
    false
}

/// `$XDG_CONFIG_HOME/autostart/<app id>.desktop`, as the portal names it too.
fn entry_path() -> Option<PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    let name = format!("{}.desktop", crate::APP_ID);
    Some(dirs.config_dir().join("autostart").join(name))
}

/// On while the entry is there and the desktop's settings haven't turned it off.
fn state_of(path: &Path) -> State {
    match std::fs::read(path) {
        Ok(bytes) if enabled(&String::from_utf8_lossy(&bytes)) => State::On,
        Ok(_) => State::Off,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(error = %e, path = %path.display(), "couldn't read the autostart entry");
            }
            State::Off
        }
    }
}

/// Writes the entry that starts `exe` at login, over whatever is there: also one
/// another copy of the app wrote, or one the desktop's settings turned off.
fn enable(path: &Path, exe: &Path) -> io::Result<()> {
    crate::settings::write_atomically(path, entry(exe)?.as_bytes())
}

/// The entry that starts `exe` at login, opening no window. `TryExec` lets the desktop
/// skip it once `exe` is gone.
fn entry(exe: &Path) -> io::Result<String> {
    let exe = exe
        .to_str()
        .filter(|exe| !exe.contains(char::is_control))
        .ok_or_else(|| {
            let why = format!("an autostart entry can't start {}", exe.display());
            io::Error::new(io::ErrorKind::InvalidInput, why)
        })?;
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName={NAME}\nExec={} --background\nTryExec={}\n\
         Icon={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        string(&quoted(exe)),
        string(exe),
        crate::APP_ID,
    ))
}

/// One argument of `Exec`, quoted: `"`, `` ` ``, `$` and `\` escaped inside the quotes,
/// `%` doubled (a field code otherwise).
fn quoted(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A string value, its backslashes escaped as every value's are (so `Exec`'s are
/// doubled again).
fn string(value: &str) -> String {
    value.replace('\\', "\\\\")
}

/// Whether the desktop starts entry `text`: neither `Hidden` (the entry removed by the
/// user, in the specification) nor GNOME's `X-GNOME-Autostart-enabled=false` (turned
/// off in its settings). A key given twice counts as its last, as GLib reads it.
fn enabled(text: &str) -> bool {
    let mut group = "";
    let (mut hidden, mut on) = (false, true);
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name;
        } else if group == "Desktop Entry"
            && let Some((key, value)) = line.split_once('=')
        {
            match key.trim_end() {
                "Hidden" => hidden = value.trim_start() == "true",
                "X-GNOME-Autostart-enabled" => on = value.trim_start() != "false",
                _ => {}
            }
        }
    }
    !hidden && on
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_quotes_the_program_as_the_specification_asks() {
        let text = entry(Path::new("/opt/with space/catmail-gui")).unwrap();
        assert_eq!(
            text,
            format!(
                "[Desktop Entry]\nType=Application\nName=catmail\n\
                 Exec=\"/opt/with space/catmail-gui\" --background\n\
                 TryExec=/opt/with space/catmail-gui\nIcon={}\nTerminal=false\n\
                 X-GNOME-Autostart-enabled=true\n",
                crate::APP_ID
            )
        );
        // Quoted, then escaped as a string: a backslash is four.
        let text = entry(Path::new(r#"/a "b" $c 100%/d\e"#)).unwrap();
        assert!(
            text.contains(r#"Exec="/a \\"b\\" \\$c 100%%/d\\\\e" --background"#),
            "{text}"
        );
        assert!(text.contains(r#"TryExec=/a "b" $c 100%/d\\e"#), "{text}");
        assert!(entry(Path::new("/a\nb")).is_err());
    }

    #[test]
    fn the_entry_is_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.desktop", crate::APP_ID));
        let exe = Path::new("/opt/catmail/catmail-gui");
        assert_eq!(state_of(&path), State::Off);
        enable(&path, exe).unwrap();
        assert_eq!(state_of(&path), State::On);

        // Turned off in the desktop's settings; a key of another group is no setting.
        let ours = std::fs::read_to_string(&path).unwrap();
        for off in [
            "Hidden=true",
            "X-GNOME-Autostart-enabled=false",
            "X-GNOME-Autostart-enabled = false",
        ] {
            std::fs::write(&path, format!("{ours}{off}\n")).unwrap();
            assert_eq!(state_of(&path), State::Off, "{off}");
        }
        std::fs::write(&path, format!("{ours}[Desktop Action x]\nHidden=true\n")).unwrap();
        assert_eq!(state_of(&path), State::On);

        // Turning it on again writes it whole; another copy's is written over.
        std::fs::write(&path, format!("{ours}Hidden=true\n")).unwrap();
        enable(&path, exe).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ours);
        enable(&path, Path::new("/elsewhere/catmail-gui")).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("/elsewhere/")
        );

        crate::settings::remove_if_present(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(state_of(&path), State::Off);
    }
}
