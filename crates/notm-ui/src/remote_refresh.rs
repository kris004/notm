//! Production, non-activating session-bus search refresh protocol.
//!
//! No GTK initialization, configuration loading, test-harness socket, or mail
//! access belongs in this client. Always address pinned unique bus owners.

use std::{
    collections::BTreeSet,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};
use gtk4::{
    gio,
    glib::{self, variant::ToVariant},
};
use serde::Serialize;

pub(crate) const NORMAL_APPLICATION_ID: &str = "io.github.kris004.notm";
pub(crate) const TEST_APPLICATION_NAMESPACE: &str = "io.github.kris004.notm.test.";
pub(crate) const INTERFACE: &str = "io.github.kris004.notm.SearchRefresh1";
pub(crate) const OBJECT_PATH: &str = "/io/github/kris004/notm/SearchRefresh";
pub(crate) const INTERFACE_XML: &str = r#"
<node>
  <interface name="io.github.kris004.notm.SearchRefresh1">
    <method name="Refresh">
      <arg name="timeout_ms" type="u" direction="in"/>
      <arg name="generation" type="t" direction="out"/>
    </method>
  </interface>
</node>
"#;
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const MAX_INSTANCES: usize = 64;

#[derive(Debug, Default, Serialize)]
pub struct RefreshReport {
    pub refreshed: usize,
    pub failed: Vec<RefreshFailure>,
}

#[derive(Debug, Serialize)]
pub struct RefreshFailure {
    pub instance: String,
    pub error: String,
}

/// Wait for a completed refresh in every discovered, same-user instance.
/// The outer deadline also bounds D-Bus connection establishment. A timed-out
/// read-only request may still complete remotely; it must not be retried as if
/// the first request were known not to have been delivered.
pub fn refresh_running(test_instances: bool, timeout: Duration) -> anyhow::Result<RefreshReport> {
    ensure!(
        (Duration::from_secs(1)..=Duration::from_secs(300)).contains(&timeout),
        "refresh timeout must be between 1 and 300 seconds"
    );
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("refresh-client".into())
        .spawn(move || {
            let _ = tx.send(refresh_on_bus(test_instances, deadline));
        })?;
    rx.recv_timeout(timeout)
        .context("refresh deadline expired; an already-dispatched search may still complete")?
}

fn session_address() -> anyhow::Result<String> {
    if let Some(address) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        let address = address
            .into_string()
            .map_err(|_| anyhow::anyhow!("DBUS_SESSION_BUS_ADDRESS must be valid UTF-8"))?;
        ensure!(!address.is_empty(), "DBUS_SESSION_BUS_ADDRESS is empty");
        // In particular, do not permit GLib's autolaunch transport.
        ensure!(
            address.split(';').all(|entry| entry.starts_with("unix:")),
            "refresh requires an existing local Unix session bus"
        );
        return Ok(address);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context(
        "refresh needs DBUS_SESSION_BUS_ADDRESS or XDG_RUNTIME_DIR from the user session",
    )?;
    let path = std::path::PathBuf::from(runtime).join("bus");
    ensure!(path.is_absolute(), "XDG_RUNTIME_DIR must be absolute");
    let path = path.to_str().context("session bus path must be UTF-8")?;
    Ok(format!(
        "unix:path={}",
        glib::uri_escape_string(path, None::<&str>, false)
    ))
}

fn remaining_ms(deadline: Instant) -> anyhow::Result<i32> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    ensure!(!remaining.is_zero(), "refresh deadline expired");
    Ok(remaining.as_millis().clamp(1, i32::MAX as u128) as i32)
}

fn bus_call(
    connection: &gio::DBusConnection,
    method: &str,
    parameters: Option<&glib::Variant>,
    deadline: Instant,
) -> anyhow::Result<glib::Variant> {
    Ok(connection.call_sync(
        Some(BUS),
        BUS_PATH,
        BUS,
        method,
        parameters,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        remaining_ms(deadline)?,
        gio::Cancellable::NONE,
    )?)
}

fn is_target(name: &str, test_instances: bool) -> bool {
    if test_instances {
        name.starts_with(TEST_APPLICATION_NAMESPACE)
    } else {
        name == NORMAL_APPLICATION_ID
    }
}

fn refresh_on_bus(test_instances: bool, deadline: Instant) -> anyhow::Result<RefreshReport> {
    let connection = gio::DBusConnection::for_address_sync(
        &session_address()?,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .context("connecting to the existing user session bus (no bus or GUI will be launched)")?;
    // A disappearing session bus is an error, not a reason to abort the CLI.
    connection.set_exit_on_close(false);
    let names = bus_call(&connection, "ListNames", None, deadline)?
        .get::<(Vec<String>,)>()
        .context("invalid session-bus name list")?
        .0;
    let names: Vec<_> = names
        .into_iter()
        .filter(|name| is_target(name, test_instances))
        .collect();
    ensure!(
        names.len() <= MAX_INSTANCES,
        "too many refresh targets (maximum {MAX_INSTANCES})"
    );

    let mut report = RefreshReport::default();
    let mut owners = BTreeSet::new();
    let (tx, rx) = mpsc::channel();
    let mut outstanding = BTreeSet::new();
    for name in names {
        let resolved = (|| -> anyhow::Result<String> {
            let owner = bus_call(
                &connection,
                "GetNameOwner",
                Some(&(name.as_str(),).to_variant()),
                deadline,
            )?
            .get::<(String,)>()
            .context("invalid session-bus owner")?
            .0;
            let uid = bus_call(
                &connection,
                "GetConnectionUnixUser",
                Some(&(owner.as_str(),).to_variant()),
                deadline,
            )?
            .get::<(u32,)>()
            .context("invalid session-bus user")?
            .0;
            // SAFETY: getuid has no arguments or preconditions.
            ensure!(
                uid == unsafe { libc::getuid() },
                "instance belongs to another user"
            );
            Ok(owner)
        })();
        let owner = match resolved {
            Ok(owner) => owner,
            Err(error) => {
                report.failed.push(RefreshFailure {
                    instance: name,
                    error: error.to_string(),
                });
                continue;
            }
        };
        if !owners.insert(owner.clone()) {
            continue;
        }
        let connection = connection.clone();
        let tx = tx.clone();
        outstanding.insert(name.clone());
        // Dispatch independently: an old or stalled peer must not prevent the
        // other peers from receiving their request.
        thread::Builder::new()
            .name("refresh-peer".into())
            .spawn(move || {
                let result = refresh_peer(&connection, &owner, deadline);
                let _ = tx.send((name, result));
            })?;
    }
    drop(tx);
    while !outstanding.is_empty() {
        let Ok((name, result)) =
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        else {
            for name in outstanding {
                report.failed.push(RefreshFailure {
                    instance: name,
                    error: "refresh deadline expired; dispatched search may still complete".into(),
                });
            }
            break;
        };
        outstanding.remove(&name);
        match result {
            Ok(()) => report.refreshed += 1,
            Err(error) => report.failed.push(RefreshFailure {
                instance: name,
                error: error.to_string(),
            }),
        }
    }
    report.failed.sort_by(|a, b| a.instance.cmp(&b.instance));
    Ok(report)
}

fn refresh_peer(
    connection: &gio::DBusConnection,
    owner: &str,
    deadline: Instant,
) -> anyhow::Result<()> {
    let timeout_ms = remaining_ms(deadline)?;
    let response = connection.call_sync(
        Some(owner),
        OBJECT_PATH,
        INTERFACE,
        "Refresh",
        Some(&(timeout_ms as u32,).to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        timeout_ms,
        gio::Cancellable::NONE,
    ).map_err(|error| {
        if error.matches(gio::DBusError::UnknownMethod)
            || error.matches(gio::DBusError::UnknownInterface)
            || error.matches(gio::DBusError::UnknownObject)
        {
            anyhow::anyhow!("running notm does not support search refresh; relaunch it with the updated binary when convenient")
        } else {
            anyhow::anyhow!("{error}")
        }
    })?;
    ensure!(
        response.get::<(u64,)>().is_some(),
        "invalid search-refresh reply"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_and_test_discovery_are_disjoint() {
        assert!(is_target(NORMAL_APPLICATION_ID, false));
        assert!(!is_target(NORMAL_APPLICATION_ID, true));
        assert!(is_target("io.github.kris004.notm.test.t123", true));
        assert!(!is_target("io.github.kris004.notm.test.t123", false));
        for name in [
            ":1.23",
            "io.github.kris004.notm.other",
            "io.github.kris004.notm2",
        ] {
            assert!(!is_target(name, false));
            assert!(!is_target(name, true));
        }
    }

    #[test]
    fn deadline_never_uses_unbounded_dbus_timeout() {
        assert!(remaining_ms(Instant::now()).is_err());
        assert!(remaining_ms(Instant::now() + Duration::from_millis(1)).unwrap() > 0);
    }

    #[test]
    fn protocol_is_valid() {
        let node = gio::DBusNodeInfo::for_xml(INTERFACE_XML).unwrap();
        assert!(node.lookup_interface(INTERFACE).is_some());
    }
}
