//! The packaging files name the app by [`APP_ID`] too, and can't refer to the constant:
//! the Flatpak's id, the `.desktop` file's name, the icon and window class it gives, the
//! AppStream id, the macOS bundle id. Checked here, so a new id can't leave one behind.

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use catmail_frontend::APP_ID;

fn repo_file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// `\n` line ends, whichever the checkout gave it.
fn text(path: &str) -> String {
    let path = repo_file(path);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// The value of a `key: value` or `key=value` line.
fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .unwrap_or_else(|| panic!("no {key:?} line"))
}

fn says(path: &str, needles: &[String]) {
    let text = text(path);
    for needle in needles {
        assert!(
            text.contains(needle.as_str()),
            "{path} doesn't say {needle:?}"
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

/// The files that add catmail's Flatpak repository (`.flatpakrepo`) or install the app
/// from it (`.flatpakref`), and the bundles, which update from it: one address, one key
/// (the one update-flatpak-repo.sh signs with), the manifest's branch.
#[test]
fn the_flatpak_repository_files_agree_with_the_manifest_and_the_key() {
    let manifest = text(&format!("packaging/linux/{APP_ID}.yml"));
    let branch = value(&manifest, "branch: ");
    let key = STANDARD.encode(fs::read(repo_file("packaging/linux/catmail-flatpak.gpg")).unwrap());
    let repo = text(&format!("packaging/linux/{APP_ID}.flatpakrepo"));
    let reference = text(&format!("packaging/linux/{APP_ID}.flatpakref"));

    assert_eq!(value(&reference, "Name="), APP_ID);
    assert_eq!(value(&reference, "Branch="), branch);
    assert_eq!(value(&repo, "DefaultBranch="), branch);
    assert_eq!(value(&reference, "Url="), value(&repo, "Url="));
    assert_eq!(value(&repo, "GPGKey="), key);
    assert_eq!(value(&reference, "GPGKey="), key);
}
