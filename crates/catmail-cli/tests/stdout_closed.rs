//! Output to a reader that stopped reading, as `catmail search x | head` has once
//! `head` has its lines.

use std::io;
use std::process::{Command, Output, Stdio};

/// Runs `catmail` on an empty data directory with `args`, its stdout a pipe whose
/// reading end is already closed, and its stderr `stderr`.
fn run_into_closed_pipe(args: &[&str], stderr: Stdio) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    Command::new(env!("CARGO_BIN_EXE_catmail"))
        .arg("--data-dir")
        .arg(dir.path().join("data"))
        .arg("--config-dir")
        .arg(dir.path().join("config"))
        .args(args)
        .stdout(writer)
        .stderr(stderr)
        .output()
        .unwrap()
}

#[test]
fn closed_stdout_exits_quietly_with_141() {
    for args in [&["ops"][..], &["--json", "ops"], &["account", "list"]] {
        let output = run_into_closed_pipe(args, Stdio::piped());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(141), "{args:?}: {stderr}");
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        assert!(!stderr.contains("Error"), "{args:?}: {stderr}");
    }
}

#[test]
fn a_failure_is_no_panic_when_stderr_is_closed_too() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    // No account to list drafts of.
    let output = run_into_closed_pipe(&["draft", "list"], writer.into());
    assert_eq!(output.status.code(), Some(1));
}
