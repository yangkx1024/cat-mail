//! The packaging files name the app by [`APP_ID`] too, and can't refer to the constant:
//! the Flatpak's id, the `.desktop` file's name, the icon and window class it gives, the
//! AppStream id, the macOS bundle id. Checked here, so a new id can't leave one behind.

use std::fs;
use std::path::PathBuf;

use catmail_frontend::APP_ID;

/// Asserts that the repository's file at `path` says each of `needles`.
fn says(path: &str, needles: &[String]) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    // `\n` line ends, whichever the checkout gave it.
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n");
    for needle in needles {
        assert!(
            text.contains(needle.as_str()),
            "{} doesn't say {needle:?}",
            path.display()
        );
    }
}

#[test]
fn the_desktop_file_is_named_and_names_the_app_by_its_id() {
    says(
        &format!("packaging/linux/{APP_ID}.desktop"),
        &[
            format!("\nIcon={APP_ID}\n"),
            format!("\nStartupWMClass={APP_ID}\n"),
        ],
    );
}

#[test]
fn the_flatpak_manifest_is_named_by_the_id_and_installs_under_it() {
    says(
        &format!("packaging/linux/{APP_ID}.yml"),
        &[
            format!("id: {APP_ID}\n"),
            format!("packaging/linux/{APP_ID}.desktop"),
            format!("packaging/linux/{APP_ID}.appdata.xml"),
            format!("/app/share/metainfo/{APP_ID}.metainfo.xml"),
            format!("/app/share/icons/hicolor/scalable/apps/{APP_ID}.svg"),
        ],
    );
}

#[test]
fn the_appstream_metadata_has_the_id() {
    let (developer, _) = APP_ID.rsplit_once('.').unwrap();
    says(
        &format!("packaging/linux/{APP_ID}.appdata.xml"),
        &[
            format!("<id>{APP_ID}</id>"),
            format!(r#"<launchable type="desktop-id">{APP_ID}.desktop</launchable>"#),
            format!(r#"<developer id="{developer}">"#),
        ],
    );
}

#[test]
fn the_macos_bundle_id_is_the_id() {
    says(
        "packaging/macos/Info.plist",
        &[format!(
            "<key>CFBundleIdentifier</key>\n\t<string>{APP_ID}</string>"
        )],
    );
}
