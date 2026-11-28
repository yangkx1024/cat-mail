//! A session bus of a test's own, for the portals' tests to serve a fake portal on.

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};

/// A `dbus-daemon` of the test's own, which starts nothing ([`Bus::start`]), or only one
/// service ([`Bus::serving`]).
pub struct Bus {
    daemon: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Bus {
    pub fn start() -> Self {
        Self::starting(None)
    }

    /// A bus that starts the service `name` as this test binary running `test`, a test
    /// in the module `module` (its `module_path!()`), ignored so that only the bus runs
    /// it: as the bus starts xdg-desktop-portal when nobody runs it yet.
    pub fn serving(name: &str, module: &str, test: &str) -> Self {
        // As libtest names it: the module's path without the crate's name.
        let (_, tests) = module.split_once("::").unwrap();
        Self::starting(Some((name, &format!("{tests}::{test}"))))
    }

    fn starting(service: Option<(&str, &str)>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let services = dir.path().join("services");
        std::fs::create_dir(&services).unwrap();
        if let Some((name, test)) = service {
            let exe = std::env::current_exe().unwrap();
            let service = format!(
                "[D-BUS Service]\nName={name}\nExec={} --exact {test} --ignored \
                 --test-threads=1\n",
                exe.display()
            );
            std::fs::write(services.join(format!("{name}.service")), service).unwrap();
        }
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<busconfig><type>session</type><listen>unix:dir={}</listen>\
                 <auth>EXTERNAL</auth><servicedir>{}</servicedir>\
                 <policy context=\"default\">\
                 <allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                dir.path().display(),
                services.display()
            ),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon");
        let mut output = BufReader::new(daemon.stdout.take().unwrap());
        let mut address = String::new();
        output.read_line(&mut address).unwrap();
        let address = address.trim().to_owned();
        // What it starts prints there too, also after the test: read to its end.
        std::thread::spawn(move || std::io::copy(&mut output, &mut std::io::sink()));
        Self {
            daemon,
            address,
            _dir: dir,
        }
    }

    pub fn connection(&self) -> zbus::blocking::connection::Builder<'_> {
        zbus::blocking::connection::Builder::address(self.address.as_str()).unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}
