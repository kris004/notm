use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

use anyhow::{Context, ensure};

/// Every remote-control smoke uses its own bus, never the user's desktop bus.
pub struct PrivateBus {
    child: Child,
    pub address: String,
    _home: tempfile::TempDir,
}

impl PrivateBus {
    pub fn start() -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("XDG_STATE_HOME", home.path().join("state"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("starting required private session bus")?;
        let mut address = String::new();
        let read =
            BufReader::new(child.stdout.take().context("bus stdout")?).read_line(&mut address);
        if !matches!(read, Ok(count) if count > 0) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("private bus did not provide its address: {read:?}");
        }
        ensure!(
            address.starts_with("unix:"),
            "unexpected private bus address"
        );
        Ok(Self {
            child,
            address: address.trim().into(),
            _home: home,
        })
    }

    pub fn configure(&self, command: &mut Command) {
        command.env("DBUS_SESSION_BUS_ADDRESS", &self.address);
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
