//! What the Flatpak portal's update monitor says (the commits, an installation's
//! progress), made into the [`Update`] to show.

use super::{Failure, Update};

/// The commits the monitor reports: the one running, the one installed (what a restart
/// runs), and the repository's (what installing deploys).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Commits {
    pub running: String,
    pub local: String,
    pub remote: String,
}

/// How an installation is going, as the portal reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Progress {
    /// Operation `op` of `n_ops` (from 0), `percent` done; no numbers before the first.
    Running {
        op: u32,
        n_ops: u32,
        percent: u32,
    },
    /// Nothing to install: what's installed is the repository's.
    Empty,
    Done,
    /// The error's D-Bus name and message.
    Failed {
        error: String,
        message: String,
    },
}

/// The D-Bus errors the portal fails an installation with, that say where else it can
/// be done: it can't (more permissions, or no Access portal), it couldn't ask the user
/// (its Access portal isn't there to call), the user said no.
const NOT_SUPPORTED: &str = "org.freedesktop.DBus.Error.NotSupported";
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";
pub(super) const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// What the monitor has said, made into the [`Update`] to show.
#[derive(Default)]
pub(super) struct Tracker {
    commits: Commits,
    installing: bool,
}

impl Tracker {
    /// The monitor's news of the commits.
    pub fn available(&mut self, commits: Commits) -> Option<Update> {
        self.commits = commits;
        if self.installing {
            // The installation's progress says how it ends.
            return Some(Update::Installing(None));
        }
        self.settled()
    }

    /// Installing was asked for.
    pub fn install(&mut self) -> Update {
        self.installing = true;
        Update::Installing(None)
    }

    /// Asking to install failed: the portal never started.
    pub fn not_installing(&mut self, why: String) -> Update {
        self.installing = false;
        Update::Failed(Failure::Other(why))
    }

    /// The portal left the bus: an installation under way went with it.
    pub fn portal_left(&mut self) -> Option<Update> {
        self.installing
            .then(|| self.not_installing("the Flatpak portal stopped".into()))
    }

    pub fn progress(&mut self, progress: Progress) -> Option<Update> {
        self.installing = matches!(progress, Progress::Running { .. });
        match progress {
            Progress::Running { op, n_ops, percent } => {
                Some(Update::Installing(overall(op, n_ops, percent)))
            }
            Progress::Empty => {
                // The repository's is installed already (by the software center, since the
                // last check): what runs may be older still.
                self.commits.local.clone_from(&self.commits.remote);
                self.settled()
            }
            Progress::Done => {
                self.commits.local.clone_from(&self.commits.remote);
                Some(Update::Installed)
            }
            Progress::Failed { error, message } => Some(Update::Failed(match error.as_str() {
                NOT_SUPPORTED | SERVICE_UNKNOWN => Failure::Elsewhere,
                ACCESS_DENIED => Failure::NotAllowed,
                _ => Failure::Other(plain(&error, &message)),
            })),
        }
    }

    /// What the commits say, with no installation under way.
    fn settled(&self) -> Option<Update> {
        let Commits {
            running,
            local,
            remote,
        } = &self.commits;
        if remote != local {
            Some(Update::Available)
        } else if local != running {
            Some(Update::Installed)
        } else {
            None
        }
    }
}

/// The portal's `message` for the error `error`, without the `GDBus.Error:<error>: ` GLib
/// puts in front of one that came over D-Bus; the error itself if there's no message.
fn plain(error: &str, message: &str) -> String {
    let message = message
        .strip_prefix("GDBus.Error:")
        .and_then(|rest| rest.strip_prefix(error))
        .and_then(|rest| rest.strip_prefix(": "))
        .unwrap_or(message);
    if message.is_empty() { error } else { message }.to_owned()
}

/// How far an installation of `n_ops` operations is, in percent, in operation `op` (from
/// 0) `percent` done. `None` before the portal says.
fn overall(op: u32, n_ops: u32, percent: u32) -> Option<u8> {
    if n_ops == 0 {
        return None;
    }
    let done = (u64::from(op.min(n_ops)) * 100 + u64::from(percent.min(100))) / u64::from(n_ops);
    u8::try_from(done.min(100)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commits(running: &str, local: &str, remote: &str) -> Commits {
        Commits {
            running: running.into(),
            local: local.into(),
            remote: remote.into(),
        }
    }

    #[test]
    fn a_newer_commit_is_an_update_to_install_or_to_restart_into() {
        let mut tracker = Tracker::default();
        assert_eq!(
            tracker.available(commits("a", "a", "b")),
            Some(Update::Available)
        );
        // The software center installed it meanwhile.
        assert_eq!(
            tracker.available(commits("a", "b", "b")),
            Some(Update::Installed)
        );
        // Installed, and the repository has a newer one still: installing it comes first.
        assert_eq!(
            tracker.available(commits("a", "b", "c")),
            Some(Update::Available)
        );
        // Back to what runs (the update was rolled back).
        assert_eq!(tracker.available(commits("a", "a", "a")), None);
    }

    #[test]
    fn installing_ends_installed_or_failed() {
        let mut tracker = Tracker::default();
        tracker.available(commits("a", "a", "b"));
        assert_eq!(tracker.install(), Update::Installing(None));
        let running = |op, n_ops, percent| Progress::Running { op, n_ops, percent };
        assert_eq!(
            tracker.progress(running(0, 0, 0)),
            Some(Update::Installing(None))
        );
        assert_eq!(
            tracker.progress(running(1, 2, 50)),
            Some(Update::Installing(Some(75)))
        );
        // A check while it installs doesn't take it back to available.
        assert_eq!(
            tracker.available(commits("a", "a", "b")),
            Some(Update::Installing(None))
        );
        assert_eq!(tracker.progress(Progress::Done), Some(Update::Installed));
        // The monitor's next check agrees.
        assert_eq!(
            tracker.available(commits("a", "b", "b")),
            Some(Update::Installed)
        );

        let mut tracker = Tracker::default();
        tracker.available(commits("a", "a", "b"));
        tracker.install();
        let failed = |error: &str, message: &str| Progress::Failed {
            error: error.into(),
            message: message.into(),
        };
        let permissions = failed(NOT_SUPPORTED, "new version requires new permissions");
        assert_eq!(
            tracker.progress(permissions),
            Some(Update::Failed(Failure::Elsewhere))
        );
        assert_eq!(
            tracker.progress(failed(ACCESS_DENIED, "not allowed")),
            Some(Update::Failed(Failure::NotAllowed))
        );
        let other = tracker.progress(failed("org.freedesktop.DBus.Error.Failed", "no network"));
        assert_eq!(
            other,
            Some(Update::Failed(Failure::Other("no network".into())))
        );
        // The portal couldn't show its dialog.
        let no_dialog = failed(
            SERVICE_UNKNOWN,
            "GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: The name is not activatable",
        );
        assert_eq!(
            tracker.progress(no_dialog),
            Some(Update::Failed(Failure::Elsewhere))
        );
        // Not installing any more: the next check says what there is.
        assert_eq!(
            tracker.available(commits("a", "a", "b")),
            Some(Update::Available)
        );
    }

    #[test]
    fn nothing_to_install_is_what_the_commits_say() {
        let mut tracker = Tracker::default();
        tracker.available(commits("a", "a", "b"));
        tracker.install();
        // The software center had installed it just before.
        assert_eq!(tracker.progress(Progress::Empty), Some(Update::Installed));
    }

    #[test]
    fn the_overall_progress_spans_the_operations() {
        assert_eq!(overall(0, 0, 0), None);
        assert_eq!(overall(0, 1, 40), Some(40));
        assert_eq!(overall(0, 2, 100), Some(50));
        assert_eq!(overall(1, 2, 0), Some(50));
        // Out of range, as no portal should say.
        assert_eq!(overall(5, 2, 300), Some(100));
    }

    #[test]
    fn a_portal_that_leaves_ends_an_installation_only() {
        let mut tracker = Tracker::default();
        tracker.available(commits("a", "a", "b"));
        assert_eq!(tracker.portal_left(), None);
        tracker.install();
        assert!(matches!(
            tracker.portal_left(),
            Some(Update::Failed(Failure::Other(_)))
        ));
        assert_eq!(tracker.portal_left(), None);
    }

    #[test]
    fn a_message_over_dbus_is_told_without_its_error_name() {
        let error = "org.freedesktop.DBus.Error.Failed";
        let over_dbus = format!("GDBus.Error:{error}: Remote not found");
        assert_eq!(plain(error, &over_dbus), "Remote not found");
        assert_eq!(plain(error, "No network"), "No network");
        assert_eq!(plain(error, ""), error);
    }
}
