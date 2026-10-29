//! What a log names the build by: the commit, the target, the profile and the compiler.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CATMAIL_COMMIT");
    let profile = std::env::var("PROFILE").unwrap_or_default();
    // A package built from a tarball (or by Nix, which leaves .git out) has no repository
    // to ask: its build says which commit it is. Only release builds ask git: the commit
    // changes the engine's code, and every crate above it would build again after each
    // commit while developing.
    let commit = std::env::var("CATMAIL_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| (profile == "release").then(git_commit).flatten())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=CATMAIL_COMMIT={commit}");
    println!("cargo:rustc-env=CATMAIL_BUILD_PROFILE={profile}");
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=CATMAIL_BUILD_TARGET={target}");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let rustc = output(Command::new(rustc).arg("--version")).unwrap_or_default();
    println!("cargo:rustc-env=CATMAIL_BUILD_RUSTC={rustc}");
}

/// The commit checked out, with `-dirty` if tracked files differ from it; `None` outside
/// a repository or without git.
fn git_commit() -> Option<String> {
    // Without optional locks git doesn't refresh the index, which would run this again.
    let git = |args: &[&str]| output(Command::new("git").arg("--no-optional-locks").args(args));
    // Built again when HEAD moves or a branch is committed to, not on every edit:
    // `-dirty` can lag behind.
    let dirs = git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
    ])?;
    let (git_dir, common_dir) = dirs.split_once('\n')?;
    for path in [
        Path::new(git_dir).join("HEAD"),
        Path::new(common_dir).join("refs/heads"),
        Path::new(common_dir).join("packed-refs"),
    ] {
        // Cargo runs the script again each build for a path that doesn't exist, and
        // packed-refs only does once refs were packed (which changes refs/heads too).
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    // Tags left out: always the commit.
    git(&[
        "describe",
        "--always",
        "--dirty",
        "--abbrev=12",
        "--exclude=*",
    ])
}

/// What the command printed, trimmed, if it succeeded.
fn output(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}
